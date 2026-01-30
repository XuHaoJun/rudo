# rudo-gc + Tokio Async/Await 整合計劃 v3

**建立日期**: 2026-01-30
**作者**: opencode
**版本**: 3.0
**基於**: v2 技術審查 + tokio-rs 程式碼庫分析

## 執行摘要

本計劃描述如何將 rudo-gc 與 tokio async/await 系統整合，採用 drop guard 模式和程序級根追蹤。

### 設計決策確認

| 決策 | 選擇 | 理由 |
|------|------|------|
| `#[gc::root]` 模式 | Drop guard (基於作用域) | 簡單實作，進入作用域時擷取所有內容 |
| 多執行期支援 | 程序級 `GcRootSet` | 單一 GC，多個執行期；根是程序級的 |
| GC 通知 | 標記為髒，下個週期收集 | 避免優先權反轉，遵循 GC 最佳實踐 |

---

## 架構概覽

```
┌─────────────────────────────────────────────────────────────────────┐
│                        User Application                              │
├─────────────────────────────────────────────────────────────────────┤
│  #[gc::main]                                                         │
│  async fn main() {                                                   │
│      let gc = Gc::new(Data { value: 42 });                          │
│      gc::spawn(async move {                                         │
│          // gc 自動受 #[gc::root] 保護                              │
│      }).await;                                                       │
│  }                                                                   │
├─────────────────────────────────────────────────────────────────────┤
│                      rudo-gc-tokio Layer                             │
├─────────────────────────────────────────────────────────────────────┤
│  #[gc::main]      gc::spawn()      #[gc::root]      Gc::yield_now()│
│  (macro)          (auto-root)      (drop guard)      (safepoint)    │
├─────────────────────────────────────────────────────────────────────┤
│                      GcRootSet (程序級)                              │
├─────────────────────────────────────────────────────────────────────┤
│  roots: Mutex<Vec<usize>>     dirty_flag: AtomicBool                │
│  count: AtomicUsize                                                   │
├─────────────────────────────────────────────────────────────────────┤
│                        rudo-gc Core                                  │
├─────────────────────────────────────────────────────────────────────┤
│  LocalHeap    ThreadRegistry    Gc (Send+Sync)    GcDriver          │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 技術洞察 (來自 tokio-rs 分析)

### tokio.TaskTracker 模式

| 模式 | tokio 實作 | rudo-gc 應用 |
|------|-----------|-------------|
| 任務追蹤 | `TaskTracker` 使用 `Arc<AtomicUsize>` + `Notify` | 使用類似模式於 `GcRootGuard` 計數 |
| 巨集模式 | `#[tokio::main]` 轉換 async fn | 建立 `#[gc::main]` 和 `#[gc::test]` |
| 釘選生成 | `spawn_pinned` 使用 oneshot + channel | 鏡像用於根守護生命週期 |
| 本地任務 | `LocalSet` + `spawn_local` | 支援 `!Send` Gc 類型 |

### 關鍵認知 (v2 → v3 修正)

| v2 假設 | v3 事實 | 影響 |
|---------|---------|------|
| 僅手動根追蹤 | 可參照 tokio-macros 實作 proc-macro | v3 提升至 Phase 3 |
| 無 GC 喚醒機制 | 添加 `dirty` 標記至 `GcRootSet` | 根變更時標記為髒 |
| 無巨集支援 | `#[gc::main]` 和 `#[gc::test]` | 更好的開發體驗 |

---

## 實作階段

### Phase 1: 基礎設施 (Week 1) - 已完成

#### 1.1 Cargo.toml

```toml
# crates/rudo-gc/Cargo.toml
[features]
default = ["derive"]
derive = ["dep:rudo-gc-derive"]
tokio = ["dep:tokio", "dep:tokio-util"]

[dependencies]
tokio = { version = "1.0", optional = true, default-features = false, features = ["rt"] }
tokio-util = { version = "0.7", optional = true, features = ["rt"] }
```

#### 1.2 GcRootSet (程序級根集合)

**`crates/rudo-gc/src/tokio/root.rs`**

