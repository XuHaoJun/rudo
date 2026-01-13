# Two-Phase Sweep 修復計畫

## 概述

本文件詳細說明 `rudo-gc` 的 Two-Phase Sweep 實作中已確認的問題及其修復方案。問題分析基於 2026-01-13 的 Gemini 審查報告與原始碼交叉驗證。

---

## 問題摘要

| 問題 ID | 嚴重度 | 類型 | 描述 |
|---------|--------|------|------|
| P0-001 | 🔴 Critical | Safety | Sweep 期間的 Iterator Invalidation (UB) |
| P0-002 | 🔴 Critical | Correctness | Orphan Page 死物件未執行 Drop (Memory Leak) |
| P1-001 | 🟡 Medium | Performance | PendingDrop 資料結構開銷過大 |
| P2-001 | 🟢 Low | Design | sweep_large_objects 複雜度 O(N²) |

---

## P0-001: Iterator Invalidation during Sweep Phase 1

### 問題描述

在 `sweep_phase1_finalize` 函數中，程式碼直接迭代 `heap.all_pages()`，同時執行使用者定義的 `drop_fn`。如果 `drop_fn` 內部進行記憶體配置 (例如 `Box::new()`、`Vec::push()` 或 `Gc::new()`)，將觸發 `alloc_slow`，進而呼叫 `heap.pages.push(new_page)`。

這違反了 Rust 的迭代器安全性規則：**在迭代期間修改底層容器會導致未定義行為 (Undefined Behavior)**。

### 影響位置

```
crates/rudo-gc/src/gc.rs:876-932 (sweep_phase1_finalize)
crates/rudo-gc/src/gc.rs:1046-1120 (sweep_large_objects)
```

### 程式碼分析

**問題程式碼 (`gc.rs:876-932`):**

```rust
fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    let mut pending = Vec::new();

    // [問題] 直接迭代 heap.pages
    for page_ptr in heap.all_pages() {
        unsafe {
            // ...
            // [危險] drop_fn 可能觸發 allocation
            ((*gc_box_ptr).drop_fn)(obj_ptr);
            // ...
        }
    }
    pending
}
```

**`all_pages` 實作 (`heap.rs:1257-1260`):**

```rust
pub fn all_pages(&self) -> impl Iterator<Item = NonNull<PageHeader>> + '_ {
    self.pages.iter().copied()
}
```

**Allocation 路徑 (`heap.rs:1029-1125`):**

```rust
fn alloc_slow(&mut self, _size: usize, class_index: usize) -> NonNull<u8> {
    // ...
    // 3. Update LocalHeap pages list
    self.pages.push(header);  // <-- 修改迭代中的容器！
    // ...
}
```

### 攻擊場景 (Proof of Concept)

```rust
struct Allocator {
    _data: Vec<u8>,
}

impl Drop for Allocator {
    fn drop(&mut self) {
        // Destructor 中配置記憶體
        let _ = Gc::new(42);  // 觸發 alloc_slow -> pages.push()
    }
}

#[test]
fn test_iterator_invalidation() {
    let gc_obj = Gc::new(Allocator { _data: vec![1, 2, 3] });
    drop(gc_obj);
    collect_full();  // 可能 hang、crash 或 UB
}
```

### UB 後果

1. **Use-After-Free:** `Vec` 擴容時釋放舊 buffer，迭代器持有 dangling pointer
2. **Infinite Loop:** 迭代器讀取垃圾數據，解析為無效的 page header
3. **Segfault:** 訪問已釋放記憶體
4. **Silent Corruption:** 讀取到錯誤的 page 指標，掃描錯誤區域

### 修復方案

**策略: Snapshotting (快照)**

在迭代前將 `heap.pages` 複製到獨立的 `Vec`，確保 `drop_fn` 觸發的 allocation 不會影響迭代。

**修復程式碼:**

