# rudo-gc 高優先級修復技術規格

本文件為 `stable-issues-1.md` 中的 4 個高優先級問題提供詳細的技術修復方案。

---

## Issue #1: GcCell::Trace Panic 風險

### 問題摘要

`GcCell<T>` 的 `Trace` 實作使用 `RefCell::borrow()`，當 GC 在使用者持有 `borrow_mut()` 的情況下觸發時會 panic。

### 影響範圍

- **檔案：** `crates/rudo-gc/src/cell.rs`
- **行號：** 108-112
- **嚴重度：** Critical（會導致程式崩潰）

### 現有程式碼

```rust
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        self.inner.borrow().trace(visitor);
    }
}
```

### 修復方案

#### 方案 A：直接存取 RefCell 內部指標（推薦）

```rust
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        // SAFETY:
        // 1. GC 發生在 Stop-The-World (STW) 期間，所有 mutator 執行緒已暫停
        // 2. 雖然 Stack 上可能有活躍的 RefMut，但在 GC 掃描期間不會有並發寫入
        // 3. 我們只是讀取欄位來進行標記，不會破壞 RefCell 的內部狀態
        // 4. RefCell::as_ptr() 是安全的，它不會 panic
        let ptr = self.inner.as_ptr();
        unsafe {
            (*ptr).trace(visitor);
        }
    }
}
```

#### 方案 B：使用 try_borrow 並忽略失敗（備選）

```rust
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        // 如果已經被 mutable borrow，代表該值正在被修改
        // 在 STW 期間這意味著 borrow 的 scope 尚未結束
        // 該值已經在 stack 上被追蹤，所以可以安全跳過
        if let Ok(borrowed) = self.inner.try_borrow() {
            borrowed.trace(visitor);
        }
        // 注意：此方案在某些邊緣情況下可能漏標，不推薦
    }
}
```

### 實作步驟

1. 修改 `cell.rs:108-112` 的 `Trace` 實作
2. 加入詳細的 `SAFETY` 註解說明前置條件
3. 新增單元測試驗證 GC 期間不會 panic

### 測試案例

```rust
#[test]
fn test_gc_during_borrow_mut() {
    use crate::{Gc, GcCell};
    
    let cell = Gc::new(GcCell::new(Some(Gc::new(42))));
    
    // 持有 mutable borrow
    let mut borrow = cell.borrow_mut();
    *borrow = Some(Gc::new(100));
    
    // 在 borrow 存活期間觸發 GC（應該不會 panic）
    crate::collect_full();
    
    drop(borrow);
    assert_eq!(**cell.borrow().as_ref().unwrap(), 100);
}
```

### 風險評估

- **破壞性變更：** 無
- **效能影響：** 略微提升（減少 borrow 檢查開銷）
- **相容性：** 完全向後相容

---

## Issue #2: Two-Phase Sweep（兩階段清除）

### 問題摘要

目前的 sweep 實作是「單次遍歷」—— 在同一個迴圈中同時執行 `drop_fn` 和回收記憶體，可能導致 Use-After-Free。

### 影響範圍

- **檔案：** `crates/rudo-gc/src/gc.rs`
- **函式：** `copy_sweep_logic()`, `sweep_large_objects()`
- **行號：** 876-934, 982-1064
- **嚴重度：** Critical（可能導致記憶體損壞）

### 現有程式碼流程

```
for each object:
    if !marked && allocated:
        drop_fn(obj)          // ← Phase 1: Finalize
        clear_allocated(i)    // ← Phase 2: Reclaim（同步執行）
        add_to_free_list()
```

### 修復方案

將 Sweep 分為兩個獨立階段：

#### Phase 1: Finalization（收集待清除物件並執行 Drop）

