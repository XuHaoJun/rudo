# HandleScope 技術規格文件

**版本**: 1.0  
**日期**: 2026-01-31  
**作者**: rudo-gc Team  
**狀態**: 草稿

---

## 摘要

本文件描述 `rudo-gc` 垃圾收集器的 **HandleScope** 實作規格。透過參考 V8 JavaScript Engine 的 HandleScope 架構，我們提出一個漸進式遷移方案，旨在消除現有 **Conservative Stack Scanning** 所帶來的 soundness 風險（False Negatives 導致 UAF），同時保持 `Gc<T>` API 的向後相容性。

**核心設計原則**：
- **精確根追蹤 (Exact Root Tracing)**：以 handle block 取代 stack 暴力掃描
- **RAII 自動管理**：利用 Rust `Drop` trait 自動處理 scope 邊界
- **漸進式遷移**：透過 feature flag 逐步引入，無破壞性變更
- **零假陽性 (Zero False Positives)**：消除記憶體洩漏的保守式掃描副作用

---

## 1. 背景與動機

### 1.1 現有架構問題

`rudo-gc` 目前採用 **Conservative Stack Scanning** 策略 (`crates/rudo-gc/src/stack.rs:137-230`)，此策略存在以下根本性缺陷：

#### 1.1.1 False Negatives (漏掃) 導致 UAF

**問題位置**：`crates/rudo-gc/src/heap.rs` 中的 `find_gc_box_from_ptr` 函數

```rust
// 現有實作問題：對於小物件，不支援 Interior Pointer
} else if offset_to_use % block_size_to_use != 0 {
    // For small objects, we still require them to point to the start of an object
    return None; 
}
```

**重現場景**：
```rust
struct Node { a: u64, b: u64 }
let node = Gc::new(Node { a: 1, b: 2 });
let ref_b = &node.b; // 指向 offset + 8 的位置
drop(node); // stack 上只剩 ref_b

// GC 發生：
// Scanner 掃到 ref_b (interior pointer)
// find_gc_box 計算 offset = 8, block_size = 16
// 8 % 16 != 0 -> return None (認為不是 GC 指標)
// 結果：Node 被回收，ref_b 變成懸空指標 -> UAF!
```

#### 1.1.2 Pointer Provenance 問題

現有實作大量使用 `ptr as usize` 和 `usize as ptr` 轉換 (`scan.rs`, `heap.rs`)：

```rust
// 在 Strict Provenance 模型下，這是 Unsound 的
let addr = ptr as usize;
let ptr_back = addr as *const u8;
```

此模式在未來 Rust 版本或 CHERI 架構硬體上可能失效。

#### 1.1.3 暫存器掃描完整性

`spill_registers_and_scan()` (`stack.rs:137-230`) 僅處理 **Callee-Saved** 暫存器：

```rust
// x86_64 僅處理：rbx, rbp, r12, r13, r14, r15
std::arch::asm!(
    "mov {0}, rbx",
    "mov {1}, rbp",
    "mov {2}, r12",
    // ...
);
```

LLVM 可能將指標暫存於 **Caller-Saved** 暫存器或向量暫存器 (AVX/SSE)。

### 1.2 V8 HandleScope 的啟發

V8 JavaScript Engine 面對相同挑戰，採用完全不同的策略：**明確的 Handle Scope 管理**。

---

## 2. V8 HandleScope 深度分析

本節基於 V8 原始碼分析，提取關鍵設計模式。

### 2.1 HandleScopeData 結構

**V8 原始碼位置**：`/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles.h:31`

```cpp
// V8 HandleScopeData
struct HandleScopeData {
  Address* next;      // 下一個可分配位置
  Address* limit;     // 當前 block 結尾
  int level = 0;      // nested scope 層數
};
```

**設計優點**：
- `next` 和 `limit` 形成 **sliding window**，決定當前 scope 的 handle 範圍
- `level` 用於驗證 nested scope 的正確性

### 2.2 LocalHandles 管理

**V8 原始碼位置**：`/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles.h:19-42`

