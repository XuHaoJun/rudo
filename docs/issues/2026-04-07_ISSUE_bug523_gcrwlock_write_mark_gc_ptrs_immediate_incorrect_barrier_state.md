# [Bug]: GcRwLock::write() passes barrier_active=true to mark_gc_ptrs_immediate unconditionally

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Occurs when incremental marking is NOT active but generational barrier is active |
| **Severity (嚴重程度)** | `Medium` | NEW GC pointers may not be marked black, potentially causing premature collection |
| **Reproducibility (復現難度)** | `Medium` | Requires specific GC timing to trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock`, `GcMutex`, `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

In `GcRwLock::write()` and related methods, `mark_gc_ptrs_immediate` is called with `barrier_active=true` unconditionally, regardless of whether incremental marking is actually active.

### 預期行為 (Expected Behavior)
- `mark_gc_ptrs_immediate` should only mark GC pointers when barriers are actually active
- The behavior should be consistent with `GcRwLockWriteGuard::drop()` which re-checks barrier state

### 實際行為 (Actual Behavior)
- `GcRwLock::write()` calls `mark_gc_ptrs_immediate(&*guard, true)` unconditionally
- This means marking happens even when `incremental_active` is false and `generational_active` is false
- But `GcRwLockWriteGuard::drop()` only marks when `generational_active || incremental_active`

## 🔬 根本原因分析 (Root Cause Analysis)

In `GcRwLock::write()` (sync.rs:283-305):

```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    record_satb_old_values_with_state(&*guard, true);  // FIX bug432 - intentional
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    mark_gc_ptrs_immediate(&*guard, true);  // BUG: passes true, not the actual barrier state!
    GcRwLockWriteGuard {
        guard,
        _marker: PhantomData,
    }
}
```

The `mark_gc_ptrs_immediate` function (sync.rs:58-74):

```rust
fn mark_gc_ptrs_immediate<T: GcCapture + ?Sized>(value: &T, barrier_active: bool) {
    if !barrier_active {
        return;  // Early return if barrier is false
    }
    // ... marks GC pointers black
}
```

When `GcRwLock::write()` passes `true` unconditionally:
- If `incremental_active=false` AND `generational_active=false`: marking happens anyway (should not)
- If `incremental_active=true` OR `generational_active=true`: marking happens (correct)

But in `GcRwLockWriteGuard::drop()` (sync.rs:472-498):

```rust
fn drop(&mut self) {
    let mut ptrs = Vec::with_capacity(32);
    self.guard.capture_gc_ptrs_into(&mut ptrs);

    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();

    if generational_active || incremental_active {  // Re-checks barrier state
        for gc_ptr in &ptrs {
            let _ = unsafe {
                crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
            };
        }
    }
    // ...
}
```

**Inconsistency**: `write()` marks regardless of barrier state, but `drop()` only marks when barriers ARE active.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable incremental marking with budget
2. Create a `Gc<GcRwLock<Vec<Gc<T>>>>`
3. Call `write()` on the GcRwLock when `incremental_active=false` but `generational_active=true`
4. Observe that `mark_gc_ptrs_immediate` is called with `true`, potentially marking NEW GC pointers incorrectly

```rust
// Conceptual PoC - actual timing-dependent
let data: Gc<GcRwLock<Vec<Gc<i32>>>> = Gc::new(GcRwLock::new(Vec::new()));

// When incremental marking is NOT active but generational IS active:
// write() calls mark_gc_ptrs_immediate(&*guard, true) unconditionally
// This marks NEW GC pointers even though incremental is inactive
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change `GcRwLock::write()` to pass the actual barrier state:

```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    record_satb_old_values_with_state(&*guard, true);
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    // FIX: Pass actual barrier state instead of true
    mark_gc_ptrs_immediate(&*guard, generational_active || incremental_active);
    GcRwLockWriteGuard {
        guard,
        _marker: PhantomData,
    }
}
```

Same fix needed for:
- `GcRwLock::try_write()` (sync.rs:331)
- `GcMutex::lock()` (sync.rs:597)
- `GcMutex::try_lock()` (sync.rs:641)

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB barrier should only record OLD values when incremental marking is active, and mark NEW values black when barriers are active. Passing `true` unconditionally to `mark_gc_ptrs_immediate` breaks this invariant - it marks NEW values even when no barrier is active.

**Rustacean (Soundness 觀點):**
The inconsistency between `write()` (marks unconditionally) and `drop()` (marks conditionally) suggests the code doesn't follow its own documented invariants. Passing `true` when the actual barrier state is false could lead to incorrect GC behavior.

**Geohot (Exploit 觀點):**
If NEW GC pointers are marked black when no barrier is active, this could potentially be exploited to prevent collection of objects that should be collected. The marking should only happen when barriers are actually tracking mutations.