```rust
/// 待清除物件的資訊
struct PendingDrop {
    page: NonNull<PageHeader>,
    index: usize,
    drop_fn: unsafe fn(*mut u8),
    obj_ptr: *mut u8,
}

/// Phase 1: 收集所有待清除物件並執行 Drop
fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    let mut pending = Vec::new();
    
    for page_ptr in heap.all_pages() {
        unsafe {
            let header = page_ptr.as_ptr();
            
            // Skip conditions...
            if ((*header).flags & 0x01) != 0 { continue; } // large obj
            if only_young && (*header).generation > 0 { continue; }
            
            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);
            
            for i in 0..obj_count {
                if !(*header).is_marked(i) && (*header).is_allocated(i) {
                    let obj_ptr = (header as *mut u8).add(header_size + i * block_size);
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                    
                    let weak_count = (*gc_box_ptr).weak_count();
                    
                    if weak_count > 0 {
                        // 有 weak ref：只 drop value，保留 allocation
                        if !(*gc_box_ptr).is_value_dead() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                            (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                            (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                            (*gc_box_ptr).set_dead();
                        }
                    } else {
                        // 無 weak ref：記錄待回收
                        pending.push(PendingDrop {
                            page: page_ptr,
                            index: i,
                            drop_fn: (*gc_box_ptr).drop_fn,
                            obj_ptr,
                        });
                        
                        // 先執行 drop_fn（但不回收記憶體）
                        ((*gc_box_ptr).drop_fn)(obj_ptr);
                    }
                }
            }
        }
    }
    
    pending
}
```

#### Phase 2: Reclamation（回收記憶體）

```rust
/// Phase 2: 回收記憶體到 Free List
fn sweep_phase2_reclaim(pending: Vec<PendingDrop>) -> usize {
    let mut reclaimed = 0;
    
    // 按 Page 分組處理，優化快取局部性
    let mut by_page: HashMap<usize, Vec<(usize, *mut u8)>> = HashMap::new();
    for p in pending {
        by_page
            .entry(p.page.as_ptr() as usize)
            .or_default()
            .push((p.index, p.obj_ptr));
    }
    
    for (page_addr, slots) in by_page {
        unsafe {
            let header = page_addr as *mut PageHeader;
            let block_size = (*header).block_size as usize;
            
            // 重建 Free List（按 index 降序）
            let mut free_head = (*header).free_list_head;
            
            for (index, obj_ptr) in slots.into_iter().rev() {
                (*header).clear_allocated(index);
                
                let obj_cast = obj_ptr.cast::<Option<u16>>();
                *obj_cast = free_head;
                free_head = Some(index as u16);
                
                reclaimed += 1;
            }
            
            (*header).free_list_head = free_head;
        }
    }
    
    // 更新全域計數器
    N_EXISTING.with(|n| n.set(n.get().saturating_sub(reclaimed)));
    
    reclaimed
}
```

#### 整合修改

```rust
/// 修改後的 sweep_segment_pages
fn sweep_segment_pages(heap: &LocalHeap, only_young: bool) -> usize {
    // Phase 1: 執行所有 Drop
    let pending = sweep_phase1_finalize(heap, only_young);
    
    // Phase 2: 回收記憶體
    sweep_phase2_reclaim(pending)
}
```

### 實作步驟

1. 新增 `PendingDrop` 結構體
2. 將 `copy_sweep_logic` 重構為 `sweep_phase1_finalize`
3. 新增 `sweep_phase2_reclaim` 函式
4. 修改 `sweep_segment_pages` 使用兩階段流程
5. 同步修改 `sweep_large_objects` 使用相同模式
6. 新增整合測試驗證 Drop 期間不會 UAF

### 測試案例

```rust
#[test]
fn test_drop_accesses_other_gc_object() {
    use std::cell::Cell;
    use crate::{Gc, Trace};
    
    thread_local! {
        static DROP_COUNT: Cell<usize> = Cell::new(0);
    }
    
    #[derive(Trace)]
    struct DropChecker {
        // 故意持有另一個 Gc，在 Drop 時存取它
        other: Option<Gc<i32>>,
    }
    
    impl Drop for DropChecker {
        fn drop(&mut self) {
            if let Some(ref other) = self.other {
                // 在 Drop 期間存取其他 Gc 物件
                let _ = **other;
            }
            DROP_COUNT.with(|c| c.set(c.get() + 1));
        }
    }
    
    {
        let a = Gc::new(42);
        let checker = Gc::new(DropChecker { other: Some(a.clone()) });
        drop(a);
        drop(checker);
    }
    
    crate::collect_full();
    
    // 兩個物件都應該被正確清除，不會 panic
    assert!(DROP_COUNT.with(|c| c.get()) >= 1);
}
```