```rust
fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) -> Vec<PendingDrop> {
    let mut pending = Vec::new();

    // [FIX] 快照當前所有頁面
    // 如果 drop_fn 配置新 Page，新 Page 會被加入 heap.pages，
    // 但不會影響我們正在迭代的 snapshot。
    // 新 Page 的 generation = 0 (Young)，且 allocated_bitmap 為空或只有新物件，
    // 下次 GC 會正常處理它們。
    let pages_snapshot: Vec<_> = heap.all_pages().collect();

    for page_ptr in pages_snapshot {
        unsafe {
            let header = page_ptr.as_ptr();

            // 跳過 Large Objects (由 sweep_large_objects 處理)
            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {
                continue;
            }

            // 跳過 Old Generation (Minor GC 時)
            if only_young && (*header).generation > 0 {
                continue;
            }

            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);

            for i in 0..obj_count {
                if (*header).is_marked(i) {
                    // 存活物件 - 清除 mark bit
                    (*header).clear_mark(i);
                } else if (*header).is_allocated(i) {
                    // 死物件 - 需要清理
                    let obj_ptr = page_ptr.as_ptr().cast::<u8>();
                    let obj_ptr = obj_ptr.add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                    let weak_count = (*gc_box_ptr).weak_count();

                    if weak_count > 0 {
                        // 有 weak refs - drop value 但保留 allocation
                        if !(*gc_box_ptr).is_value_dead() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                            (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                            (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                            (*gc_box_ptr).set_dead();
                        }
                    } else {
                        // 無 weak refs - 完全回收
                        ((*gc_box_ptr).drop_fn)(obj_ptr);
                        pending.push(PendingDrop {
                            page: page_ptr,
                            index: i,
                        });
                    }
                }
            }
        }
    }

    pending
}
```

**同樣的修復應用於 `sweep_large_objects`:**

```rust
fn sweep_large_objects(heap: &mut LocalHeap, only_young: bool) -> usize {
    // [FIX] 快照 Large Object Pages
    let target_pages: Vec<_> = heap.large_object_pages();  // 已經是新 Vec

    let mut to_deallocate: Vec<(NonNull<PageHeader>, usize, usize)> = Vec::new();

    // ... (現有邏輯保持不變，因為 large_object_pages() 回傳的是獨立 Vec)
}
```

### 測試計畫

```rust
#[test]
fn test_drop_allocates() {
    use std::cell::Cell;
    
    thread_local! {
        static DROP_COUNT: Cell<usize> = const { Cell::new(0) };
    }
    
    struct AllocatingDropper;
    
    unsafe impl Trace for AllocatingDropper {
        fn trace(&self, _: &mut impl Visitor) {}
    }
    
    impl Drop for AllocatingDropper {
        fn drop(&mut self) {
            // 在 drop 中配置新物件
            let _ = Gc::new(12345i32);
            DROP_COUNT.with(|c| c.set(c.get() + 1));
        }
    }
    
    // 配置多個物件來增加觸發 Vec reallocation 的機會
    for _ in 0..100 {
        let _obj = Gc::new(AllocatingDropper);
    }
    
    collect_full();
    
    // 驗證所有 dropper 都被正常 drop
    assert!(DROP_COUNT.with(Cell::get) >= 100);
}
```

---

## P0-002: Orphan Page 死物件未執行 Drop

### 問題描述

當一個執行緒終止時，其 `LocalHeap` 擁有的頁面會被標記為 "Orphan" 並移交給 `GlobalSegmentManager`。在後續的 GC 中，`sweep_orphan_pages` 函數會處理這些頁面。

**問題在於：** 如果 Orphan Page 中有任何存活物件 (`has_survivors == true`)，整個頁面會被保留，但頁面內的**死物件永遠不會執行 `drop_fn`**。

### 影響

1. **資源洩漏:** File handles、Sockets、Database connections 等不會被釋放
2. **記憶體洩漏:** 死物件佔用的 slots 永遠無法被重新使用
3. **Finalizers 失效:** 依賴 `Drop` 進行清理的邏輯不會執行

### 影響位置

```
crates/rudo-gc/src/heap.rs:1405-1464 (sweep_orphan_pages)
```

