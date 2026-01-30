# rudo-gc + Tokio Async/Await 整合計劃

**建立日期**: 2026-01-29
**作者**: opencode
**版本**: 1.0

## 執行摘要

本計劃描述如何將 rudo-gc 與 tokio async/await 系統整合，使 `Gc<T>` 可以在異步任務中安全使用。

### 關鍵決策

| 決策點 | 選擇 | 理由 |
|--------|------|------|
| Runtime 集成 | 擴展 `tokio::runtime::Builder` | 用戶無縫切換 |
| Future 掃描 | 保守式內存掃描 | 透明性高 |
| Feature 設計 | 單一 `tokio` feature，默認關閉 | 減少依賴 |

---

## 技術可行性

### 現有基礎設施

```rust
// ptr.rs:294-301 - 已支援 Send + Sync
unsafe impl<T: Trace + Send + Sync> Send for Gc<T> {}
unsafe impl<T: Trace + Send + Sync> Sync for Gc<T> {}
```

### Tokio Task Hooks (Stable API)

```rust
pub fn on_task_spawn<F>(&mut self, f: F) -> &mut Self
where F: Fn(&TaskMeta<'_>) + Send + Sync + 'static;

pub fn on_task_terminate<F>(&mut self, f: F) -> &mut Self
where F: Fn(&TaskMeta<'_>) + Send + Sync + 'static;
```

---

## 架構設計

```
┌─────────────────────────────────────────────────┐
│              User Application                    │
├─────────────────────────────────────────────────┤
│  #[gc::main]                                    │
│  async fn main() {                              │
│      let gc = Gc::new(...);                     │
│      tokio::spawn(async move { ... });          │
│  }                                              │
├─────────────────────────────────────────────────┤
│              rudo-gc-tokio Layer                │
├─────────────────────────────────────────────────┤
│  GcRuntimeBuilder    FutureRootTracker          │
│  enable_gc()          scan_all_tasks()          │
├─────────────────────────────────────────────────┤
│              rudo-gc Core                       │
├─────────────────────────────────────────────────┤
│  LocalHeap    ThreadRegistry    Gc (Send+Sync)  │
└─────────────────────────────────────────────────┘
```

---

## 實現階段

### Phase 1: 基礎設施 (Week 1)

#### 1.1 Cargo.toml

```toml
# crates/rudo-gc/Cargo.toml
[features]
tokio = ["dep:tokio"]

[dependencies]
tokio = { version = "1.0", optional = true, default-features = false }
```

#### 1.2 新增 `src/tokio_mod.rs`

```rust
pub mod builder;
pub mod hooks;
pub mod scanner;

pub use builder::Builder;
pub use hooks::{GcTaskRegistry, global_registry};
pub use scanner::FutureScanner;
```

#### 1.3 新增 `Gc::yield_now()` (ptr.rs)

```rust
#[cfg(feature = "tokio")]
pub async fn yield_now() {
    crate::heap::check_safepoint();
    tokio::task::yield_now().await;
    crate::heap::check_safepoint();
}

#[cfg(not(feature = "tokio"))]
pub async fn yield_now() {}
```

---

### Phase 2: Future 掃描引擎 (Week 2)

#### 2.1 新增 `src/scan/future.rs`

```rust
use crate::heap::{page_size, page_mask, HEAP_HINT_ADDRESS};

pub fn conservative_scan_region(
    ptr: *const u8,
    size: usize,
    mut scan_fn: impl FnMut(*const u8),
) {
    let end = unsafe { ptr.add(size) };
    let mut current = ptr;
    let ptr_size = std::mem::size_of::<usize>();

    while unsafe { current.add(ptr_size) <= end } {
        let value = unsafe { std::ptr::read_unaligned(current as *const usize) };
        if is_likely_gc_pointer(value) {
            scan_fn(value as *const u8);
        }
        current = unsafe { current.add(ptr_size) };
    }
}

fn is_likely_gc_pointer(addr: usize) -> bool {
    if addr == 0 { return false; }
    let in_heap = addr >= HEAP_HINT_ADDRESS && addr < HEAP_HINT_ADDRESS + (1024 * 1024 * 1024);
    let aligned = (addr & !page_mask()) >= std::mem::size_of::<crate::heap::PageHeader>();
    in_heap && aligned
}

pub unsafe fn scan_task_memory(task_header: *const u8, scan_fn: impl FnMut(*const u8)) {
    let future_offset = 72; // tokio task layout offset
    let future_ptr = task_header.add(future_offset);
    conservative_scan_region(future_ptr, 64, scan_fn);
}
```

#### 2.2 新增 `src/scan/mod.rs`

```rust
pub mod future;
pub use future::{conservative_scan_region, scan_task_memory};
```

---

### Phase 3: Task Hook 系統 (Week 3)

#### 3.1 新增 `src/tokio/hooks.rs`