```cpp
class LocalHandles {
 public:
  LocalHandles();
  ~LocalHandles();
  void Iterate(RootVisitor* visitor);  // GC 時遍歷所有 handles

 private:
  HandleScopeData scope_;
  std::vector<Address*> blocks_;        // handle blocks 列表
};
```

**關鍵機制**：
- `blocks_` 以 256 個 handle 為單位動態擴展 (`kHandleBlockSize`)
- `Iterate(RootVisitor)` 遍歷所有 blocks，精確收集 roots

### 2.3 HandleScope 生命週期

**V8 原始碼位置**：`/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles.h:44-89`

```cpp
class V8_NODISCARD LocalHandleScope {
 public:
  explicit inline LocalHandleScope(LocalHeap* local_heap);
  inline ~LocalHandleScope();

 private:
  LocalHeap* local_heap_;
  Address* prev_limit_;
  Address* prev_next_;
};
```

**RAII 模式**：
```cpp
// 建構時：保存當前狀態
LocalHandleScope::LocalHandleScope(LocalHeap* local_heap) {
  LocalHandles* handles = local_heap->handles();
  prev_next_ = handles->scope_.next;
  prev_limit_ = handles->scope_.limit;
  handles->scope_.level++;
}

// 解構時：還原狀態，使 scope 內 handles 失效
LocalHandleScope::~LocalHandleScope() {
  CloseScope(local_heap_, prev_next_, prev_limit_);
}
```

### 2.4 Handle 分配

**V8 原始碼位置**：`/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles-inl.h:19-34`

```cpp
V8_INLINE Address* LocalHandleScope::GetHandle(LocalHeap* local_heap,
                                               Address value) {
  LocalHandles* handles = local_heap->handles();
  Address* result = handles->scope_.next;
  
  if (result == handles->scope_.limit) {
    result = handles->AddBlock();  // 區塊滿了，新增 block
  }
  
  handles->scope_.next++;
  *result = value;  // 寫入 handle
  return result;
}
```

**設計特點**：
- **O(1) 分配**：指針遞增，無鎖操作
- **區塊化**：減少系統呼叫次數
- **Lazy 釋放**：`CloseScope` 時標記未使用 blocks，待下次 GC 或閒置時回收

### 2.5 LocalHeap 整合

**V8 原始碼位置**：`/home/noah/Desktop/rudo/learn-projects/v8/src/heap/local-heap.h:50-76`

```cpp
class LocalHeap {
 public:
  LocalHandles* handles() { return handles_.get(); }
  
 private:
  std::unique_ptr<LocalHandles> handles_;
  // ...
};
```

**Thread-Local 綁定**：
- 每個執行緒的 `LocalHeap` 包含獨立的 `LocalHandles`
- GC 時，每個執行緒的 handles 被遍歷收集 roots

---

## 3. rudo-gc 現有架構分析

### 3.1 ThreadControlBlock 對應

**位置**：`crates/rudo-gc/src/heap.rs:42-55`

```rust
pub struct ThreadControlBlock {
    pub state: AtomicUsize,
    pub gc_requested: AtomicBool,
    pub park_cond: Condvar,
    pub park_mutex: Mutex<()>,
    pub heap: UnsafeCell<LocalHeap>,
    pub stack_roots: Mutex<Vec<*const u8>>,  // 對應 V8 的 handles
}
```

**對應關係**：

| V8 | rudo-gc | 說明 |
|----|---------|------|
| `LocalHeap` | `ThreadControlBlock` | 執行緒本地狀態 |
| `LocalHandles` | 新增 `HandleBlock` | 管理 handles |
| `HandleScope` | 新增 `HandleScope` | RAII scope 管理 |
| `RootVisitor` | `GcVisitor` | GC 時遍歷 roots |

### 3.2 現有 GC Root 收集

**位置**：`crates/rudo-gc/src/gc/gc.rs:703-791`

```rust
fn mark_minor_roots_multi(heap: &mut LocalHeap, stack_roots: &[...]) {
    // 問題：conservative scan，可能漏掃
    crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
        if let Some(gc_box_ptr) = crate::heap::find_gc_box_from_ptr(...) {
            mark_object_minor(gc_box_ptr, &mut visitor);
        }
    });
    
    // 問題：interior pointer 不被識別
    for &(ptr, _) in stack_roots {
        unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                mark_object(gc_box, &mut visitor);
            }
        }
    }
}
```