### 程式碼分析

**問題程式碼 (`heap.rs:1410-1427`):**

```rust
manager.orphan_pages.retain(|orphan| unsafe {
    let header = orphan.addr as *mut PageHeader;

    let has_survivors = if orphan.is_large {
        (*header).is_marked(0)
    } else {
        let obj_count = (*header).obj_count as usize;
        (0..obj_count).any(|i| (*header).is_marked(i))
    };

    if has_survivors {
        (*header).clear_all_marks();
        true  // [問題] 保留頁面，但死物件沒有 drop！
    } else {
        to_reclaim.push((orphan.addr, orphan.size));
        false
    }
});
```

### 攻擊場景

**場景：Thread A 終止，Thread B 仍引用 Thread A 的某個物件**

1. Thread A 配置了 100 個物件：`obj_0, obj_1, ..., obj_99`
2. Thread B 持有對 `obj_0` 的引用
3. Thread A 終止，所有頁面變成 Orphan
4. GC 執行：
   - `obj_0` 被 Thread B 標記為存活
   - `has_survivors = true`
   - **整頁被保留**
5. `obj_1` 到 `obj_99` 的 `drop` **永遠不會被呼叫**

**影響範例:**

```rust
struct FileHolder {
    file: std::fs::File,
}

impl Drop for FileHolder {
    fn drop(&mut self) {
        // 這個 drop 永遠不會被呼叫！
        // File handle 洩漏，可能導致 "too many open files" 錯誤
    }
}
```

### 修復方案

**策略: 完整的 Orphan Page Sweep**

對 Orphan Page 執行與一般頁面相同的 Two-Phase Sweep：
1. Phase 1: 對所有未標記的物件執行 `drop_fn`
2. Phase 2: 清除 allocated bit，更新 free list (可選，因為 Orphan Page 暫時無法被 allocator 使用)

**修復程式碼:**

```rust
/// Sweep and reclaim orphan pages.
/// 
/// 對 Orphan Pages 執行完整的 Two-Phase Sweep：
/// - Phase 1: 對死物件執行 drop_fn
/// - Phase 2: 如果整頁無存活物件，回收整頁；否則清理死物件 slot
///
/// # Panics
///
/// Panics if the segment manager lock is poisoned.
pub fn sweep_orphan_pages() {
    let mut manager = segment_manager().lock().unwrap();

    let mut to_reclaim_full: Vec<(usize, usize)> = Vec::new();
    
    // [FIX] Two-Phase Sweep for Orphan Pages
    
    // Phase 1: Execute drop_fn for all dead objects
    for orphan in &manager.orphan_pages {
        unsafe {
            let header = orphan.addr as *mut PageHeader;
            
            if orphan.is_large {
                // Large Object: 只有一個物件 (index 0)
                if !(*header).is_marked(0) && (*header).is_allocated(0) {
                    let header_size = (*header).header_size as usize;
                    let obj_ptr = (orphan.addr as *mut u8).add(header_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                    
                    if !(*gc_box_ptr).is_value_dead() {
                        ((*gc_box_ptr).drop_fn)(obj_ptr);
                        (*gc_box_ptr).drop_fn = crate::ptr::GcBox::<()>::no_op_drop;
                        (*gc_box_ptr).trace_fn = crate::ptr::GcBox::<()>::no_op_trace;
                        (*gc_box_ptr).set_dead();
                    }
                }
            } else {
                // Small Object Page: 迭代所有物件
                let block_size = (*header).block_size as usize;
                let obj_count = (*header).obj_count as usize;
                let header_size = PageHeader::header_size(block_size);
                
                for i in 0..obj_count {
                    // [FIX] 對每個死物件執行 drop，不只是檢查 has_survivors
                    if !(*header).is_marked(i) && (*header).is_allocated(i) {
                        let obj_ptr = (orphan.addr as *mut u8).add(header_size + i * block_size);
                        #[allow(clippy::cast_ptr_alignment)]
                        let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                        
                        if !(*gc_box_ptr).is_value_dead() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                            (*gc_box_ptr).drop_fn = crate::ptr::GcBox::<()>::no_op_drop;
                            (*gc_box_ptr).trace_fn = crate::ptr::GcBox::<()>::no_op_trace;
                            // 注意：我們不設 dead flag，因為後面要清除 allocated bit
                        }
                        
                        // 清除 allocated bit (Phase 2 的一部分，這裡合併)
                        (*header).clear_allocated(i);
                    }
                }
            }
        }
    }
    
    // Phase 2: 決定保留或回收頁面
    manager.orphan_pages.retain(|orphan| unsafe {
        let header = orphan.addr as *mut PageHeader;

        let has_survivors = if orphan.is_large {
            (*header).is_marked(0)
        } else {
            // 檢查是否還有任何已配置的物件 (包括存活的)
            let obj_count = (*header).obj_count as usize;
            (0..obj_count).any(|i| (*header).is_allocated(i))
        };

        if has_survivors {
            // 還有存活物件 - 保留頁面，清除 mark bits
            (*header).clear_all_marks();
            true
        } else {
            // 沒有任何物件了 - 回收整頁
            to_reclaim_full.push((orphan.addr, orphan.size));
            false
        }
    });

    drop(manager);

    // Reclaim pages with no survivors
    for (addr, size) in to_reclaim_full {
        unsafe {
            sys_alloc::Mmap::from_raw(addr as *mut u8, size);
        }
    }
}
```

