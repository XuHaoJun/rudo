# rudo-gc + Tokio Async/Await 整合計劃 v2

**建立日期**: 2026-01-29
**作者**: opencode
**版本**: 2.0
**基於**: v1 技術審查 + Dybvig/Lerche 架構分析

## 執行摘要

本計劃描述如何將 rudo-gc 與 tokio async/await 系統整合。

### 關鍵認知 (v1 → v2 修正)

| v1 假設 | v2 事實 | 影響 |
|---------|---------|------|
| 可透過 `TaskMeta` 取得 task header | `TaskMeta` 只提供 `id()` 和 `spawned_at()` | 透明掃描不可行 |
| 保守式掃描 task memory | tokio 不會暴露內部記憶體結構 | 需用戶配合 |
| 完全透明整合 | 需手動/proc-macro 標註 | 用戶體驗取捨 |

### 核心決策

| 決策點 | 選擇 | 理由 |
|--------|------|------|
| 整合方式 | 手動標註 + proc-macro | tokio 不暴露內部，透明方案不可能 |
| Runtime 集成 | 最小侵入 | 只添加 `Gc::yield_now()` |
| Feature 設計 | 單一 `tokio` feature | 減少依賴 |
| GC 觸發 | 用戶控制 + safepoint | `Gc::yield_now()` |

---

## 技術可行性分析

### 為何透明掃描不可能

```rust
// tokio/src/runtime/task_hooks.rs - 穩定 API
pub struct TaskMeta<'a> {
    id: super::task::Id,           // 只提供 ID，不提供指標
    spawned_at: SpawnLocation,     // 僅源碼位置
    _phantom: PhantomData<&'a ()>,
}

// TaskMeta 沒有 task_header 或任何記憶體指標
// tokio 有意識地不暴露這些內部結構
```

**結論**: 需要用戶明確標註 captured Gc<T>，而非自動掃描。

### 專家分析 (推測)

#### R. Kent Dybvig 的觀點

-Chez Scheme GC 使用精確標記而非保守式掃描
-在編譯器層面控制 GC 點
-可能批評: "保守式掃描是必要的邪惡，但應盡量避免"
-建議: "用 proc-macro 自動生成 root registration"

#### Carl Lerche 的觀點

-Tokio 主要維護者，理解 async/await 完整生命週期
-建議: "不要 hack tokio，使用現有 API"
-推薦: "用 `yield_now` 作為 safepoint，用戶明確標註"

---

## 架構設計

```
┌─────────────────────────────────────────────────┐
│              User Application                    │
├─────────────────────────────────────────────────┤
│  #[gc::main]                                    │
│  async fn main() {                              │
│      let gc = Gc::new(...);                     │
│      gc::spawn(async move {                     │
│          let _guard = gc.root();                │
│          // ...                                 │
│      }).await;                                  │
│  }                                              │
├─────────────────────────────────────────────────┤
│              rudo-gc-tokio Layer                │
├─────────────────────────────────────────────────┤
│  GcRootGuard      Gc::yield_now()               │
│  (手動標註)        (safepoint)                  │
├─────────────────────────────────────────────────┤
│              rudo-gc Core                       │
├─────────────────────────────────────────────────┤
│  LocalHeap    ThreadRegistry    Gc (Send+Sync)  │
└─────────────────────────────────────────────────┘
```

### 核心元件

1. **`GcRootGuard`** - 手動根守護，用戶負責創建/銷毀
2. **`Gc::yield_now()`** - 異步讓渡點，允許 GC 執行
3. **`#[gc::root]` proc-macro** - 可選自動化 (v3)

---

## 實現階段

### Phase 1: 基礎設施 (Week 1)

#### 1.1 Cargo.toml

```toml
# crates/rudo-gc/Cargo.toml
[features]
default = ["derive"]
derive = ["dep:rudo-gc-derive"]
tokio = ["dep:tokio"]

[dependencies]
tokio = { version = "1.0", optional = true, default-features = false, features = ["rt"] }
```

