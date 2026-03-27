# [Bug]: process_worklist generation check is ineffective - compares same value twice

**Status:** Closed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Very Low` | Requires generation counter wraparound (2^32 slot reuses) |
| **Severity (嚴重程度)** | `Critical` | Calling trace_fn on wrong object after slot reuse - memory corruption |
| **Reproducibility (重現難度)** | `Very Hard` | Requires generation wraparound, ~2^32 allocations in same slot |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `process_worklist` in `gc/gc.rs` (lines 3046, 3060-3063)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

`process_worklist` should verify that the object being traced is the same object that was enqueued. Since generation changes on slot reuse, a generation check comparing the **enqueue-time generation** against the **dequeue-time generation** should be performed before calling `trace_fn`.

### 實際行為 (Actual Behavior)

The bug435 fix (commit 6a77e42) added a generation check, but the check is ineffective:

```rust
// Line 3046 - reads generation at DEQUEUE time
let pop_generation = (*ptr.as_ptr()).generation();

// ... checks is_allocated, is_marked ...

// Lines 3060-3063 - reads AGAIN at DEQUEUE time
let current_generation = (*ptr.as_ptr()).generation();
if current_generation != pop_generation {
    continue;  // Never triggers!
}
```

Both `pop_generation` and `current_generation` are read **after** dequeuing, from the **same object at the same memory location**. They will always be equal unless:
1. The generation counter wraps around (after ~2^32 reuses of the same slot)
2. The slot is reused **between** the two sequential reads (requires precise timing)

### 為什麼修復不正確 (Why the Fix is Wrong)

The generation at **enqueue time** is never stored. The check compares the generation with itself:

- If slot was reused between enqueue and dequeue: `pop_generation` = new object's gen, `current_generation` = same new object's gen → Equal → Bug not caught!
- Only catches if generation changes **between** the two reads (extremely rare)

### 對比其他正確模式 (Correct Pattern)

`worker_mark_loop_with_registry` (marker.rs:1117-1134) correctly captures generation **when processing** the object (right after `try_mark` succeeds), then checks before calling `trace_fn`. This works because the generation is captured at the right moment.

---

## 根本原因分析 (Root Cause Analysis)

The bug435 fix incorrectly captures generation at DEQUEUE time instead of ENQUEUE time. When an object is enqueued to `visitor.worklist.push(ptr)`, the generation is not stored alongside the pointer.

When the object is later dequeued and the generation is read twice sequentially, both reads return the current (post-reuse) generation, so they always match.

---

## 建議修復方案 (Suggested Fix)

The worklist needs to store generation alongside the pointer. Options:

1. **Change worklist to store (ptr, generation) pairs** - Most correct but requires API change
2. **Store generation in a parallel HashMap keyed by pointer** - Less invasive
3. **Accept the ~2^32generation wraparound risk** - Not recommended

Example fix using Option 1 (worklist as Vec of (ptr, generation)):

```rust
// When pushing to worklist:
let gen = (*gc_box.as_ptr()).generation();
self.worklist.push((ptr, gen));

// When popping from worklist:
while let Some((ptr, enqueue_generation)) = self.worklist.pop() {
    // ... checks ...
    let current_generation = (*ptr.as_ptr()).generation();
    if current_generation != enqueue_generation {
        continue;  // Slot was reused!
    }
    ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), self);
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check pattern in process_worklist is fundamentally flawed. Generation must be captured at enqueue time and compared at dequeue time. The current implementation captures it twice at dequeue time, achieving nothing.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. If generation wraps (~2^32 reuses), trace_fn could be called on wrong object data. While unlikely, this is undefined behavior.

**Geohot (Exploit 觀點):**
The generation wraparound attack surface is theoretical but exists. An attacker with enough control over the system could potentially trigger rapid slot reallocation to wrap the counter.

---

## 相關 Issue

- bug435: Original issue - process_worklist missing generation check (CLOSED but fix is incorrect)
- bug427: worker_mark_loop generation check - correct pattern
- bug426: trace_and_mark_object generation check - correct pattern

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-03-28

**Fix:** Changed `GcVisitor.worklist` from `Vec<NonNull<GcBox<()>>>` to `Vec<(NonNull<GcBox<()>>, u32)>` to store generation at enqueue time.

**Files Changed:**
- `crates/rudo-gc/src/trace.rs` - Changed worklist type and updated doc comment
- `crates/rudo-gc/src/gc/gc.rs` - Updated all push/pop sites:
  - `process_worklist` - Now destructures `(ptr, enqueue_generation)` and compares with current generation
  - `mark_object_minor` - Captures generation before push
  - `mark_object` - Captures generation before push
  - `mark_and_trace_incremental` - Captures generation before push
  - `Visitor::visit` - Captures generation before push
  - `mark_minor_roots_multi` - Uses `process_worklist` instead of manual pop
  - `mark_major_roots_multi` - Uses `process_worklist` instead of manual pop
- `crates/rudo-gc/src/gc/incremental.rs` - Updated push sites and worklist transfer sites

**Key Changes:**
1. Worklist now stores `(pointer, enqueue_generation)` pairs
2. At push time, generation is captured and stored alongside pointer
3. At pop time, stored generation is compared with current generation to detect slot reuse