### 設計考量

**Q: 為什麼不把 Orphan Page 的空閒 slots 加入 Free List？**

A: Orphan Pages 不屬於任何執行緒的 `LocalHeap`，因此：
1. 無法被 TLAB 使用
2. 無法被 `alloc_from_free_list` 找到
3. 若要重用，需要額外的 "Orphan Page 認養機制"

**目前的決定：** 暫不實作認養機制。死物件的 slot 被清理 (`drop` 執行 + `allocated_bit` 清除)，但不會被重用，直到整頁沒有存活物件時才回收。這避免了複雜的頁面所有權轉移邏輯，且對大多數工作負載已足夠。

**未來改進 (P2)：** 
- 實作 "Page Adoption"：當 Orphan Page 完全空閒 slots 達到閾值時，可以被活躍執行緒認養
- 或者實作 "Global Free Page Pool"：活躍執行緒可以從 pool 中取得空閒的 Orphan Page

### 測試計畫

```rust
#[test]
fn test_orphan_page_drop() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    
    static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);
    
    struct DropCounter(i32);
    
    unsafe impl Trace for DropCounter {
        fn trace(&self, _: &mut impl Visitor) {}
    }
    
    impl Drop for DropCounter {
        fn drop(&mut self) {
            DROP_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }
    
    let survivor = Arc::new(std::sync::Mutex::new(None::<Gc<i32>>));
    let survivor_clone = survivor.clone();
    
    let handle = std::thread::spawn(move || {
        // 在子執行緒配置物件
        let keep = Gc::new(42i32);
        *survivor_clone.lock().unwrap() = Some(keep);
        
        // 這些物件在執行緒終止後應該被 drop
        for i in 0..50 {
            let _ = Gc::new(DropCounter(i));
        }
        // 執行緒終止，pages 變成 orphan
    });
    
    handle.join().unwrap();
    
    // 確保 survivor 仍被持有
    assert!(survivor.lock().unwrap().is_some());
    
    // 觸發 GC，應該 drop 50 個 DropCounter
    collect_full();
    
    // 所有 DropCounter 都應該被 drop
    assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 50);
    
    // 釋放 survivor，再 GC，整頁應該被回收
    *survivor.lock().unwrap() = None;
    collect_full();
}
```

---

## P1-001: PendingDrop 資料結構開銷

### 問題描述

目前的 Two-Phase Sweep 使用以下流程：

1. **Phase 1:** 遍歷所有頁面，記錄死物件到 `Vec<PendingDrop>`
2. **中間處理:** 將 `Vec<PendingDrop>` 轉換成 `HashMap<PageAddr, Vec<Index>>`
3. **Phase 2:** 再次遍歷頁面，根據 `HashMap` 查表來重建 free list