---

## 4. 提議的 HandleScope 設計

### 4.1 架構總覽

```
┌─────────────────────────────────────────────────────────────────┐
│                        HandleScope 架構                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────┐    ┌─────────────────────┐             │
│  │   HandleScope       │    │   HandleBlock       │             │
│  │   - prev_next       │    │   - handles: Vec<T> │             │
│  │   - prev_limit      │    │   - capacity: 256   │             │
│  │   - tcb: Arc<TCB>   │    │                     │             │
│  │   - Drop impl       │    │                     │             │
│  └─────────────────────┘    └─────────────────────┘             │
│           │                          ▲                          │
│           │ (RAII)                   │ (allocate)               │
│           ▼                          │                          │
│  ┌─────────────────────────────────┴──────────────┐             │
│  │          ThreadControlBlock                    │             │
│  │  (現有)                                          │             │
│  │  + handle_scope_data: HandleScopeData           │             │
│  │  + handle_blocks: Vec<NonNull<HandleBlock>>     │             │
│  └─────────────────────────────────────────────────┘             │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 4.2 核心資料結構

#### 4.2.1 HandleScopeData

```rust
// crates/rudo-gc/src/handles/mod.rs

pub struct HandleScopeData {
    /// 下一個可分配 handle 的位置
    next: *mut *const GcBox<()>,
    /// 當前 block 的結尾位置
    limit: *mut *const GcBox<()>,
    /// Nested scope 層數 (用於驗證)
    level: usize,
}

impl HandleScopeData {
    fn new() -> Self {
        Self {
            next: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            level: 0,
        }
    }
}
```

#### 4.2.2 HandleBlock

```rust
/// Handle block 大小 (與 V8 一致)
const HANDLE_BLOCK_SIZE: usize = 256;

pub struct HandleBlock {
    /// Block 中的 handles (固定大小陣列)
    handles: [std::ptr::NonNull<GcBox<()>>; HANDLE_BLOCK_SIZE],
    /// 實際使用的 slot 數量
    used: usize,
}

impl HandleBlock {
    fn new() -> Self {
        Self {
            handles: [std::ptr::NonNull::dangling(); HANDLE_BLOCK_SIZE],
            used: 0,
        }
    }
    
    fn allocate(&mut self) -> *mut *const GcBox<()> {
        if self.used >= HANDLE_BLOCK_SIZE {
            std::ptr::null_mut()
        } else {
            let ptr = &mut self.handles[self.used] as *mut _ as *mut *const GcBox<()>;
            self.used += 1;
            ptr
        }
    }
}
```

#### 4.2.3 HandleScope

```rust
pub struct HandleScope<'a> {
    /// 關聯的執行緒控制區塊
    tcb: &'a std::sync::Arc<ThreadControlBlock>,
    /// 先前的 next 指標 (用於還原)
    prev_next: *mut *const GcBox<()>,
    /// 先前的 limit 指標 (用於還原)
    prev_limit: *mut *const GcBox<()>,
}

impl<'a> HandleScope<'a> {
    /// 建立新的 HandleScope
    pub fn new(tcb: &'a std::sync::Arc<ThreadControlBlock>) -> Self {
        let data = &mut tcb.handle_scope_data;
        
        Self {
            tcb,
            prev_next: data.next,
            prev_limit: data.limit,
        }
    }
    
    /// 在當前 scope 中分配 handle
    pub fn allocate(&mut self) -> *mut *const GcBox<()> {
        let data = &mut self.tcb.handle_scope_data;
        
        // 如果當前 block 已滿，需要擴展
        if data.next == data.limit {
            self.add_block();
        }
        
        let handle_ptr = data.next;
        data.next = data.next.wrapping_add(1);
        handle_ptr
    }
    
    fn add_block(&mut self) {
        let block = Box::new(HandleBlock::new());
        let block_ptr = Box::into_raw(block);
        self.tcb.handle_blocks.push(std::ptr::NonNull::dangling());
        
        let data = &mut self.tcb.handle_scope_data;
        data.next = block_ptr as *mut *const GcBox<()>;
        data.limit = unsafe {
            block_ptr.as_mut().unwrap_unchecked().handles[HANDLE_BLOCK_SIZE - 1].as_ptr()
        }.wrapping_add(1);
    }
}

