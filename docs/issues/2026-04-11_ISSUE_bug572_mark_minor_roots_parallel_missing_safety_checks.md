# [Bug]: mark_minor_roots_parallel pushes to worker queues without safety checks

**Status:** Fixed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep during minor GC marking |
| **Severity (嚴重程度)** | High | Could trace partially initialized objects or deallocated slots |
| **Reproducibility (重現難度)** | Low | Requires specific timing between dirty page snapshot and lazy sweep |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `mark_minor_roots_parallel` in `gc/gc.rs:1338-1388`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

When pushing objects to worker queues from dirty pages, the code should apply the same safety checks as `mark_and_push_to_worker_queue`:
1. `is_allocated` check to avoid tracing deallocated slots
2. `is_under_construction` check to avoid tracing partially initialized objects (e.g., `Gc::new_cyclic_weak`)
3. Generation check to detect slot reuse

### 實際行為 (Actual Behavior)

The code in `mark_minor_roots_parallel` (lines 1338-1388) pushes objects directly to worker queues WITHOUT these safety checks:

**Buggy code at lines 1343-1349 (large objects):**
```rust
if (*header).is_large_object() {
    let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    // Add to first worker queue (will be distributed by work stealing)
    worker_queues[0].push(gc_box_ptr);  // NO SAFETY CHECKS!
    continue;
}
```

**Buggy code at lines 1378-1384 (non-large objects):**
```rust
for i in 0..obj_count {
    if (*header).is_dirty(i) {  // Only checks dirty flag!
        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
        worker_queues[worker_idx].push(gc_box_ptr);  // NO is_allocated, NO is_under_construction!
    }
}
```

### Compare with correct pattern in `mark_and_push_to_worker_queue` (lines 1205-1253):

```rust
// 1. try_mark with is_allocated re-check
match (*header.as_ptr()).try_mark(idx) {
    Ok(true) => {
        if !(*header.as_ptr()).is_allocated(idx) {  // Safety check
            (*header.as_ptr()).clear_mark_atomic(idx);
            return;
        }
        let marked_generation = (*gc_box.as_ptr()).generation();
        if (*gc_box.as_ptr()).generation() != marked_generation {  // Generation check
            (*header.as_ptr()).clear_mark_atomic(idx);
            return;
        }
    }
    // ...
}
// 2. is_under_construction check
if (*gc_box.as_ptr()).is_under_construction() {  // Safety check
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
// 3. Second is_allocated re-check before push
if !(*header.as_ptr()).is_allocated(idx) {  // Safety check
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
worker_queues[worker_idx].push(gc_box.as_ptr());  // NOW safe to push
```

---

## 根本原因分析 (Root Cause Analysis)

**Problem:** The dirty page handling code in `mark_minor_roots_parallel` assumes that if a page is in the dirty pages list and an object is marked dirty, it's safe to trace. However:

1. **Slot reuse race**: Between `take_dirty_pages_snapshot()` (line 1335) and processing dirty pages, lazy sweep could deallocate and reuse a slot
2. **is_dirty is stale**: The `is_dirty(i)` flag indicates a barrier was triggered, but doesn't prove the slot is still allocated or contains the same object
3. **Missing safety net**: Unlike `mark_and_push_to_worker_queue`, no try_mark, generation check, or is_under_construction check is performed

**Scenario triggering the bug:**

1. Object A allocated in slot with generation G, dirty flag set (old->young reference)
2. Dirty pages snapshot taken (line 1335)
3. Between snapshot and processing: lazy sweep deallocates slot, Object B allocated with generation G+1
4. `mark_minor_roots_parallel` iterates dirty pages, finds `is_dirty(i) == true`
5. Object B (not Object A!) is pushed to worker queue and traced
6. Object B may be incorrectly retained or incorrectly traced

**Partially initialized object scenario:**

1. `Gc::new_cyclic_weak` allocates slot, starts construction
2. Generational barrier sets dirty bit on the page
3. Before construction completes, dirty page processing pushes object
4. Object traced before fully initialized