這產生了不必要的記憶體開銷，尤其在 GC 壓力大時更為諷刺。

### 影響位置

```
crates/rudo-gc/src/gc.rs:938-1001 (sweep_phase2_reclaim)
```

### 優化方案

**策略: 利用現有 Bitmap 避免側邊資料結構**

在 Phase 1 執行 `drop_fn` 後，**不要**立即清除 `allocated_bit`。在 Phase 2 透過檢查 `is_allocated && !is_marked` 來識別需要回收的 slots。

**優化程式碼 (Zero-Alloc Phase 2):**

```rust
fn sweep_phase1_finalize(heap: &LocalHeap, only_young: bool) {
    // 快照頁面
    let pages_snapshot: Vec<_> = heap.all_pages().collect();

    for page_ptr in pages_snapshot {
        unsafe {
            let header = page_ptr.as_ptr();

            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {
                continue;
            }

            if only_young && (*header).generation > 0 {
                continue;
            }

            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);

            for i in 0..obj_count {
                if (*header).is_marked(i) {
                    // 存活 - 清除 mark (為下次 GC 準備)
                    (*header).clear_mark(i);
                } else if (*header).is_allocated(i) {
                    // 死亡 - 執行 drop，但 **不清除 allocated bit**
                    let obj_ptr = page_ptr.as_ptr().cast::<u8>();
                    let obj_ptr = obj_ptr.add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();

                    let weak_count = (*gc_box_ptr).weak_count();

                    if weak_count > 0 {
                        if !(*gc_box_ptr).is_value_dead() {
                            ((*gc_box_ptr).drop_fn)(obj_ptr);
                            (*gc_box_ptr).drop_fn = GcBox::<()>::no_op_drop;
                            (*gc_box_ptr).trace_fn = GcBox::<()>::no_op_trace;
                            (*gc_box_ptr).set_dead();
                        }
                        // 有 weak refs 的物件保持 allocated
                    }
                    // 無 weak refs 的物件在 Phase 2 處理 (透過 allocated && !marked 識別)
                }
            }
        }
    }
}

fn sweep_phase2_reclaim(heap: &LocalHeap, only_young: bool) -> usize {
    let mut reclaimed = 0;

    // 再次快照 (或可重用 Phase 1 的快照)
    let pages_snapshot: Vec<_> = heap.all_pages().collect();

    for page_ptr in pages_snapshot {
        unsafe {
            let header = page_ptr.as_ptr();

            if (*header).flags & crate::heap::PAGE_FLAG_LARGE != 0 {
                continue;
            }

            if only_young && (*header).generation > 0 {
                continue;
            }

            let block_size = (*header).block_size as usize;
            let obj_count = (*header).obj_count as usize;
            let header_size = PageHeader::header_size(block_size);
            let page_addr = header.cast::<u8>();

            // 重建 Free List + 清除死物件的 allocated bit
            let mut free_head: Option<u16> = None;
            
            for i in (0..obj_count).rev() {
                let is_alloc = (*header).is_allocated(i);
                let is_marked = (*header).is_marked(i);  // 應該都是 0 (Phase 1 清除了)
                
                // 死物件：已配置但未標記 (Phase 1 已 drop，但 allocated bit 還在)
                // 或本來就是空閒 slot
                let is_dead_or_free = !is_alloc || (is_alloc && !is_marked);
                
                // 更精確：如果 weak_count > 0，該物件在 Phase 1 被 set_dead() 
                // 但 allocated bit 沒被清... 這裡需要更精細的邏輯
                
                // [簡化版] 直接檢查 is_value_dead
                if is_alloc {
                    let obj_ptr = page_addr.add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                    
                    let weak_count = (*gc_box_ptr).weak_count();
                    
                    if weak_count == 0 && (*gc_box_ptr).is_value_dead() {
                        // 無 weak refs 且已 dead -> 回收
                        (*header).clear_allocated(i);
                        reclaimed += 1;
                    } else if weak_count > 0 {
                        // 有 weak refs -> 保持 allocated
                        continue;
                    } else if (*gc_box_ptr).drop_fn as usize == GcBox::<()>::no_op_drop as usize {
                        // drop_fn 被設為 no_op -> 已經 drop 過，回收
                        (*header).clear_allocated(i);
                        reclaimed += 1;
                    }
                }

                // 加入 free list
                if !(*header).is_allocated(i) {
                    let obj_ptr = page_addr.add(header_size + i * block_size);
                    #[allow(clippy::cast_ptr_alignment)]
                    let obj_cast = obj_ptr.cast::<Option<u16>>();
                    *obj_cast = free_head;
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        free_head = Some(i as u16);
                    }
                }
            }
            (*header).free_list_head = free_head;
        }
    }

    N_EXISTING.with(|n| n.set(n.get().saturating_sub(reclaimed)));
    reclaimed
}
```