### 風險評估

- **破壞性變更：** 無（內部實作細節）
- **效能影響：** 輕微增加（需要兩次遍歷 + Vec 分配）
- **記憶體影響：** 暫時增加（pending drop 清單）

---

## Issue #3: Orphan Pages（孤兒頁面機制）

### 問題摘要

當執行緒結束時，其 `LocalHeap` 直接 unmap 所有 Pages，但其他執行緒可能持有指向這些 Pages 的指標。

### 影響範圍

- **檔案：** `crates/rudo-gc/src/heap.rs`
- **函式：** `LocalHeap::drop()`
- **行號：** 1302-1347
- **嚴重度：** Critical（跨執行緒 SIGSEGV）

### 現有程式碼

```rust
impl Drop for LocalHeap {
    fn drop(&mut self) {
        for page_ptr in &self.pages {
            // ... 直接 unmap ...
            sys_alloc::Mmap::from_raw(header.cast::<u8>(), alloc_size);
        }
    }
}
```

### 修復方案

#### Step 1: 擴展 GlobalSegmentManager

```rust
pub struct GlobalSegmentManager {
    // ... 現有欄位 ...
    
    /// 孤兒頁面：執行緒終止後尚未回收的頁面
    /// Key: 頁面起始位址, Value: (Mmap, owner_thread_id, is_large)
    pub orphan_pages: Vec<OrphanPage>,
}

pub struct OrphanPage {
    /// 頁面起始位址
    pub addr: usize,
    /// 頁面大小（可能多頁）
    pub size: usize,
    /// 是否為 Large Object
    pub is_large: bool,
    /// 原始 owner 的 thread id（用於除錯）
    pub original_owner: std::thread::ThreadId,
}
```

#### Step 2: 修改 LocalHeap::drop

```rust
impl Drop for LocalHeap {
    fn drop(&mut self) {
        let current_thread = std::thread::current().id();
        
        // 不直接 unmap，而是將頁面轉移給 GlobalSegmentManager
        let mut manager = segment_manager()
            .lock()
            .expect("segment manager lock poisoned");
        
        for page_ptr in std::mem::take(&mut self.pages) {
            unsafe {
                let header = page_ptr.as_ptr();
                
                // 檢查頁面是否有效
                if (*header).magic != MAGIC_GC_PAGE {
                    continue;
                }
                
                let is_large = ((*header).flags & 0x01) != 0;
                let block_size = (*header).block_size as usize;
                let header_size = (*header).header_size as usize;
                
                let size = if is_large {
                    let total = header_size + block_size;
                    total.div_ceil(PAGE_SIZE) * PAGE_SIZE
                } else {
                    PAGE_SIZE
                };
                
                // 標記頁面為孤兒狀態
                (*header).flags |= 0x02; // ORPHAN_FLAG
                
                manager.orphan_pages.push(OrphanPage {
                    addr: page_ptr.as_ptr() as usize,
                    size,
                    is_large,
                    original_owner: current_thread,
                });
            }
        }
        
        // 清理本地 large_object_map（全域 map 保留給 GC 使用）
        self.large_object_map.clear();
        self.small_pages.clear();
    }
}
```

#### Step 3: GC 處理孤兒頁面