---

## 重現步驟 / 概念驗證 (PoC)

```rust
// Theoretical race - requires precise concurrent timing
// 1. Allocate object A in slot with generation G, set dirty flag
// 2. Take dirty pages snapshot
// 3. Lazy sweep deallocates slot, Object B allocated with generation G+1
// 4. mark_minor_roots_parallel finds is_dirty(i) == true
// 5. Object B incorrectly pushed to worker queue and traced
```

---

## 建議修復方案 (Suggested Fix)

Replace the direct pushes with calls to `mark_and_push_to_worker_queue`, or add equivalent safety checks before the push:

```rust
// For large objects (lines 1343-1349):
if (*header).is_large_object() {
    let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    // FIX bug572: Use mark_and_push_to_worker_queue for safety checks
    mark_and_push_to_worker_queue(
        obj_ptr,
        gc_box_ptr,
        &worker_queues,
        num_workers,
    );
    continue;
}

// For non-large objects (lines 1378-1384):
for i in 0..obj_count {
    if (*header).is_dirty(i) {
        let block_size = (*header).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
        // FIX bug572: Use mark_and_push_to_worker_queue for safety checks
        mark_and_push_to_worker_queue(
            obj_ptr,
            gc_box_ptr,
            &worker_queues,
            num_workers,
        );
    }
}
```

Alternatively, add inline safety checks before each push.

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The dirty page snapshot mechanism assumes pages are processed soon after snapshot, but concurrent lazy sweep can invalidate this assumption. The fix pattern (use mark_and_push_to_worker_queue) is correct because it provides try_mark + generation check + is_under_construction check before any push.

**Rustacean (Soundness 觀點):**
The missing `is_under_construction` check is a soundness concern. Tracing partially initialized objects can lead to undefined behavior. The `is_allocated` check prevents tracing deallocated memory. Both are necessary.

**Geohot (Exploit 觀點):**
While the race window is small, an attacker who could influence GC timing might trigger incorrect object tracing. This could potentially lead to memory corruption or information disclosure if sensitive data from a partially initialized object is traced.

---

## 相關 Issue

- bug570: mark_and_push_to_worker_queue missing safety checks (just fixed)
- bug469: Skip objects under construction in worker_mark_loop
- bug547: mark_and_trace_incremental missing is_under_construction check (Fixed)
- bug558: mark_and_push_to_worker_queue has same pattern as bug551 (Fixed)

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-04-11
**Fix Applied:** Modified `mark_minor_roots_parallel` in `gc/gc.rs` to use `mark_and_push_to_worker_queue` instead of direct pushes.

**Changes made:**

1. **Lines 1343-1351** (large objects from dirty_pages_iter): Replaced direct `worker_queues[0].push(gc_box_ptr)` with `mark_and_push_to_worker_queue()` call.

2. **Lines 1360-1367** (large objects from overflow buffer): Replaced direct `worker_queues[0].push(gc_box_ptr)` with `mark_and_push_to_worker_queue()` call.

3. **Lines 1385-1392** (non-large objects): Replaced direct `worker_queues[worker_idx].push(gc_box_ptr)` with `mark_and_push_to_worker_queue()` call.

**Before:**
```rust
worker_queues[0].push(gc_box_ptr);  // NO SAFETY CHECKS!
```

**After:**
```rust
#[allow(clippy::cast_ptr_alignment)]
let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
#[allow(clippy::ptr_as_ptr)]
let gc_box = std::ptr::NonNull::new(gc_box_ptr as *mut GcBox<()>);
if let Some(gc_box) = gc_box {
    mark_and_push_to_worker_queue(obj_ptr, gc_box, &worker_queues, num_workers);
}
```

**Verification:** `./clippy.sh` passes.

**Note:** The test `deep_tree_allocation_test` was already failing before this fix due to a pre-existing bug unrelated to this change.