impl<'a> Drop for HandleScope<'a> {
    fn drop(&mut self) {
        let data = &mut self.tcb.handle_scope_data;
        
        // 還原指標，使當前 scope 內的所有 handles 失效
        data.next = self.prev_next;
        data.limit = self.prev_limit;
        data.level = data.level.saturating_sub(1);
        
        // 標記未使用的 blocks 待回收 (由 GC 或 cleanup 任務處理)
    }
}
```

#### 4.2.4 Handle<T> 類型

```rust
/// Handle 類型 (類似 V8 Handle<T>)
///
/// Handle 是一個包裝類型，持有 GC 堆中物件的指標。
/// Handle 只能在 HandleScope 內創建，當 HandleScope 結束時，
/// 所有在該 scope 內創建的 Handles 都會自動失效。
pub struct Handle<T: Trace> {
    /// 內部指標，指向 handle block 中的槽位
    ptr: *const GcBox<T>,
}

impl<T: Trace> Handle<T> {
    /// 從 Gc<T> 創建新的 Handle
    #[must_use]
    pub fn new(gc: &Gc<T>) -> Self {
        let mut scope = HandleScope::current();
        let handle_ptr = scope.allocate();
        
        unsafe {
            *handle_ptr = Gc::internal_ptr(gc) as *const GcBox<()>;
        }
        
        Self {
            ptr: handle_ptr as *const GcBox<T>,
        }
    }
    
    /// 取得 Handle 所指向的物件
    #[must_use]
    pub fn get(&self) -> &T {
        unsafe {
            &(*self.ptr).value
        }
    }
}

impl<T: Trace> std::ops::Deref for Handle<T> {
    type Target = T;
    
    fn deref(&self) -> &Self::Target {
        self.get()
    }
}
```

### 4.3 GC Root 收集整合

#### 4.3.1 HandleScope 遍歷

```rust
impl ThreadControlBlock {
    /// 遍歷所有 handles，收集 GC roots
    pub fn iterate_handles(&self, visitor: &mut GcVisitor) {
        for block_ptr in &self.handle_blocks {
            let block = unsafe { &*block_ptr.as_ptr() };
            for i in 0..block.used {
                let handle_ptr = &block.handles[i] as *const _ as *const *const GcBox<()>;
                let gc_box_ptr = unsafe { *handle_ptr };
                
                if let Some(gc_box) = unsafe { 
                    crate::heap::find_gc_box_from_ptr(
                        &self.heap_mut(), 
                        gc_box_ptr as *const u8
                    )
                } {
                    visitor.mark(gc_box);
                }
            }
        }
    }
}
```

#### 4.3.2 GC 收集函數修改

```rust
// crates/rudo-gc/src/gc/gc.rs

fn mark_minor_roots_multi(
    heap: &mut LocalHeap,
    stack_roots: &[(*const u8, std::sync::Arc<ThreadControlBlock>)],
) {
    let mut visitor = GcVisitor::new(VisitorKind::Minor);
    
    // 優先遍歷 handles (精確 root)
    for tcb in /* 所有執行緒的 TCB */ {
        tcb.iterate_handles(&mut visitor);
    }
    
    // Fallback: Conservative scan (向後相容)
    // 只對非 handle 區域做保守掃描
    #[cfg(feature = "handle-scope-conservative-fallback")]
    {
        unsafe {
            crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
                // 略過已由 handle 涵蓋的區域
                if is_in_handle_region(potential_ptr) {
                    return;
                }
                if let Some(gc_box_ptr) = crate::heap::find_gc_box_from_ptr(heap, potential_ptr as *const u8) {
                    mark_object_minor(gc_box_ptr, &mut visitor);
                }
            });
        }
    }
    
    visitor.process_worklist();
}
```

---

## 5. 漸進式遷移策略

### 5.1 Feature Flag 設計

```toml
# Cargo.toml

[features]
default = []

# 啟用 HandleScope 模式 (實驗性)
handle-scope = []