#### 1.2 新增 `src/tokio/mod.rs`

```rust
#[cfg(feature = "tokio")]
pub mod yield_now;
```

#### 1.3 新增 `Gc::yield_now()` (ptr.rs)

```rust
#[cfg(feature = "tokio")]
pub async fn yield_now() {
    crate::gc::check_safepoint();
    tokio::task::yield_now().await;
    crate::gc::check_safepoint();
}

#[cfg(not(feature = "tokio"))]
pub async fn yield_now() {
    // 空實現，非 tokio 環境無效
}
```

---

### Phase 2: 手動根追蹤系統 (Week 2)

#### 2.1 新增 `src/tokio/root.rs`

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use crate::ptr::Gc;

type RootSet = Mutex<Vec<*const u8>>;
type RootCount = AtomicUsize;

#[derive(Debug, Clone)]
pub struct GcRootGuard {
    ptr: *const u8,
    roots: Arc<(RootSet, RootCount)>,
}

impl GcRootGuard {
    pub fn new<T: crate::Trace>(gc: &Gc<T>) -> Self {
        let internal_ptr = Gc::internal_ptr(gc);
        let (roots, count) = global_roots();
        
        roots.lock().unwrap().push(internal_ptr);
        count.fetch_add(1, Ordering::SeqCst);
        
        Self {
            ptr: internal_ptr,
            roots: Arc::new((roots, count)),
        }
    }
    
    pub fn is_same_root(&self, other: &GcRootGuard) -> bool {
        self.ptr == other.ptr
    }
}

impl Drop for GcRootGuard {
    fn drop(&mut self) {
        let (roots, count) = &*self.roots;
        roots.lock().unwrap().retain(|&p| p != self.ptr);
        count.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn get_registered_roots() -> Vec<*const u8> {
    let (roots, _) = global_roots();
    roots.lock().unwrap().clone()
}

pub fn root_count() -> usize {
    let (_, count) = global_roots();
    count.load(Ordering::SeqCst)
}

fn global_roots() -> &'static (RootSet, RootCount) {
    static ROOTS: std::sync::OnceLock<(RootSet, RootCount)> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| (Mutex::new(Vec::new()), AtomicUsize::new(0)))
}
```

#### 2.2 修改 GC 收集邏輯 (gc/gc.rs)

```rust
#[cfg(feature = "tokio")]
fn get_future_roots() -> Vec<*const u8> {
    crate::tokio::root::get_registered_roots()
}

#[cfg(not(feature = "tokio"))]
fn get_future_roots() -> Vec<*const u8> {
    Vec::new()
}

pub fn collect_with_future_roots() {
    let mut all_roots = Vec::new();
    
    // 傳統棧根
    for tcb in get_all_thread_control_blocks() {
        all_roots.extend(take_stack_roots(&tcb));
    }
    
    // 手動註冊的根
    #[cfg(feature = "tokio")]
    all_roots.extend(get_future_roots());
    
    // ... 其餘收集邏輯
}
```

---

### Phase 3: GcSpawn 包裝器 (Week 3)

#### 3.1 新增 `src/tokio/spawn.rs`

```rust
use std::future::Future;
use tokio::runtime::Handle;

/// 在 tokio runtime 中安全地 spawn task，
/// 並自動管理 Gc root registration。
pub async fn spawn<F, T>(handle: &Handle, future: F) -> tokio::task::JoinResult<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    handle.spawn(future).await
}

/// 將 async block 包裝為自動管理 root 的版本。
#[cfg(feature = "derive")]
pub async fn with_root<F, R, T>(gc: Gc<T>, f: F) -> R
where
    F: Future<Output = R>,
    T: crate::Trace + Send + 'static,
{
    let _guard = gc.into_root_guard();
    f.await
}

