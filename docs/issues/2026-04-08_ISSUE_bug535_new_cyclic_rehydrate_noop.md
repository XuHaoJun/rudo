# [Bug]: Gc::new_cyclic calls no-op rehydrate_self_refs leaving self-references dead

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Any use of `Gc::new_cyclic` with self-references triggers this |
| **Severity (嚴重程度)** | Medium | Self-referential Gc pointers remain null after construction |
| **Reproducibility (復現難度)** | Low | Easy to reproduce - just use new_cyclic with self-ref |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::new_cyclic`, `rehydrate_self_refs` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Gc::new_cyclic` (line 1375) calls `rehydrate_self_refs` at line 1406 to rehydrate dead self-references. However, `rehydrate_self_refs` is a **no-op stub** that does nothing when it finds null Gc pointers - it only contains a FIXME comment explaining why it's not implemented.

This means self-referential cycles created with `Gc::new_cyclic` have their internal `Gc` pointers remain null forever, even after the allocation is complete.

### 預期行為 (Expected Behavior)
When `new_cyclic` calls `rehydrate_self_refs`, the function should:
1. Trace through the value's fields
2. Find any dead/self-referential `Gc` pointers (that were passed as `dead_gc`)
3. Update them to point to the newly created `GcBox`

### 實際行為 (Actual Behavior)
In `ptr.rs:3203-3231`, `rehydrate_self_refs` is a no-op:
```rust
fn rehydrate_self_refs<T: Trace>(_target: NonNull<GcBox<T>>, value: &T) {
    struct Rehydrator;
    impl Visitor for Rehydrator {
        fn visit<U: Trace>(&mut self, gc: &Gc<U>) {
            if gc.ptr.load(Ordering::Relaxed).is_null() {
                // FIXME: Self-referential cycle support is not implemented.
                // ... (explanation) ...
                // NO ACTUAL REHYDRATION HAPPENS
            }
        }
        unsafe fn visit_region(&mut self, _ptr: *const u8, _len: usize) {}
    }
    let mut rehydrator = Rehydrator;
    value.trace(&mut rehydrator);
    // Function returns without doing anything!
}
```

The `dead_gc` passed to `data_fn` starts with `ptr: AtomicNullable::null()`. The callback stores this `dead_gc` in self-referential structures. Then `rehydrate_self_refs` is called to "fix" these null pointers, but since it's a no-op, they remain null.

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **History**: `rehydrate_self_refs` was added as a placeholder for `new_cyclic` (deprecated)
2. **Problem**: The function was never implemented - it's still just a FIXME comment
3. **Current call**: `new_cyclic` calls `rehydrate_self_refs(gc_box_ptr, &(*gc_box).value)` at line 1406
4. **Effect**: The function traces the value but does nothing when it finds null Gc pointers

The issue is that `rehydrate_self_refs` receives `_target` (underscore prefix = unused) and the `Rehydrator::visit` only has a FIXME comment, no actual rehydration logic.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcCell};

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Gc<Node>>>,  // Self reference via Gc (not Weak)
    data: i32,
}

// Create self-referential node using new_cyclic
let node = Gc::new_cyclic(|dead_self| Node {
    self_ref: GcCell::new(Some(dead_self)),  // dead_self has null internal pointer
    data: 42,
});

// After construction, rehydrate_self_refs is called but does nothing
// The self_ref still contains a Gc with null pointer!
let inner = node.self_ref.borrow();
if inner.is_some() {
    let inner_gc = inner.unwrap();
    // This will likely fail or cause issues because inner_gc.ptr is null
    // even though node itself is valid
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1 (Quick Fix)**: Remove the call to `rehydrate_self_refs` from `new_cyclic` until it's properly implemented. Document that self-referential cycles may not be properly rehydrated after GC.

**Option 2 (Complete Fix)**: Actually implement `rehydrate_self_refs`:
- Store a unique allocation ID in each GcBox (not just generation)
- When rehydrate_self_refs is called, iterate through traced Gc pointers
- For each null/stale pointer, verify the target allocation ID matches
- Update the pointer to point to the new allocation if valid

**Note**: Compare with `new_cyclic_weak` which correctly does NOT call `rehydrate_self_refs` (line 1535-1539 has a comment explaining why).

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The FIXME explains why rehydration is difficult: type erasure prevents us from knowing if a null Gc should point to this specific allocation. Without runtime type info, we can't verify compatibility.

However, we do have generation counters. Perhaps rehydration should work differently - instead of "fixing" null pointers, we should ensure Gc::upgrade properly detects when a slot has been reused and returns None.

**Rustacean (Soundness 觀點):**
Calling a no-op function that was designed as a placeholder is dangerous. The function should either:
1. Actually do something useful
2. Not be called at all
3. Have a clear comment that it's a no-op for future implementation

The current state (calling a FIXME stub) is misleading and could lead to bugs if someone relies on the supposed "rehydration" behavior.

**Geohot (Exploit 觀點):**
If rehydration doesn't actually happen, could an attacker exploit this?
- Create self-referential structure with new_cyclic
- Let it be collected, slot reused for different type
- Old self-refs might point to new (attacker-controlled) data

However, generation checking should prevent invalid upgrades. The risk is lower if refs properly detect slot reuse.

**Summary:**
The bug is that `rehydrate_self_refs` is a placeholder function being called as if it worked. Either implement it properly or remove the call.

---

## 驗證指南檢查

- Pattern 1 (Full GC 遮蔽 barrier bug): N/A - not a barrier bug
- Pattern 2 (單執行緒無法觸發競態): Rehydration is single-threaded during allocation
- Pattern 3 (測試情境與 issue 描述不符): PoC matches issue description
- Pattern 4 (容器內的 Gc 未被當作 root): N/A
- Pattern 5 (難以觀察的內部狀態): Rehydration success/failure is observable via accessing the self-reference