```rust
/// 在 Major GC 結束後處理孤兒頁面
fn sweep_orphan_pages() {
    let mut manager = segment_manager().lock().unwrap();
    
    // 收集可回收的孤兒頁面
    let mut to_reclaim = Vec::new();
    
    manager.orphan_pages.retain(|orphan| {
        unsafe {
            let header = orphan.addr as *mut PageHeader;
            
            // 檢查頁面是否還有存活物件
            let has_survivors = if orphan.is_large {
                (*header).is_marked(0)
            } else {
                // 檢查所有 slots
                let obj_count = (*header).obj_count as usize;
                (0..obj_count).any(|i| (*header).is_marked(i))
            };
            
            if has_survivors {
                // 有存活物件，保留頁面
                (*header).clear_all_marks();
                true
            } else {
                // 無存活物件，標記為可回收
                to_reclaim.push((orphan.addr, orphan.size));
                false
            }
        }
    });
    
    // 釋放鎖後再 unmap（避免長時間持有鎖）
    drop(manager);
    
    for (addr, size) in to_reclaim {
        // 先執行所有 drop_fn
        unsafe {
            let header = addr as *mut PageHeader;
            let is_large = ((*header).flags & 0x01) != 0;
            
            if is_large {
                let header_size = (*header).header_size as usize;
                let obj_ptr = (addr as *mut u8).add(header_size);
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                if !(*gc_box_ptr).is_value_dead() {
                    ((*gc_box_ptr).drop_fn)(obj_ptr);
                }
            } else {
                // Small object page: iterate all allocated slots
                let block_size = (*header).block_size as usize;
                let obj_count = (*header).obj_count as usize;
                let header_size = PageHeader::header_size(block_size);
                
                for i in 0..obj_count {
                    if (*header).is_allocated(i) {
                        let obj_ptr = (addr as *mut u8).add(header_size + i * block_size);
                        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                        if !(*gc_box_ptr).is_value_dead() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                        }
                    }
                }
            }
            
            // 最後 unmap
            sys_alloc::Mmap::from_raw(addr as *mut u8, size);
        }
    }
}
```

#### Step 4: 整合到 GC 流程

```rust
fn perform_multi_threaded_collect() {
    // ... 現有的 mark/sweep 邏輯 ...
    
    // 在 Major GC 結束時處理孤兒頁面
    if total_size > MAJOR_THRESHOLD {
        sweep_orphan_pages();
    }
    
    // ... resume threads ...
}
```

### 新增常數

```rust
// heap.rs
/// Flag: Page 是孤兒頁面（owner thread 已終止）
pub const PAGE_FLAG_ORPHAN: u8 = 0x02;
```

### 實作步驟

1. 在 `PageHeader.flags` 定義 `ORPHAN_FLAG (0x02)`
2. 新增 `OrphanPage` 結構體和相關欄位到 `GlobalSegmentManager`
3. 修改 `LocalHeap::drop` 實作孤兒轉移邏輯
4. 新增 `sweep_orphan_pages()` 函式
5. 在 Major GC 流程中整合 `sweep_orphan_pages()`
6. 修改 `find_gc_box_from_ptr` 支援孤兒頁面查找

### 測試案例

> **注意：** 由於 `Gc<T>` 的設計不是 `Send` 或 `Sync`，無法在執行緒間共享。
> 跨執行緒引用測試需要使用 `sync::Gc`，這超出了當前實作範圍。

```rust
#[test]
fn test_cross_thread_reference_survives_thread_death() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    
    // 注意：此測試需要 sync::Gc，暫時無法實作
    // 因為 rudo-gc 目前只支援單執行緒的 Gc<T>
}
```

### 風險評估

- **破壞性變更：** 無
- **效能影響：** 輕微增加（孤兒頁面管理開銷）
- **記憶體影響：** 可能延遲回收（等到下次 Major GC）

---

## Issue #4: Atomic Mark Bitmap

### 問題摘要

`mark_bitmap`, `dirty_bitmap`, `allocated_bitmap` 使用普通 `[u64; 4]`，在多執行緒並發標記時可能遺失 bit。

### 影響範圍

- **檔案：** `crates/rudo-gc/src/heap.rs`
- **結構：** `PageHeader`
- **行號：** 398-530
- **嚴重度：** High（可能導致物件被錯誤回收）

### 現有程式碼