# 在 handle-scope 模式下，是否保留保守掃描作為 fallback
handle-scope-conservative-fallback = ["handle-scope"]

# 預設安全模式 (推薦)
safe = ["handle-scope", "handle-scope-conservative-fallback"]
```

### 5.2 遷移階段

#### Phase 1: 實驗性引入 (v0.6.0)

```rust
#[cfg(feature = "handle-scope")]
let _scope = HandleScope::new(&tcb);

let gc = Gc::new(data);
// 如果 feature 啟用，gc 會自動註冊為 root
// 如果 feature 未啟用，維持現有 conservative scan
```

**目標**：
- 驗證設計可行性
- 收集效能數據
- 不影響現有使用者

#### Phase 2: 預設啟用 (v0.7.0)

```toml
[features]
default = ["safe"]
safe = ["handle-scope", "handle-scope-conservative-fallback"]
```

**變更**：
- `default` feature 包含 `safe`
- 新專案預設使用 HandleScope
- 舊專案不受影響

#### Phase 3: 移除保守掃描 (v1.0.0)

```rust
// 破壞性變更
#[cfg(feature = "safe")]
let _scope = HandleScope::new(&tcb); // 必需

#[cfg(not(feature = "safe"))]
let gc = Gc::new(data); // 發出 deprecation warning
```

**目標**：
- 完全消除 Conservative Stack Scanning
- 確保 100% soundness

### 5.3 API 相容性

| 場景 | 現有 API | 新 API | 相容性 |
|------|----------|--------|--------|
| 基本使用 | `Gc::new(x)` | `Gc::new(x)` | ✅ 完全相容 |
| HandleScope | N/A | `HandleScope::new(&tcb)` | ✅ 新增 |
| Handle<T> | N/A | `Handle::new(&gc)` | ✅ 新增 |
| 跨執行緒 | `Gc::new(x).send()` | `Handle::new(&gc)` | ✅ 需用 Handle |

---

## 6. 實作計畫

### 6.1 檔案結構

```
crates/rudo-gc/
├── src/
│   ├── handles/                    # 新增目錄
│   │   ├── mod.rs                  # HandleScope, HandleBlock, Handle
│   │   └── iter.rs                 # RootVisitor 整合
│   ├── heap.rs                     # ThreadControlBlock 擴展
│   ├── gc/
│   │   └── gc.rs                   # mark_*_roots_* 函數修改
│   └── lib.rs                      # re-exports
```

### 6.2 實作 Task List

| ID | Task | 估計工作量 | 優先順序 |
|----|------|-----------|----------|
| H1 | 建立 `handles/mod.rs` 骨架 | 1 天 | P1 |
| H2 | 實作 `HandleBlock` | 1 天 | P1 |
| H3 | 實作 `HandleScope` (RAII) | 2 天 | P1 |
| H4 | 實作 `Handle<T>` 類型 | 1 天 | P1 |
| H5 | 擴展 `ThreadControlBlock` | 1 天 | P1 |
| H6 | 整合 GC root 收集 | 2 天 | P1 |
| H7 | 效能測試與優化 | 2 天 | P2 |
| H8 | 文件與範例 | 1 天 | P2 |

### 6.3 測試策略

#### 6.3.1 單元測試

```rust
#[cfg(test)]
mod handle_scope_tests {
    use super::*;
    
    #[test]
    fn test_handle_scope_allocation() {
        let tcb = ThreadControlBlock::new();
        let mut scope = HandleScope::new(&tcb);
        
        let handle1 = scope.allocate();
        let handle2 = scope.allocate();
        
        assert!(!handle1.is_null());
        assert!(!handle2.is_null());
        assert_ne!(handle1, handle2);
    }
    