```rust
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

pub struct GcRootSet {
    roots: Mutex<Vec<usize>>,
    count: AtomicUsize,
    dirty: AtomicBool,
}

impl GcRootSet {
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<GcRootSet> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    const fn new() -> Self {
        Self {
            roots: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn register(&self, ptr: usize) {
        let mut roots = self.roots.lock().unwrap();
        if !roots.contains(&ptr) {
            roots.push(ptr);
        }
        drop(roots);
        self.count.fetch_add(1, Ordering::AcqRel);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn unregister(&self, ptr: usize) {
        self.roots.lock().unwrap().retain(|&p| p != ptr);
        self.count.fetch_sub(1, Ordering::Release);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn snapshot(&self) -> Vec<usize> {
        self.roots.lock().unwrap().clone()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}
```

#### 1.3 GcRootGuard (drop 模式)

**`crates/rudo-gc/src/tokio/guard.rs`**

```rust
use std::ptr::NonNull;

#[must_use]
pub struct GcRootGuard {
    ptr: usize,
    _phantom: std::marker::PhantomData<u8>,
}

impl GcRootGuard {
    #[must_use]
    pub fn new(ptr: NonNull<u8>) -> Self {
        let addr = ptr.as_ptr() as usize;
        crate::tokio::GcRootSet::global().register(addr);
        Self { ptr: addr, _phantom: std::marker::PhantomData }
    }
}

impl Drop for GcRootGuard {
    fn drop(&mut self) {
        crate::tokio::GcRootSet::global().unregister(self.ptr);
    }
}
```

#### 1.4 tokio module 入口

**`crates/rudo-gc/src/tokio/mod.rs`**

```rust
use crate::ptr::Gc;
use crate::Trace;

pub mod root;
pub mod guard;

pub use root::GcRootSet;
pub use guard::GcRootGuard;

#[cfg(feature = "tokio")]
pub trait GcTokioExt: Trace + Send + Sync {
    fn root_guard(&self) -> GcRootGuard;
    async fn yield_now(&self);
}

#[cfg(feature = "tokio")]
impl<T: Trace + Send + Sync> GcTokioExt for Gc<T> {
    fn root_guard(&self) -> GcRootGuard {
        let ptr = Gc::<T>::internal_ptr(self);
        GcRootGuard::new(unsafe { std::ptr::NonNull::new_unchecked(ptr as *mut u8) })
    }

    async fn yield_now(&self) {
        ::tokio::task::yield_now().await;
    }
}
```

---

### Phase 2: Gc 整合 (Week 2)

1. **`crates/rudo-gc/src/lib.rs`** - 導出 `tokio` module
2. **`crates/rudo-gc/src/ptr.rs`** - 可選：`yield_now()` 和 `root_guard()` 方法

---

### Phase 3: Proc-macro 自動化 (Week 3-4)

#### 3.1 `#[gc::main]` 巨集

**`crates/rudo-gc-derive/src/main.rs`**

```rust
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

#[proc_macro_attribute]
pub fn main(_args: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let fn_vis = &input.vis;
    let fn_sig = &input.sig;
    let fn_body = &input.block;

    let expanded = quote! {
        #fn_vis #fn_sig {
            use ::rudo_gc::tokio::GcRootSet;
            GcRootSet::global();
            let rt = ::tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed building the Runtime");
            rt.block_on(async { #fn_body })
        }
    };

    expanded.into()
}
```

#### 3.2 `#[gc::root]` 巨集

**`crates/rudo-gc-derive/src/root.rs`**

```rust
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

#[proc_macro_attribute]
pub fn root(_args: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let fn_vis = &input.vis;
    let fn_sig = &input.sig;
    let fn_body = &input.block;

    let expanded = quote! {
        #fn_vis #fn_sig {
            let _guard = ::rudo_gc::tokio::GcRootGuard::enter_scope();
            #fn_body
        }
    };

    expanded.into()
}
```

#### 3.3 rudo-gc-derive 導出

**`crates/rudo-gc-derive/src/lib.rs`**

```rust
mod main;
mod root;

pub use main::main;
pub use root::root;
```

---

### Phase 4: Spawn 包裝器 (Week 4-5)

**`crates/rudo-gc/src/tokio/spawn.rs`**

```rust
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(feature = "tokio")]
pub async fn spawn<F, T>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let wrapped = GcRootScope::new(future);
    tokio::task::spawn(wrapped).await
}

struct GcRootScope<F> {
    future: F,
    _guard: crate::tokio::GcRootGuard,
}

impl<F: Future> GcRootScope<F> {
    fn new(future: F) -> Self {
        Self {
            future,
            _guard: crate::tokio::GcRootGuard::enter_scope(),
        }
    }
}

impl<F: Future> Future for GcRootScope<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let future = unsafe { Pin::new_unchecked(&mut this.future) };
        future.poll(cx)
    }
}
```

