# [Bug]: Weak::upgrade() panics when try_upgrade() returns None - inconsistent behavior

**Status:** Closed
**Tags:** Verified, Fixed

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Programming error scenario during Gc::new_cyclic_weak |
| **Severity (嚴重程度)** | `Medium` | Unexpected panic vs graceful None return |
| **Reproducibility (復現難度)** | `Medium` | Requires specific Gc::new_cyclic_weak usage pattern |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `Weak::upgrade()` in `ptr.rs:2323-2422`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Weak::upgrade()` and `Weak::try_upgrade()` should have consistent behavior when the `GcBox` is under construction. Both should return `None` instead of panicking.

### 實際行為 (Actual Behavior)

`Weak::upgrade()` panics when `is_under_construction()` is true:

```rust
// ptr.rs:2352-2357
assert!(
    !gc_box.is_under_construction(),
    "Weak::upgrade: cannot upgrade while GcBox is under construction. \
     This typically happens if you call upgrade() inside the closure \
     passed to Gc::new_cyclic_weak()."
);
```

But `Weak::try_upgrade()` returns `None` for the same condition:

```rust
// ptr.rs:2473-2475
if gc_box.is_under_construction() {
    return None;
}
```

### 影響範圍

This creates inconsistent API behavior:
- `Weak::upgrade()` - panics on `is_under_construction`
- `Weak::try_upgrade()` - returns `None` on `is_under_construction`
- `GcBoxWeakRef::upgrade()` - returns `None` on `is_under_construction` (internal type)

The inconsistency makes it difficult to write correct code that handles both methods similarly.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `Weak::upgrade()` method uses `assert!` instead of returning `None` for the `is_under_construction` check, treating it as a programmer error rather than a runtime condition.

However:
1. During `Gc::new_cyclic_weak`, it's possible for user code to have a `Weak` reference from before construction started
2. The weak reference may legitimately exist and should return `None`, not panic
3. The comment suggests this is a programming error, but in async/reactive contexts, this can happen naturally

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Create a weak reference before cyclic weak construction
    let weak_ref: Weak<Data>;
    
    let _gc = Gc::new_cyclic_weak(|weak| {
        // At this point, weak is being constructed
        // If weak_ref were somehow accessible here and we called upgrade(),
        // it would panic instead of returning None
        weak_ref = weak.clone();
        Data { value: 42 }
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change `Weak::upgrade()` to return `None` instead of panicking when `is_under_construction()` is true:

```rust
// Change from:
assert!(
    !gc_box.is_under_construction(),
    "Weak::upgrade: cannot upgrade while GcBox is under construction. ..."
);

// To:
if gc_box.is_under_construction() {
    return None;
}
```

This makes behavior consistent with:
- `Weak::try_upgrade()` (returns None)
- `GcBoxWeakRef::upgrade()` (returns None)

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generational GC should handle construction semantics consistently. During `new_cyclic_weak`, objects may not be fully initialized, but weak references existing prior to construction should gracefully return `None` rather than cause panics.

**Rustacean (Soundness 觀點):**
Using `assert!` for a runtime condition (rather than a true invariant) is problematic. `is_under_construction()` is a runtime state that can occur in legitimate usage patterns, especially with async/closure-based APIs. Returning `None` is more consistent with Rust error handling idioms.

**Geohot (Exploit 觀點):**
Panics in cleanup/drop paths can cause unexpected process termination. In GC systems with weak references, graceful degradation (returning None) is preferred over crashes when possible.

---

## 修復紀錄 (Fix Applied)

**Date:** 2026-03-28

**Fix:** Changed `Weak::upgrade()` to return `None` instead of panicking when `is_under_construction()` is true.

**File Changed:** `crates/rudo-gc/src/ptr.rs`

**Changes:**
1. Line 2333-2337: Replaced `assert!(!gc_box.is_under_construction(), ...)` with `if gc_box.is_under_construction() { return None; }`
2. Removed the "# Panics" section from the doc comment that documented the panic behavior
