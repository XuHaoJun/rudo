# [Bug]: trace_and_mark_object discards enqueue_generation from GcVisitor worklist

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires concurrent lazy sweep during STW snapshot marking |
| **Severity (嚴重程度)** | Critical | Wrong object traced during GC marking corrupts live set |
| **Reproducibility (復現難度)** | Medium | Needs precise timing between lazy sweep and STW marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `trace_and_mark_object`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`trace_and_mark_object` should verify `enqueue_generation` matches current generation before calling `trace_fn`, preserving the slot reuse detection from `mark_root_for_snapshot`.

### 實際行為 (Actual Behavior)
`trace_and_mark_object` discards `_enqueue_generation` when popping from `GcVisitor.worklist` and calls `state.push_work(child_ptr)` which only passes the pointer, losing the generation information that was captured at enqueue time.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `gc/incremental.rs`, `mark_root_for_snapshot` (lines 537-540) pushes entries with generation:
```rust
(*header.as_ptr()).set_mark(idx);
visitor.objects_marked += 1;
let enqueue_generation = (*ptr.as_ptr()).generation();
visitor.worklist.push((ptr, enqueue_generation));  // Stores (ptr, generation)
```

But `trace_and_mark_object` (lines 794-795) discards the generation:
```rust
while let Some((child_ptr, _enqueue_generation)) = visitor.worklist.pop() {
    state.push_work(child_ptr);  // Only pushes ptr, loses generation!
}
```

The `IncrementalMarkState::push_work` accepts `NonNull<GcBox<()>>` only:
```rust
pub fn push_work(&self, ptr: NonNull<GcBox<()>>) {
    self.worklist().push(ptr.as_ptr() as usize);  // No generation parameter
}
```

Compare with `trace_and_mark_object` at lines 770-780 which captures and verifies generation for its own entry:
```rust
let marked_generation = (*gc_box.as_ptr()).generation();
// ...
if (*gc_box.as_ptr()).generation() != marked_generation {
    return;  // Slot was reused - skip
}
```

The generation check pattern exists in this function but is NOT applied to worklist entries from `GcVisitor`.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The `GcVisitor.worklist` stores `(ptr, generation)` tuples specifically to detect slot reuse between enqueue and dequeue. `mark_root_for_snapshot` captures generation at push time. `trace_and_mark_object` should verify this generation before processing children, but currently ignores it entirely.

**Rustacean (Soundness 觀點):**
Calling `trace_fn` on wrong object data is undefined behavior. If a slot is swept and reused between when `mark_root_for_snapshot` enqueues an object and when `trace_and_mark_object` processes it, the generation check would catch this - but only if we actually used the stored generation.

**Geohot (Exploit 觀點):**
An attacker who can influence GC timing could cause slot reuse during the snapshot marking window. Without generation verification on worklist entries, `trace_fn` could be called on attacker-controlled data.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Requires Miri or precise concurrent timing:
1. Allocate root object A in slot S (generation = G)
2. Minor GC runs, object A is promoted to old gen
3. Major GC triggers, STW begins
4. `execute_snapshot` calls `mark_root_for_snapshot` on A, pushes `(A_ptr, G)` to `GcVisitor.worklist`
5. Concurrent lazy sweep reclaims slot S, allocates new object B (generation = G+1)
6. `trace_and_mark_object` pops `(B_ptr, G)` from worklist
7. Generation check in `trace_and_mark_object` only checks its OWN entry (root), not the popped child
8. `state.push_work(B_ptr)` loses generation G
9. Later processing of B calls `trace_fn` on B's data

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Option 1: Add generation check when processing GcVisitor worklist:
```rust
while let Some((child_ptr, enqueue_generation)) = visitor.worklist.pop() {
    // Verify slot wasn't reused since enqueue
    let current_gen = (*child_ptr.as_ptr()).generation();
    if current_gen != enqueue_generation {
        continue; // Slot was reused - skip this entry
    }
    state.push_work(child_ptr);
}
```

Option 2: Store generation in worklist entry when pushing to `IncrementalMarkState`:
- Modify `push_work` to accept generation
- Store `(ptr, generation)` in `IncrementalMarkState.worklist`
- Verify on pop

Option 3: Verify generation in `state.push_work` or when popping from `IncrementalMarkState.worklist`