---

### Phase 5: 測試與文件 (Week 5-6)

#### 5.1 測試類型

| 測試類型 | 描述 |
|----------|------|
| 單元測試 | `GcRootSet` 註冊/取消註冊邏輯 |
| 整合測試 | G task 中的使用 |
c 在 tokio| 壓力測試 | 大量任務、大量 Gc 指標 |
| 生命週期測試 | guard 正確釋放記憶體 |
| 多執行期測試 | 多個 tokio runtime 並存 |

#### 5.2 文件變更清單

```
crates/rudo-gc/
├── Cargo.toml                          [修改 - tokio feature]
├── src/
│   ├── lib.rs                          [修改 - pub mod tokio]
│   └── tokio/
│       ├── mod.rs                      [修改 - GcTokioExt]
│       ├── root.rs                     [新增]
│       ├── guard.rs                    [新增]
│       └── spawn.rs                    [新增]
└── tests/
    └── tokio_integration.rs            [新增]

crates/rudo-gc-derive/
├── Cargo.toml
└── src/
    ├── lib.rs                          [修改 - 導出新巨集]
    ├── main.rs                         [新增]
    └── root.rs                         [新增]

docs/
└── tokio-integration.md                [新增]
```

---

## API 使用範例

### 基本用法 (手動標註)

```rust
use rudo_gc::{Gc, Trace, GcTokioExt};

#[derive(Trace)]
struct Data { value: i32 }

async fn example() {
    let gc = Gc::new(Data { value: 42 });

    // 手動創建 root guard
    let _guard = gc.root_guard();

    tokio::spawn(async move {
        println!("{}", gc.value);  // gc 在此 task 執行期間受保護
    }).await.unwrap();

    // _guard 銷毀時，自動取消註冊
}
```

### 使用 yield_now

```rust
use rudo_gc::GcTokioExt;

async fn gc_friendly_task() {
    let gc = Gc::new(vec![1, 2, 3]);

    for i in 0..1000 {
        // 定期讓渡，允許 GC 執行
        gc.yield_now().await;
        do_work(&gc, i);
    }
}
```

### 使用 Spawn 包裝器

```rust
use rudo_gc::tokio::spawn;

let rt = tokio::runtime::Builder::new_multi_thread()
    .build()
    .unwrap();

let gc = Gc::new(Data { value: 42 });

spawn(async move {
    println!("{}", gc.value);
});
```

### 使用 Proc-macro

```rust
use rudo_gc_derive::main;
use rudo_gc::{Gc, Trace};

#[main]
async fn main() {
    let gc = Gc::new(Data { value: 42 });

    tokio::spawn(async move {
        println!("{}", gc.value);
    }).await.unwrap();
}
```

---

## 多執行期考量

由於 `GcRootSet` 是程序級的 (使用 `OnceLock`)，它能正確運作：

1. **執行期 A 生成任務** → 註冊 Gc 根
2. **執行期 B 生成任務** → 註冊 Gc 根
3. **GC 觸發** → 從全域 `GcRootSet::snapshot()` 讀取
4. **任務完成** → 在 `GcRootGuard` drop 時取消註冊

---

## 與 v2 的差異摘要

| v2 (初步) | v3 (最終) |
|-----------|-----------|
| 手動根追蹤為主 | proc-macro 自動化提升至 Phase 3 |
| 無 GC 喚醒機制 | `GcRootSet` 添加 `dirty` 標記 |
| 無巨集支援 | `#[gc::main]` 和 `#[gc::test]` |
| 簡單 spawn 包裝 | `GcRootScope` 包裝所有生成任務 |

---

## 時間線

| 階段 | 內容 | 預估 |
|------|------|------|
| Phase 1 | 基礎設施 | Week 1 (已完成) |
| Phase 2 | Gc 整合 | Week 2 |
| Phase 3 | Proc-macro | Week 3-4 |
| Phase 4 | Spawn 包裝器 | Week 4-5 |
| Phase 5 | 測試與文件 | Week 5-6 |

---

## 待確認問題

1. **用戶體驗**: drop guard 模式是否可接受？
2. **效能**: `GcRootSet` 的 atomic 操作是否有優化空間？
3. **邊界案例**: nested spawn 的根追蹤行為？
