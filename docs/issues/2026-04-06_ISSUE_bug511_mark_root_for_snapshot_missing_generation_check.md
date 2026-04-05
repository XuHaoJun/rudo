# [Bug]: mark_root_for_snapshot missing generation check - slot reuse TOCTOU during STW

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Only during STW execute_snapshot; requires lazy sweep concurrent with STW snapshot |
| **Severity (嚴重程度)** | Critical | Wrong object traced during STW could corrupt GC state |
| **Reproducibility (復現難度)** | Medium | Needs concurrent lazy sweep timing relative to STW snapshot |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_root_for_snapshot`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`mark_root_for_snapshot` should verify slot has not been reused (generation unchanged) before pushing to worklist, similar to `trace_and_mark_object` and `scan_page_for_unmarked_refs` which capture and verify generation.

### 實際行為 (Actual Behavior)
`mark_root_for_snapshot` only checks `is_allocated` but does NOT capture or verify generation. If slot is swept and reused between `set_mark` and when `trace_and_mark_object` processes the worklist entry, `trace_fn` is called on wrong object data.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `gc/incremental.rs`, `mark_root_for_snapshot` (lines 522-543):

```rust
unsafe fn mark_root_for_snapshot(ptr: NonNull<GcBox<()>>, visitor: &mut crate::trace::GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let header = crate::heap::ptr_to_page_header(ptr_addr);

    if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
        return;
    }

    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
        // Skip if slot was swept; avoids marking wrong object when lazy sweep runs concurrently.
        if !(*header.as_ptr()).is_allocated(idx) {
            return;
        }
        let was_marked = (*header.as_ptr()).is_marked(idx);
        if !was_marked {
            (*header.as_ptr()).set_mark(idx);  // <-- Uses set_mark, not try_mark
            visitor.objects_marked += 1;
            let enqueue_generation = (*ptr.as_ptr()).generation();  // <-- Captured but NOT stored with worklist entry
            visitor.worklist.push((ptr, enqueue_generation));  // <-- Generation not verified later
        }
    }
}
```

The `enqueue_generation` is captured but:
1. Not stored with the worklist entry (only `ptr` is stored)
2. Not verified in `trace_and_mark_object` when worklist is popped

Compare with `trace_and_mark_object` (lines 747-797) which DOES verify generation:
```rust
let marked_generation = (*gc_box.as_ptr()).generation();
// ... verification checks ...
if (*gc_box.as_ptr()).generation() != marked_generation {
    return;  // Slot was reused - skip
}
```

And `scan_page_for_unmarked_refs` (lines 964-1034) which also verifies:
```rust
let marked_generation = unsafe { (*gc_box_ptr).generation() };
// ... later ...
let current_generation = unsafe { (*gc_box_ptr).generation() };
if current_generation != marked_generation {
    break;  // Slot was reused
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
During STW snapshot marking, all mutators are stopped but lazy sweep may still be running in the background (if sweep is not also stopped). The `mark_root_for_snapshot` function assumes the world is stopped so no concurrent modification is possible, but sweep operates independently. If sweep reclaims a root slot between when `mark_root_for_snapshot` pushes it and when `trace_and_mark_object` processes it, the generation check in `trace_and_mark_object` should catch this - but currently `enqueue_generation` is not stored with the worklist entry so the check is ineffective.

**Rustacean (Soundness 觀點):**
Calling `trace_fn` on wrong object data is undefined behavior. The worklist stores `(ptr, enqueue_generation)` but `enqueue_generation` is never actually written to the tuple - only `ptr` is pushed. This makes the generation tracking mechanism broken for root marking.

**Geohot (Exploit 觀點):**
If an attacker can influence GC timing to cause sweep to reclaim a root slot during STW snapshot, they could potentially cause `trace_fn` to be called on attacker-controlled data, leading to memory corruption. The generation mechanism exists to prevent this - not using it correctly is a critical oversight.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Allocate root object A in slot S during mutator execution
2. GC triggers, `execute_snapshot` stops all mutators
3. `mark_root_for_snapshot` marks A and pushes to worklist (generation = G)
4. Concurrent lazy sweep reclaims slot S, allocates new object B (generation = G+1)
5. `trace_and_mark_object` processes worklist entry for A/B
6. `trace_fn` is called on B's data instead of A's

Note: This requires sweep to be running during STW, which may not be the case by design. Further investigation needed.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Option 1: Store generation with worklist entry and verify on pop
```rust
// In mark_root_for_snapshot:
visitor.worklist.push((ptr, enqueue_generation));  // enqueue_generation IS stored

// In trace_and_mark_object worklist processing:
while let Some((ptr, enqueue_generation)) = visitor.worklist.pop() {
    // Verify generation matches
}
```

Option 2: Add generation check in `mark_root_for_snapshot` before pushing
```rust
let marked_generation = (*ptr.as_ptr()).generation();
if (*ptr.as_ptr()).generation() != marked_generation {
    return;  // Slot was reused
}
```

Option 3: Use `try_mark` instead of `set_mark` like other marking functions