```rust
pub struct PageHeader {
    // ...
    pub mark_bitmap: [u64; 4],
    pub dirty_bitmap: [u64; 4],
    pub allocated_bitmap: [u64; 4],
    // ...
}

impl PageHeader {
    pub const fn set_mark(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word] |= 1 << bit;  // ← 非原子
    }
}
```

### 修復方案

#### Step 1: 修改 PageHeader 結構

```rust
use std::sync::atomic::{AtomicU64, Ordering};

#[repr(C)]
pub struct PageHeader {
    /// Magic number to validate this is a GC page.
    pub magic: u32,
    /// Size of each object slot in bytes.
    pub block_size: u32,
    /// Maximum number of objects in this page.
    pub obj_count: u16,
    /// Offset from the start of the page to the first object.
    pub header_size: u16,
    /// Generation index.
    pub generation: u8,
    /// Bitflags.
    pub flags: u8,
    /// Padding for alignment.
    _padding: [u8; 2],
    
    /// Bitmap of marked objects (atomic for concurrent marking).
    pub mark_bitmap: [AtomicU64; 4],
    /// Bitmap of dirty objects (atomic for concurrent write barriers).
    pub dirty_bitmap: [AtomicU64; 4],
    /// Bitmap of allocated objects (non-atomic, only modified by owner thread).
    pub allocated_bitmap: [u64; 4],
    
    /// Index of first free slot in free list.
    pub free_list_head: Option<u16>,
}
```

**注意：** `allocated_bitmap` 保持非原子，因為只有 owner thread 會修改它。

#### Step 2: 修改 Bitmap 操作方法

```rust
impl PageHeader {
    // ========== Mark Bitmap (Atomic) ==========
    
    /// Check if an object at the given index is marked.
    #[must_use]
    pub fn is_marked(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.mark_bitmap[word].load(Ordering::Acquire) & (1 << bit)) != 0
    }
    
    /// Set the mark bit for an object (atomic, suitable for concurrent marking).
    /// Returns true if the bit was newly set (not previously marked).
    pub fn set_mark(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        let mask = 1u64 << bit;
        let old = self.mark_bitmap[word].fetch_or(mask, Ordering::AcqRel);
        (old & mask) == 0
    }
    
    /// Clear the mark bit for an object.
    pub fn clear_mark(&self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word].fetch_and(!(1u64 << bit), Ordering::Release);
    }
    
    /// Clear all mark bits.
    pub fn clear_all_marks(&self) {
        for word in &self.mark_bitmap {
            word.store(0, Ordering::Release);
        }
    }
    
    // ========== Dirty Bitmap (Atomic) ==========
    
    /// Check if an object at the given index is dirty.
    #[must_use]
    pub fn is_dirty(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.dirty_bitmap[word].load(Ordering::Acquire) & (1 << bit)) != 0
    }
    
    /// Set the dirty bit for an object (atomic, called from write barrier).
    pub fn set_dirty(&self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word].fetch_or(1u64 << bit, Ordering::Release);
    }
    
    /// Clear the dirty bit for an object.
    pub fn clear_dirty(&self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word].fetch_and(!(1u64 << bit), Ordering::Release);
    }
    
    /// Clear all dirty bits.
    pub fn clear_all_dirty(&self) {
        for word in &self.dirty_bitmap {
            word.store(0, Ordering::Release);
        }
    }
    
    // ========== Allocated Bitmap (Non-Atomic) ==========
    // 保持原有實作，因為只有 owner thread 會修改
    
    #[must_use]
    pub const fn is_allocated(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.allocated_bitmap[word] & (1 << bit)) != 0
    }
    
    pub fn set_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word] |= 1 << bit;
    }
    
    pub fn clear_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word] &= !(1 << bit);
    }
    
    pub fn clear_all_allocated(&mut self) {
        self.allocated_bitmap = [0; 4];
    }
}
```

#### Step 3: 修改 PageHeader 初始化