#[cfg(feature = "derive")]
impl<T: crate::Trace + Send + 'static> Gc<T> {
    /// 取得用於 async task 的 root guard。
    pub fn into_root_guard(self) -> (GcRootGuard, Gc<T>) {
        let guard = GcRootGuard::new(&self);
        (guard, self)
    }
    
    /// 取得 root guard 的引用（保留 Gc 的所有權）。
    pub fn root(&self) -> GcRootGuard {
        GcRootGuard::new(self)
    }
}
```

---

### Phase 4: proc-macro 自動化 (v3, Week 4-5)

#### 4.1 `rudo-gc-derive` 新增 `#[gc::root]`

```rust
#[gc::root]
async move {
    // 自動將所有捕獲的 Gc<T> 註冊為 root
    println!("{}", gc.value);  // gc 自動受保護
}
```

此為進階功能，v2 僅定義介面。

---

## 文件變更清單

```
crates/rudo-gc/
├── Cargo.toml                       [修改]
├── src/
│   ├── lib.rs                      [修改 - 新增 tokio module]
│   ├── ptr.rs                      [修改 - 新增 yield_now]
│   └── tokio/                      [新增]
│       ├── mod.rs
│       ├── yield_now.rs
│       ├── root.rs
│       └── spawn.rs
└── tests/
    └── tokio_integration.rs        [新增]
```

---

## API 使用示例

### 基本用法 (手動標註)

```rust
use rudo_gc::{Gc, Trace, GcRootGuard};

#[derive(Trace)]
struct Data { value: i32 }

async fn example() {
    let gc = Gc::new(Data { value: 42 });
    
    // 手動創建 root guard
    let _guard = gc.root();
    
    tokio::spawn(async move {
        println!("{}", gc.value);  // gc 在此 task 執行期間受保護
    }).await.unwrap();
    
    // _guard 銷毀時，自動取消註冊
}
```

### 使用 yield_now

```rust
use rudo_gc::Gc;

async fn gc_friendly_task() {
    let gc = Gc::new(vec![1, 2, 3]);
    
    for i in 0..1000 {
        // 定期讓渡，允許 GC 執行
        Gc::yield_now().await;
        do_work(&gc, i);
    }
}
```

### Spawn 包裝器

```rust
use rudo_gc::tokio::spawn;

let rt = tokio::runtime::Builder::new_multi_thread()
    .build()
    .unwrap();

let gc = Gc::new(Data { value: 42 });

spawn(&rt, async move {
    println!("{}", gc.value);
});
```

---

## 測試計劃

| 測試類型 | 描述 |
|----------|------|
| 單元測試 | `GcRootGuard` 註冊/取消註冊邏輯 |
| 整合測試 | Gc 在 tokio task 中的使用 |
| 壓力測試 | 大量任務、大量 Gc 指針 |
| 生命周期測試 | guard 正確釋放記憶體 |

---

## 風險與緩解

| 風險 | 級別 | 緩解方案 |
|------|------|----------|
| 用戶忘記標註 | 中 | proc-macro (v3) + 文檔警告 |
| tokio 版本兼容性 | 低 | 使用 stable API |
| yield_now 開銷 | 低 | 只在用戶呼叫時觸發 |

---

## 時間線

| 階段 | 內容 | 預估 |
|------|------|------|
| Phase 1 | 基礎設施 | Week 1 |
| Phase 2 | 手動根追蹤 | Week 2 |
| Phase 3 | Spawn 包裝器 | Week 3 |
| Phase 4 | Proc-macro (v3) | Week 4-5 |
| Phase 5 | 測試與文檔 | Week 5-6 |

---

## 與 v1 的差異摘要

| v1 (錯誤) | v2 (正確) |
|-----------|-----------|
| 透明掃描 task memory | 手動根標註 |
| 假設可取得 task header | 接受限制，不嘗試 |
| `GcTaskExt` trait | `GcRootGuard` + `Gc::root()` |
| 複雜的 task hook 系統 | 簡單的 global root set |

---

## 待確認問題

1. **用戶體驗**: 手動標註是否可接受？是否加速開發 proc-macro？
2. **效能**: `GcRootGuard` 的 atomic 操作是否有優化空間？
3. **邊界案例**: nested spawn 的根追蹤行為？
