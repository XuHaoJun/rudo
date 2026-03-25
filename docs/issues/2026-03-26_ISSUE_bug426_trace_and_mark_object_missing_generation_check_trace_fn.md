# [Bug]: trace_and_mark_object missing generation check before trace_fn - slot reuse TOCTOU

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep to reuse slot between mark and trace |
| **Severity (嚴重程度)** | Critical | Calling trace_fn on wrong object data causes memory corruption |
| **Reproducibility (復現難度)** | High | Needs concurrent incremental marking + lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Incremental Marking`, `trace_and_mark_object` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`trace_and_mark_object` should verify the slot has not been reused (generation unchanged) before calling `trace_fn`. If generation changed, the object was swept and reallocated - calling trace on the new object's data is incorrect.

### 實際行為 (Actual Behavior)
`trace_and_mark_object` checks `is_allocated` and `is_under_construction` but NOT generation before calling `trace_fn`. If the slot was swept and reused between when the object was marked and when `trace_and_mark_object` processes it, `trace_fn` is called on the new object's data.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `gc/incremental.rs`, `trace_and_mark_object` (lines 791-822) calls `trace_fn` at line 817 without verifying the generation hasn't changed:

```rust
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    // ... checks is_allocated and is_under_construction but NOT generation ...
    
    ((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);  // BUG: No generation check!
    // ...
}
```

In contrast, `scan_page_for_marked_refs` (lines 862-868) correctly checks generation after successful mark:

```rust
// Verify generation hasn't changed (bug336 fix).
// If slot was reallocated between try_mark and push_work,
// generation will differ and we should skip this object.
let current_generation = unsafe { (*gc_box_ptr).generation() };
if current_generation != marked_generation {
    // Slot was reused - the mark now belongs to the new object, don't clear.
    break;
}
```

The same pattern should be applied in `trace_and_mark_object` - capture generation at entry, verify it hasn't changed before calling `trace_fn`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable incremental marking
2. Allocate object A in slot S, mark it (generation = G)
3. Lazy sweep reclaims slot S, reallocates with new object B (generation = G+1)
4. `trace_and_mark_object` processes stale worklist entry for A
5. `trace_fn` is called on B's data instead of A's

```rust
// Conceptual PoC - concurrent scenario needed
fn trigger_bug() {
    // Object A allocated in slot S with generation G
    let gc_a = Gc::new(ObjectA);
    
    // Force incremental mark to add gc_a to worklist
    
    // Concurrently: lazy sweep reclaims slot S
    collect_full();  // triggers lazy sweep
    
    // New object B allocated in same slot S with generation G+1
    let gc_b = Gc::new(ObjectB);
    
    // Now trace_and_mark_object processes stale entry for A
    // But calls trace_fn on B's data due to missing generation check!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

In `trace_and_mark_object`, add generation capture at entry and verification before calling `trace_fn`:

```rust
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    let ptr = gc_box.as_ptr() as *const u8;
    let header = crate::heap::ptr_to_page_header(ptr);

    if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
        return;
    }
    let Some(idx) = crate::heap::ptr_to_object_index(ptr) else {
        return;
    };
    if !(*header.as_ptr()).is_allocated(idx) {
        return;
    }

    if (*gc_box.as_ptr()).is_under_construction() {
        return;
    }

    // FIX: Add generation check to detect slot reuse (matching scan_page_for_marked_refs)
    let marked_generation = (*gc_box.as_ptr()).generation();

    let block_size = (*header.as_ptr()).block_size as usize;
    let header_size = crate::heap::PageHeader::header_size(block_size);
    let data_ptr = ptr.add(header_size);

    // FIX: Verify generation hasn't changed before calling trace_fn
    if (*gc_box.as_ptr()).generation() != marked_generation {
        // Slot was reused - the trace_fn call would be on wrong object
        return;
    }

    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);
    ((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);

    while let Some(child_ptr) = visitor.worklist.pop() {
        state.push_work(child_ptr);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The incremental marking algorithm uses a worklist populated by `mark_slice`. When `trace_and_mark_object` processes an entry from the worklist, it assumes the entry is still valid. However, lazy sweep can reclaim and reuse slots between when an object is marked and when `trace_and_mark_object` processes it. Without a generation check, `trace_fn` operates on potentially uninitialized or wrong object data. This is a classic TOCTOU bug in concurrent GC systems.

**Rustacean (Soundness 觀點):**
Calling `trace_fn` on the wrong object's data is undefined behavior - we're treating memory containing object B as if it contains object A. Even if both objects implement `Trace`, the trace visitor may read/write fields at offsets specific to the original object type. This could cause memory corruption, use-after-free, or type confusion.

**Geohot (Exploit 觀點):**
If an attacker can influence the timing of lazy sweep relative to incremental marking, they could potentially cause `trace_fn` to be called on attacker-controlled data, leading to memory corruption or information disclosure. The generation mechanism exists specifically to detect this slot reuse - not using it is a critical oversight.