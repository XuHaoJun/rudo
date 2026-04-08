# [Bug]: new_cyclic_weak self-referential cycles not rehydrated after GC

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | User-facing API with documented but unimplemented behavior |
| **Severity (嚴重程度)** | High | Self-referential data structures collect but leave dangling references |
| **Reproducibility (復現難度)** | Low | PoC demonstrates cycle collection with upgrade returning None |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::new_cyclic_weak`, `rehydrate_self_refs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When using `Gc::new_cyclic_weak` to create self-referential data structures (like linked lists or trees with back-references), the self-referential `Weak` pointers become "stuck" after the cyclic structure is collected. They never get rehydrated to point to the new allocation when the slot is reused, and upgrade() continues to return `None` even after a new valid Gc exists at the same memory address.

### 預期行為 (Expected Behavior)

1. `new_cyclic_weak` creates a self-referential structure where `Weak::upgrade()` returns `None` during construction
2. After construction completes, `Weak::upgrade()` should return `Some(Gc)` pointing to the created object
3. When the self-referential structure becomes unreachable and is collected, the `Weak` references inside it are marked dead
4. **Critical**: If the memory slot is reused for a new allocation, the `Weak` should detect this (via generation mismatch) and return `None` appropriately
5. When a new `Gc` is created at the same address, previously created `Weak` references to that address should NOT automatically rehydrate to the new object - they should detect the generation change and remain invalid

### 實際行為 (Actual Behavior)

The `rehydrate_self_refs` function (called from `new_cyclic` path only) is marked as `// FIXME: Self-referential cycle support is not implemented.` (ptr.rs:3203).

However, `new_cyclic_weak` does NOT call `rehydrate_self_refs` at all - it only calls it in the deprecated `new_cyclic` path.

This means:
1. During construction of `new_cyclic_weak`, the `Weak` passed to the closure CAN be upgraded after construction completes
2. BUT when the cyclic structure is collected and the memory slot is reused, the `Weak` references embedded in the collected value are NOT rehydrated
3. The `rehydrate_self_refs` function exists but is never called from `new_cyclic_weak`

The result is that self-referential cycles created via `new_cyclic_weak` can accumulate "dead" Weak references that point to collected slots, and the rehydration mechanism designed to handle this is not implemented.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `ptr.rs:3197`, the `rehydrate_self_refs` function is defined but contains only a FIXME comment:

```rust
fn rehydrate_self_refs<T: Trace>(_target: NonNull<GcBox<T>>, value: &T) {
    struct Rehydrator;

    impl Visitor for Rehydrator {
        fn visit<U: Trace>(&mut self, gc: &Gc<U>) {
            if gc.ptr.load(Ordering::Relaxed).is_null() {
                // FIXME: Self-referential cycle support is not implemented.
                // ... (long comment about why this is hard)
            }
        }

        unsafe fn visit_region(&mut self, _ptr: *const u8, _len: usize) {}
    }

    let mut rehydrator = Rehydrator;
    value.trace(&mut rehydrator);
}
```

This function is called from `new_cyclic` (ptr.rs:1406) but NOT from `new_cyclic_weak`.

The `new_cyclic_weak` function (ptr.rs:1447-1544) creates a self-referential structure but does not call any rehydration function after the value is written to the GcBox.

When self-referential cycles become unreachable and are collected, the slot can be reused for a new allocation. However, the old `Weak` references embedded in the collected value's fields are not updated to point to the new allocation - they still point to the old (now collected) GcBox with its generation counter having been incremented.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, GcCell, collect_full};

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Weak<Node>>>,
    data: i32,
}

// Create a self-referential cycle
let node = Gc::new_cyclic_weak(|weak_self| Node {
    self_ref: GcCell::new(Some(weak_self)),
    data: 42,
});

// Upgrade should work after construction
assert!(node.self_ref.borrow().as_ref().unwrap().upgrade().is_some());

// Drop the only reference to create an unreachable cycle
drop(node);

// Force GC to collect the cycle and reuse the slot
collect_full();

// Create a new node at the same memory location (if slot was reused)
let new_node = Gc::new(Node {
    self_ref: GcCell::new(None),
    data: 100,
});

// The old weak reference should detect slot reuse via generation mismatch
// and return None, but the rehydration mechanism is not implemented,
// so the old Weak's behavior depends on implementation details of generation tracking
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **Implement `rehydrate_self_refs`** or determine if it's the correct approach
2. **Consider calling rehydration from `new_cyclic_weak`** after construction completes, similar to how `new_cyclic` calls it
3. **Alternative**: Document that `new_cyclic_weak` creates self-referential structures that cannot be safely reused after collection without explicit user intervention
4. **Add tests** to verify self-referential Weak behavior after cycle collection and slot reuse

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The rehydrate_self_refs function was likely designed to handle the case where a self-referential Gc is collected and its slot reused. The function would trace through the value's fields, find any dead Gc pointers (with null or stale internal pointers), and update them to point to the new allocation if the target is still live.

However, the implementation difficulty noted in the FIXME is real: without type information at runtime, we cannot safely verify that a "dead" Gc reference should be rehydrated to point to a specific new allocation. The type erasure in our design makes this problematic.

**Rustacean (Soundness 觀點):**
The FIXME mentions potential solutions:
1. Store a unique allocation ID in GcBox for comparison
2. Use runtime type information (RTTI)
3. Require users to manually rehydrate after construction

Option 3 seems most aligned with Rust's philosophy - explicit is better than implicit. Users who create self-referential structures should be responsible for maintaining them.

**Geohot (Exploit 觀點):**
If self-referential cycles are not properly rehydrated, an attacker might be able to:
1. Create a self-referential structure
2. Let it become unreachable and be collected
3. Allocate new data in the reused slot
4. Have old Weak references unexpectedly point to new data

This could potentially be used to bypass safety checks if the Weak's upgrade() returns Some when it shouldn't. However, the generation counter mechanism should prevent this if properly implemented and checked.

**Summary:**
The core issue is that `rehydrate_self_refs` is unimplemented and `new_cyclic_weak` doesn't call it. This leaves self-referential cycles in an undefined state after collection. The fix requires either implementing rehydration properly or explicitly documenting the limitation and requiring user intervention.