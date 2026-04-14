# [Bug]: Gc::downgrade() uses dec_weak() instead of undo_inc_weak() causing weak count leak

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Race condition requires concurrent slot sweep during downgrade |
| **Severity (嚴重程度)** | `High` | Weak reference count leak leads to memory leaks; repeated leaks can exhaust memory |
| **Reproducibility (復現難度)** | `Medium` | Requires precise timing with concurrent GC and lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::downgrade()`, `GcBox::as_weak()`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

`Gc::downgrade()` and `GcBox::as_weak()` use `dec_weak()` to undo `inc_weak()` on error paths, but `dec_weak()` has **early-return semantics** that can skip the decrement when `weak_count == 1`. This causes weak reference count leaks.

### 預期行為 (Expected Behavior)
When `inc_weak()` is called and subsequently an error is detected (generation mismatch or slot swept), the weak count should be decremented unconditionally via `undo_inc_weak()` to exactly undo the increment.

### 實際行為 (Actual Behavior)
`dec_weak()` is called instead. When `weak_count == 1`, `dec_weak()` CASs directly to flags and returns `true` without decrementing. If a concurrent operation changes `weak_count` between CAS attempts, the decrement may be skipped entirely.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug exists in two locations:

**1. `Gc::downgrade()` at ptr.rs:1854 and 1861:**
```rust
// Line 1853-1855
if pre_generation != (*gc_box_ptr).generation() {
    (*gc_box_ptr).dec_weak();  // BUG: should use undo_inc_weak()
    panic!(...);
}

// Line 1860-1862
if !(*header.as_ptr()).is_allocated(idx) {
    (*gc_box_ptr).dec_weak();  // BUG: should use undo_inc_weak()
    panic!(...);
}
```

**2. `GcBox::as_weak()` at ptr.rs:617:**
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    // FIX bug504: Call dec_weak to undo inc_weak.
    // The generation check above catches slot REUSE (where dec_weak would
    // target the wrong object). If we reach here with generation unchanged
    // but is_allocated=false, the slot was simply swept - dec_weak is safe.
    (*NonNull::from(self).as_ptr()).dec_weak();  // BUG: should use undo_inc_weak()
    return GcBoxWeakRef::null();
}
```

**The Problem with `dec_weak()`:**
At ptr.rs:410-438, `dec_weak()` has early-return semantics when `weak_count == 1`:
```rust
pub fn dec_weak(&self) -> bool {
    loop {
        let current = self.weak_count.load(Ordering::Acquire);
        let flags = current & Self::FLAGS_MASK;
        let count = current & !Self::FLAGS_MASK;

        if count == 0 {
            return false;  // Early return - decrement skipped!
        } else if count == 1 {
            // CAS directly to flags, skipping decrement
            match self.weak_count.compare_exchange_weak(...) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
        // ...
    }
}
```

**The Fix:**
Use `undo_inc_weak()` (ptr.rs:289-294) which unconditionally decrements:
```rust
pub(crate) unsafe fn undo_inc_weak(self_ptr: *mut Self) {
    (*self_ptr).weak_count.fetch_sub(1, Ordering::Release);
}
```

This issue was previously fixed for:
- `GcBoxWeakRef::clone()` - commit 222dc82 (bug613)
- `GcHandle::downgrade()` - commit a6b36fd (bug623)

But the same fix was **not applied** to `Gc::downgrade()` and `GcBox::as_weak()`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudocode - requires precise timing with concurrent GC
fn reproduce_weak_leak() {
    // Create a Gc and immediately downgrade it
    let gc = Gc::new(Data { value: 42 });
    
    // Race: trigger lazy sweep to reclaim the slot AFTER inc_weak but BEFORE downgrade returns
    // This requires concurrent GC and precise thread scheduling
    let weak = gc.downgrade();
    
    // If the race condition triggers, weak_count is leaked
    // Verify by checking that dropping the weak doesn't properly free memory
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. Change `Gc::downgrade()` at ptr.rs:1854 and 1861 to use `undo_inc_weak()`:
```rust
if pre_generation != (*gc_box_ptr).generation() {
    unsafe { (*gc_box_ptr).undo_inc_weak() };  // Use undo_inc_weak
    panic!(...);
}
// and
if !(*header.as_ptr()).is_allocated(idx) {
    unsafe { (*gc_box_ptr).undo_inc_weak() };  // Use undo_inc_weak
    panic!(...);
}
```

2. Change `GcBox::as_weak()` at ptr.rs:617 to use `undo_inc_weak()`:
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    unsafe { (*NonNull::from(self).as_ptr()).undo_inc_weak() };
    return GcBoxWeakRef::null();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The weak reference count leak is a memory management issue. When `dec_weak()` skips the decrement, the weak count never reaches zero even after all weak references are dropped. This prevents the `GcBox` from being properly finalized, potentially causing memory leaks. In a generational GC, this could also affect remembered set accuracy if weak refs aren't properly cleaned up.

**Rustacean (Soundness 觀點):**
This is not a direct soundness violation since weak refs don't affect memory safety directly. However, the `dec_weak()` early-return when `weak_count == 1` is a subtle bug that violates the expected semantics of "undo increment." The fix is straightforward - use `undo_inc_weak()` which is an unconditional `fetch_sub`.

**Geohot (Exploit 觀點):**
While not directly exploitable for code execution, the weak count leak could be weaponized via memory exhaustion. An attacker who can trigger the race condition repeatedly could leak weak reference counts, preventing GC cleanup and eventually exhausting memory. The race window is small but achievable with concurrent GC threads.