```rust
// heap.rs: alloc_slow
unsafe {
    header.as_ptr().write(PageHeader {
        magic: MAGIC_GC_PAGE,
        block_size: block_size as u32,
        obj_count: obj_count as u16,
        header_size: h_size as u16,
        generation: 0,
        flags: 0,
        _padding: [0; 2],
        mark_bitmap: [
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        ],
        dirty_bitmap: [
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
            AtomicU64::new(0),
        ],
        allocated_bitmap: [0; 4],
        free_list_head: None,
    });
}
```

#### Step 4: 更新方法簽名

由於 `set_mark` 等方法不再需要 `&mut self`，需要更新所有呼叫處：

```rust
// 修改前
(*header.as_ptr()).set_mark(idx);

// 修改後（不變，因為我們使用的是 &self）
(*header.as_ptr()).set_mark(idx);
```

**關鍵變更：** 方法從 `&mut self` 變為 `&self`，這允許並發呼叫。

### Memory Ordering 選擇理由

| 操作 | Ordering | 理由 |
|------|----------|------|
| `is_marked` load | `Acquire` | 確保看到其他執行緒的最新標記 |
| `set_mark` fetch_or | `AcqRel` | 確保標記可見性，且返回值正確 |
| `clear_mark` fetch_and | `Release` | 確保清除可見於後續讀取 |
| `set_dirty` fetch_or | `Release` | 確保 write barrier 可見於 GC |

### 實作步驟

1. 將 `mark_bitmap` 和 `dirty_bitmap` 改為 `[AtomicU64; 4]`
2. 修改所有 bitmap 操作方法的簽名和實作
3. 更新所有 `PageHeader` 初始化程式碼
4. 移除不再需要的 `&mut` 借用
5. 更新 `const fn` 標記（原子操作不能是 const）
6. 執行壓力測試驗證並發安全性

### 測試案例

> **注意：** 此測試無法實作，因為 `Gc<T>` 不是 `Send`，無法在執行緒間傳遞。
> 並發標記需要 `sync::Gc` 或其他同步機制，這超出了當前實作範圍。

```rust
#[test]
fn test_concurrent_marking() {
    use std::sync::{Arc, Barrier};
    use std::thread;
    
    // 注意：此測試需要跨執行緒共享 Gc 物件，但 Gc<T> 不是 Send
    // 無法使用 thread::spawn 傳遞 Gc 物件
}
```

### 風險評估

- **破壞性變更：** API 變更（`&mut self` → `&self`）
- **效能影響：** 原子操作有少許開銷，但確保正確性
- **相容性：** 需要更新所有呼叫點

---

## 修復順序與時程建議

| 優先級 | Issue | 預估工時 | 依賴關係 |
|--------|-------|---------|---------|
| 1 | #1 GcCell::Trace | 0.5 天 | 無 |
| 2 | #4 Atomic Bitmap | 1 天 | 無 |
| 3 | #2 Two-Phase Sweep | 2 天 | 無 |
| 4 | #3 Orphan Pages | 2 天 | #2（使用相同的 drop 邏輯） |

**總計：** 約 5.5 工作天

### 測試驗證矩陣

| Issue | 單元測試 | 整合測試 | 壓力測試 | Miri | 說明 |
|-------|---------|---------|---------|------|------|
| #1 | ✓ | ✓ | - | ✓ | `test_gc_during_borrow_mut` |
| #2 | ✓ | ✓ | ✓ | ✓ | `test_drop_accesses_other_gc_object` |
| #3 | ✓ | - | - | - | `test_cross_thread_reference_survives_thread_death` 需要 sync::Gc |
| #4 | ✓ | - | - | ✓ | `test_concurrent_marking` 需要跨執行緒共享 Gc |

> **注意：** Issue #3 和 #4 的多執行緒測試需要 `sync::Gc`，這超出了當前單執行緒 GC 的實作範圍。

---

## 附錄：相關檔案清單

- `crates/rudo-gc/src/cell.rs` - Issue #1
- `crates/rudo-gc/src/gc.rs` - Issue #2
- `crates/rudo-gc/src/heap.rs` - Issue #3, #4
- `crates/rudo-gc/src/ptr.rs` - 受 Issue #2 間接影響