### 預期效益

- **記憶體減少:** 移除 `Vec<PendingDrop>` 和 `HashMap<usize, Vec<usize>>`
- **GC Latency 降低:** 減少動態分配次數
- **複雜度不變:** 仍是 O(N) where N = 物件數量

### 優先級

🟡 **Medium** - 功能正確性不受影響，但在大型堆積上可能造成明顯的 GC pause。建議在 P0 問題修復後實施。

---

## P2-001: sweep_large_objects 的 O(N²) 複雜度

### 問題描述

在 `sweep_large_objects` 中，每回收一個 Large Object 就呼叫一次 `heap.pages.retain(|&p| p != page_ptr)`，這是 O(N) 操作。如果有 M 個 Large Objects 要回收，總複雜度是 O(M*N)。

### 影響位置

```
crates/rudo-gc/src/gc.rs:1096 (heap.pages.retain)
```

### 優化方案

收集所有要刪除的 page addresses 到 `HashSet`，然後做一次 `retain`：

```rust
let pages_to_remove: HashSet<_> = to_deallocate
    .iter()
    .map(|(page_ptr, _, _)| page_ptr.as_ptr() as usize)
    .collect();

heap.pages.retain(|&p| !pages_to_remove.contains(&(p.as_ptr() as usize)));
```

### 優先級

🟢 **Low** - 僅在擁有大量 Large Objects 時才會明顯。

---

## 實施計畫

### Phase 1: Critical Fixes (P0)

| 任務 | 負責 | 預估時間 | 狀態 |
|------|------|----------|------|
| 修復 `sweep_phase1_finalize` Iterator Invalidation | TBD | 1h | ⬜ |
| 修復 `sweep_large_objects` Iterator Invalidation | TBD | 30m | ⬜ |
| 重寫 `sweep_orphan_pages` 執行死物件 drop | TBD | 2h | ⬜ |
| 新增 regression tests | TBD | 2h | ⬜ |
| 多執行緒壓力測試 | TBD | 2h | ⬜ |

### Phase 2: Optimization (P1)

| 任務 | 負責 | 預估時間 | 狀態 |
|------|------|----------|------|
| 移除 `PendingDrop` + `HashMap` | TBD | 2h | ⬜ |
| 優化 `sweep_large_objects` retain | TBD | 30m | ⬜ |
| Benchmark 驗證改進 | TBD | 1h | ⬜ |

---

## 附錄: 驗證原始碼的 Checksum

用於確保此文件發布時的參考程式碼版本：

- `gc.rs` Lines 876-932: `sweep_phase1_finalize` 
- `gc.rs` Lines 1046-1120: `sweep_large_objects`
- `heap.rs` Lines 1257-1260: `all_pages`
- `heap.rs` Lines 1405-1464: `sweep_orphan_pages`

---

## 變更歷史

| 日期 | 版本 | 變更描述 |
|------|------|----------|
| 2026-01-13 | 1.0 | 初始版本，基於 Gemini 報告與原始碼交叉驗證 |