```rust
use std::sync::{Arc, RwLock, atomic::{AtomicU64, AtomicBool, Ordering}};

#[derive(Debug)]
pub struct TaskHookData {
    pub task_header: *const u8,
    pub task_id: u64,
    pub active: AtomicBool,
}

impl TaskHookData {
    pub fn new(task_header: *const u8, task_id: u64) -> Self {
        Self { task_header, task_id, active: AtomicBool::new(true) }
    }
}

#[derive(Debug)]
pub struct GcTaskRegistry {
    tasks: RwLock<Vec<Arc<TaskHookData>>>,
    task_count: AtomicU64,
}

impl GcTaskRegistry {
    pub fn new() -> Self {
        Self { tasks: RwLock::new(Vec::new()), task_count: AtomicU64::new(0) }
    }

    pub fn on_spawn(&self, task_header: *const u8) {
        let id = self.task_count.fetch_add(1, Ordering::SeqCst);
        self.tasks.write().unwrap().push(Arc::new(TaskHookData::new(task_header, id)));
    }

    pub fn on_terminate(&self, task_header: *const u8) {
        let tasks = self.tasks.read().unwrap();
        for task in tasks.iter() {
            if task.task_header == task_header {
                task.active.store(false, Ordering::Release);
                break;
            }
        }
    }

    pub fn get_active_task_headers(&self) -> Vec<*const u8> {
        let tasks = self.tasks.read().unwrap();
        tasks.iter().filter(|t| t.active.load(Ordering::Acquire)).map(|t| t.task_header).collect()
    }
}

pub fn global_registry() -> &'static GcTaskRegistry {
    static REGISTRY: std::sync::OnceLock<GcTaskRegistry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(GcTaskRegistry::new)
}
```

---

### Phase 4: Runtime 構建器 (Week 3-4)

#### 4.1 新增 `src/tokio/builder.rs`

```rust
use tokio::runtime::{Builder as TokioBuilder, Runtime};
use super::{GcTaskRegistry, FutureScanner, global_registry};

#[derive(Debug, Default)]
pub struct GcConfig { enabled: bool }

#[derive(Debug)]
pub struct Builder {
    inner: TokioBuilder,
    gc_config: GcConfig,
}

impl Builder {
    pub fn new_multi_thread() -> Self {
        Self { inner: TokioBuilder::new_multi_thread(), gc_config: GcConfig::default() }
    }

    pub fn new_current_thread() -> Self {
        Self { inner: TokioBuilder::new_current_thread(), gc_config: GcConfig::default() }
    }

    pub fn enable_gc(&mut self) -> &mut Self {
        self.gc_config.enabled = true;
        self
    }

    pub fn build(&mut self) -> Result<Runtime, std::io::Error> {
        if !self.gc_config.enabled {
            return self.inner.build();
        }

        let registry = global_registry().clone();

        self.inner.on_task_spawn(move |_meta| {
            // TODO: Get task header from meta
            // registry.on_spawn(task_header);
        });

        self.inner.on_task_terminate(move |_meta| {
            // TODO: Get task header from meta
            // registry.on_terminate(task_header);
        });

        self.inner.build()
    }
}
```

---

### Phase 5: GC 收集整合 (Week 4)

#### 5.1 修改 `gc/gc.rs`

```rust
#[cfg(feature = "tokio")]
fn get_future_roots() -> Vec<*const u8> {
    use crate::tokio_mod::{global_registry, FutureScanner};

    let registry = global_registry();
    let scanner = FutureScanner;
    let task_headers = registry.get_active_task_headers();
    scanner.scan_all_tasks(&task_headers)
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

    // Future 根
    #[cfg(feature = "tokio")]
    all_roots.extend(get_future_roots());

    // ... 其餘收集邏輯
}
```

---

## 文件變更清單

```
crates/rudo-gc/
├── Cargo.toml                       [修改]
├── src/
│   ├── lib.rs                      [修改]
│   ├── ptr.rs                      [修改 - 新增 yield_now]
│   ├── scan/                       [新增]
│   │   ├── mod.rs
│   │   └── future.rs
│   └── tokio/                      [新增]
│       ├── mod.rs
│       ├── builder.rs
│       ├── hooks.rs
│       └── scanner.rs
└── tests/
    └── tokio_integration.rs        [新增]
```

---

## API 使用示例

### 基本用法

```rust
use rudo_gc::{Gc, Trace, GcTaskExt};

#[derive(Trace)]
struct Data { value: i32 }

#[gc::main]
async fn main() {
    let rt = tokio::runtime::Handle::current();
    let gc = Gc::new(Data { value: 42 });

    rt.gc_spawn(async move {
        println!("{}", gc.value);
        Gc::yield_now().await;
    }).await.unwrap();
}
```

### Runtime Builder

```rust
use tokio::runtime::Builder;

let rt = Builder::new_multi_thread()
    .enable_gc()
    .worker_threads(4)
    .build()
    .unwrap();

rt.block_on(async {
    let gc = Gc::new(vec![1, 2, 3]);
    // ...
});
```

---

## 測試計劃

| 測試類型 | 描述 |
|----------|------|
| 單元測試 | Future 掃描、保守式指針檢測 |
| 整合測試 | Gc 在 tokio 任務中的使用 |
| 壓力測試 | 大量任務、大量 Gc 指針 |
| 生命周期測試 | 任務創建/銷毀時的根追蹤 |

---

## 風險與緩解

| 風險 | 級別 | 緩解方案 |
|------|------|----------|
| Future 掃描不完整 | 中 | 保守式掃描 + 用戶可選手動標記 |
| tokio 版本兼容性 | 低 | 針對 stable API 設計 |
| 性能開銷 | 低 | Future 根追蹤優化 |

---

## 時間線

| 階段 | 內容 | 預估 |
|------|------|------|
| Phase 1 | 基礎設施 | Week 1 |
| Phase 2 | Future 掃描 | Week 2 |
| Phase 3 | Task Hook | Week 3 |
| Phase 4 | Runtime 構建器 | Week 3-4 |
| Phase 5 | GC 整合 | Week 4 |
| Phase 6 | 測試與文檔 | Week 5-6 |

---

## 待確認問題

1. **tokio 版本**: 使用 tokio 1.0+ stable API
2. **任務頭指針**: 需要驗證 tokio Task 結構佈局以獲取正確偏移量
