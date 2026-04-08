# [Bug]: rehydrate_self_refs is a no-op stub despite being called from new_cyclic_weak

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any use of `new_cyclic_weak` for self-referential structures triggers this |
| **Severity (嚴重程度)** | Medium | Self-referential Weak refs never get rehydrated after GC collects cycle |
| **Reproducibility (復現難度)** | Low | Easy to reproduce - just use new_cyclic_weak with a cycle |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `rehydrate_self_refs` (ptr.rs:3205)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

The `rehydrate_self_refs` function is a **no-op stub** that does nothing when it encounters null `Gc` pointers during tracing. The function body only contains a FIXME comment explaining why rehydration is not implemented, but `new_cyclic_weak` now calls it at line 1540, giving the **false impression** that rehydration is happening.

### 預期行為 (Expected Behavior)

When `new_cyclic_weak` calls `rehydrate_self_refs`, the function should:
1. Trace through the value's fields
2. Find any dead/self-referential `Gc` pointers
3. Rehydrate them to point to the newly created `GcBox`

### 實際行為 (Actual Behavior)

In `ptr.rs:3205-3233`:
```rust
fn rehydrate_self_refs<T: Trace>(_target: NonNull<GcBox<T>>, value: &T) {
    struct Rehydrator;

    impl Visitor for Rehydrator {
        fn visit<U: Trace>(&mut self, gc: &Gc<U>) {
            if gc.ptr.load(Ordering::Relaxed).is_null() {
                // FIXME: Self-referential cycle support is not implemented.
                // ... (explanation of why it's hard)
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

The function traces the value but **does nothing** when it finds null Gc pointers. The FIXME was left as a marker but the function is now being called from production code.

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **History**: `rehydrate_self_refs` was added for `new_cyclic` (deprecated) as a placeholder
2. **bug533 fix**: Added call to `rehydrate_self_refs` in `new_cyclic_weak` at line 1540
3. **Problem**: The function was never implemented - it's still just a FIXME comment

When `new_cyclic_weak` calls `rehydrate_self_refs`, it effectively does nothing because:
- The `Rehydrator::visit` only has a FIXME comment, no actual rehydration logic
- The function signature takes `_target` (underscore prefix = unused) 
- When tracing finds a null Gc, nothing is done to fix it

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, GcCell, collect_full};

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Weak<Node>>>,
    data: i32,
}

// Create self-referential node using new_cyclic_weak
let node = Gc::new_cyclic_weak(|weak_self| Node {
    self_ref: GcCell::new(Some(weak_self)),
    data: 42,
});

// After construction, rehydrate_self_refs is called but does nothing
// The Weak is already valid (not null), so it works

// Drop to create unreachable cycle
drop(node);
collect_full();

// After GC collects the cycle and slot is reused,
// the rehydrate_self_refs call on the new allocation does nothing
// because there are no null Gc pointers to rehydrate in the new value

// The bug is: rehydrate_self_refs is a no-op regardless of whether
// there's anything to rehydrate. If called at wrong time or on wrong
// type, it simply doesn't work.
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1 (Quick Fix)**: Remove the call to `rehydrate_self_refs` from `new_cyclic_weak` until it's properly implemented. Document that self-referential cycles may not be properly rehydrated after GC.

**Option 2 (Complete Fix)**: Actually implement `rehydrate_self_refs`:
- Store a unique allocation ID in each GcBox (not just generation)
- When rehydrate_self_refs is called, iterate through traced Gc pointers
- For each null/stale pointer, verify the target allocation ID matches
- Update the pointer to point to the new allocation if valid

**Option 3 (Alternative Design)**: Change the design so rehydration isn't needed:
- Use generation counter in Weak to detect when slot is reused
- Weak::upgrade already checks generation, so stale refs return None
- This is already partially implemented but may not cover all cases

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The FIXME explains why rehydration is difficult: type erasure prevents us from knowing if a null Gc should point to this specific allocation. Without runtime type info, we can't verify compatibility.

However, we do have generation counters. Perhaps rehydration should work differently - instead of "fixing" null pointers, we should ensure Weak::upgrade properly detects when a slot has been reused and returns None.

**Rustacean (Soundness 觀點):**
Calling a no-op function that was designed as a placeholder is dangerous. The function should either:
1. Actually do something useful
2. Not be called at all
3. Have a clear comment that it's a no-op for future implementation

The current state (calling a FIXME stub) is misleading and could lead to bugs if someone relies on the supposed "rehydration" behavior.

**Geohot (Exploit 觀點):**
If rehydration doesn't actually happen, could an attacker exploit this?
- Create self-referential structure with new_cyclic_weak
- Let it be collected, slot reused for different type
- Old Weak refs might point to new (attacker-controlled) data

However, generation checking in Weak::upgrade should prevent invalid upgrades. The risk is lower if Weak refs properly detect slot reuse.

**Summary:**
The bug is that `rehydrate_self_refs` is a placeholder function being called as if it worked. Either implement it properly or remove the call.

---

## 驗證指南檢查

- Pattern 1 (Full GC 遮蔽 barrier bug): N/A - not a barrier bug
- Pattern 2 (單執行緒無法觸發競態): Rehydration is single-threaded during allocation
- Pattern 3 (測試情境與 issue 描述不符): PoC matches issue description
- Pattern 4 (容器內的 Gc 未被當作 root): N/A
- Pattern 5 (難以觀察的內部狀態): Rehydration success/failure is observable via Weak::upgrade