    #[test]
    fn test_handle_scope_drop() {
        let tcb = ThreadControlBlock::new();
        
        {
            let _scope = HandleScope::new(&tcb);
            let handle = tcb.handle_scope_data.next;
            // handle 有效
        }
        
        // handle 已失效 (scope 結束)
    }
}
```

#### 6.3.2 整合測試

- `tests/handlescope_basic.rs`：基本使用範例
- `tests/handlescope_async.rs`：Tokio 整合測試
- `tests/handlescope_thread.rs`：多執行緒測試

---

## 7. 風險評估與緩解

### 7.1 技術風險

| 風險 | 影響 | 機率 | 緩解措施 |
|------|------|------|----------|
| Handle 泄漏 | 記憶體洩漏 | 低 | `Drop` 自動清理；文件提醒 |
| 效能回退 | 吞吐量下降 | 低 | 基準測試；優化 handle allocation |
| 向後相容性破壞 | 使用者遷移困難 | 低 | Feature flag；漸進式遷移 |

### 7.2 設計權衡

| 權衡點 | 選擇 | 理由 |
|--------|------|------|
| Handle 儲存 | Pointer (非 index) | 簡化 `Handle<T>` API；相容現有 `Gc<T>` |
| Block 大小 | 256 (與 V8 一致) | 平衡記憶體與分配頻率 |
| Scope 管理 | RAII + Drop | Rust idiomatic；安全 |

---

## 8. 參考文獻

### 8.1 V8 原始碼

| 路徑 | 內容 |
|------|------|
| `/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles.h` | `LocalHandles`, `LocalHandleScope` 定義 |
| `/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles.cc` | `LocalHandles` 實作 |
| `/home/noah/Desktop/rudo/learn-projects/v8/src/handles/local-handles-inl.h` | `GetHandle`, `CloseScope` 內聯實作 |
| `/home/noah/Desktop/rudo/learn-projects/v8/src/handles/handles.h` | `Handle<T>`, `HandleScope` 定義 |
| `/home/noah/Desktop/rudo/learn-projects/v8/src/handles/handles-inl.h` | Handle 內聯函數 |
| `/home/noah/Desktop/rudo/learn-projects/v8/src/heap/local-heap.h` | `LocalHeap` 定義 |
| `/home/noah/Desktop/rudo/learn-projects/v8/src/heap/local-heap.cc` | `LocalHeap` 實作 |

### 8.2 rudo-gc 原始碼

| 路徑 | 內容 |
|------|------|
| `crates/rudo-gc/src/stack.rs` | Conservative Stack Scanning |
| `crates/rudo-gc/src/heap.rs` | `ThreadControlBlock`, `LocalHeap` |
| `crates/rudo-gc/src/gc/gc.rs` | Mark-Sweep Collection |
| `crates/rudo-gc/src/ptr.rs` | `Gc<T>` 實作 |

### 8.3 相關文件

| 文件 | 內容 |
|------|------|
| `/home/noah/Desktop/rudo/docs/Investigating Conservative Scanning.md` | Conservative Scanning 問題分析 |
| `/home/noah/Desktop/rudo/learn-projects/gc-arena/src/arena.rs` | gc-arena 設計參考 |
| `/home/noah/Desktop/rudo/learn-projects/dumpster/dumpster/src/unsync/mod.rs` | dumpster 設計參考 |

---

## 9. 附錄：替代方案比較

### 9.1 方案比較表

| 特性 | Conservative Scan (現有) | HandleScope (提議) | gc-arena | dumpster |
|------|-------------------------|-------------------|----------|----------|
| **Soundness** | ❌ 有 False Negative 風險 | ✅ 精確追蹤 | ✅ 編譯期保證 | ✅ RAII |
| **Ergonomics** | ✅ 簡單 (如 Rc) | ✅ 簡單 + 可選 | ❌ 需閉包 | ✅ 簡單 |
| **效能** | ⚠️ 不穩定 | ✅ 可預測 | ✅ 可預測 | ❌ RefCount 開銷 |
| **跨執行緒** | ⚠️ root_guard() 必需 | ✅ Handle 機制 | ❌ 不支援 | ✅ 支援 |
| **遷移成本** | N/A | ✅ 漸進式 | ❌ 需重寫 | ❌ 需重寫 |

### 9.2 選擇 HandleScope 的理由

1. **漸進式遷移**：無需破壞現有 API
2. **效能可預測**：消除保守掃描的不確定性
3. **跨執行緒安全**：`Handle<T>` 提供清晰的跨執行緒語義
4. **V8 實戰驗證**：已在大規模生產環境驗證

---

*文件結束*
