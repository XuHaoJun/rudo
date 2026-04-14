# [Bug]: execute_snapshot worklist pop reads generation before is_allocated check (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Concurrent lazy sweep could deallocate slot between generation read and is_allocated |
| **Severity (嚴重程度)** | High | UB - reading from potentially deallocated slot |
| **Reproducibility (復現難度)** | Medium | Race condition between mark and is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `execute_snapshot` in `gc/incremental.rs` (lines 594-605) and `trace_and_mark_object` (lines 805-814)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

Following the correct pattern (as seen in bug561/bug562 fixes):
1. Check `is_allocated` FIRST
2. Then read `generation()`

This ensures we never read from a deallocated slot.

### 實際行為 (Actual Behavior)

In `execute_snapshot` (incremental.rs:594-605):

```rust
while let Some((ptr, enqueue_generation)) = visitor.worklist.pop() {
    // FIX bug512: Verify slot wasn't reused since enqueue.
    // The generation was captured when the object was pushed to worklist.
    // If slot was swept and reused, generation will differ.
    unsafe {
        let current_generation = (*ptr.as_ptr()).generation();  // LINE 599 - READS GEN!
        if current_generation != enqueue_generation {
            continue; // Slot was reused - skip this entry
        }
        state.push_work(ptr);
    }
}
```

Similarly in `trace_and_mark_object` (incremental.rs:805-814):

```rust
while let Some((child_ptr, enqueue_generation)) = visitor.worklist.pop() {
    // FIX bug512: Verify slot wasn't reused since enqueue.
    // The generation was captured when the object was pushed to worklist.
    // If slot was swept and reused, generation will differ.
    let current_generation = (*child_ptr.as_ptr()).generation();  // LINE 809 - READS GEN!
    if current_generation != enqueue_generation {
        continue; // Slot was reused - skip this entry
    }
    state.push_work(child_ptr);
}
```

The generation is read at lines 599 and 809 **before** any `is_allocated` check. If the slot is deallocated (but not reused) between when it was enqueued and when it's popped, we're reading `generation()` from deallocated memory.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Timeline of potential race:**

1. Thread A: `mark_root_for_snapshot()` pushes `(ptr, enqueue_generation)` to worklist
   - At this point, `is_allocated` was checked (line 532) before reading generation (line 539)
   - Generation is captured and stored with the worklist entry
2. **Race window**: Lazy sweep deallocates slot (slot is empty, not reused)
3. Thread A: `execute_snapshot()` pops the entry from worklist
4. Line 599: Reads `current_generation` from deallocated slot - **UB!**
5. Generation check passes (generation unchanged - slot is empty, not reused)
6. `state.push_work(ptr)` is called with pointer to deallocated slot

**Why this is UB:**
- The generation check detects **slot REUSE** (new object in slot)
- It does NOT detect **simple deallocation** (slot is empty)
- If slot is deallocated but not reused, `enqueue_generation` equals `current_generation`
- But `generation()` was read from deallocated memory - this is UB

**Contrast with bug561/bug562 fixes:**
The correct pattern is to check `is_allocated` FIRST, then read any GcBox fields:

```rust
// FIX pattern (bug561/bug562):
if !(*header).is_allocated(idx) {
    return;
}
// Now safe to read generation
let current_generation = (*ptr.as_ptr()).generation();
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Theoretical race scenario requiring concurrent lazy sweep and worklist processing. Miri or ThreadSanitizer would detect this UB.

```rust
// Pseudocode - requires precise timing
// Thread A: Runs execute_snapshot, processes worklist
// Thread B: Runs lazy sweep to deallocate (not reuse) a slot that was enqueued

// Window: between worklist.push(ptr, gen) and worklist.pop()
// If sweep deallocates slot (empty, not reused) in this window,
// generation check passes but we read from deallocated memory
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check BEFORE reading `generation()` in both locations:

**Location 1: execute_snapshot (lines 594-605)**
```rust
while let Some((ptr, enqueue_generation)) = visitor.worklist.pop() {
    unsafe {
        // FIX bug565: Check is_allocated BEFORE reading generation.
        // Must verify slot is still allocated before reading any GcBox fields.
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                continue; // Slot was swept - skip this entry
            }
        }

        // Now safe to read generation from guaranteed allocated slot
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != enqueue_generation {
            continue; // Slot was reused - skip this entry
        }
        state.push_work(ptr);
    }
}
```

**Location 2: trace_and_mark_object (lines 805-814)**
```rust
while let Some((child_ptr, enqueue_generation)) = visitor.worklist.pop() {
    // FIX bug565: Check is_allocated BEFORE reading generation.
    if let Some(idx) = crate::heap::ptr_to_object_index(child_ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(child_ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            continue; // Slot was swept - skip this entry
        }
    }

    // Now safe to read generation from guaranteed allocated slot
    let current_generation = (*child_ptr.as_ptr()).generation();
    if current_generation != enqueue_generation {
        continue; // Slot was reused - skip this entry
    }
    state.push_work(child_ptr);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Reading from deallocated memory in a concurrent GC is a serious issue. The generation check was added in bug512 to detect slot REUSE, but it doesn't protect against simple deallocation. The correct pattern is to verify allocation status BEFORE reading any object fields. This is the same fundamental issue as bug561/bug562 but in the worklist processing code path.

**Rustacean (Soundness 觀點):**
This is undefined behavior - reading from memory that may have been deallocated. Even if the generations happen to match (making the logic "work" for the reuse case), the read itself is UB when the slot is simply deallocated. The fix is straightforward: check `is_allocated` before reading `generation()`.

**Geohot (Exploit 觀點):**
While this is a race condition that's difficult to exploit, the UB itself is concerning. If an attacker could somehow control the timing precisely, they might be able to cause incorrect GC behavior by manipulating when lazy sweep runs relative to mark operations. The deallocated-but-not-reused case is particularly insidious because the generation check doesn't catch it.

---

## 相關 Issue

- bug561: scan_page_for_unmarked_refs reads gen before is_allocated (fixed 2026-04-10)
- bug562: scan_page_for_marked_refs reads gen before is_allocated (fixed 2026-04-10)
- bug563: test missing cfg gate for incremental marking (fixed 2026-04-10)
- bug564: GcBoxWeakRef::clone reads gen before is_allocated (fixed 2026-04-10)
- bug512: execute_snapshot worklist slot reuse detection (partial fix only - doesn't check is_allocated)