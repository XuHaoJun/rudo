# [Bug]: GcHandle::downgrade uses dec_weak instead of undo_inc_weak causing ref_count leak

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Generation change during downgrade triggers the bug path |
| **Severity (嚴重程度)** | `Medium` | Ref count leak leads to memory leaks, not UAF |
| **Reproducibility (復現難度)** | `Medium` | Requires slot reuse between pre-generation check and dec_weak |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade` (handles/cross_thread.rs)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

In `GcHandle::downgrade`, when the generation check detects slot reuse (lines 642-650, 592-601, 543-550), the code calls `dec_weak()` to undo the `inc_weak()`. However, `dec_weak()` has early-return semantics when `weak_count == 1` that can cause the decrement to be skipped, leading to a ref_count leak.

### 預期行為 (Expected Behavior)
When `inc_weak()` is undone due to generation mismatch, the weak count should be decremented via direct subtraction (`undo_inc_weak`).

### 實際行為 (Actual Behavior)
When `inc_weak()` is undone, `dec_weak()` is called instead. If `weak_count == 1` at that moment (the 1 we just added), `dec_weak()` performs a CAS directly to flags without decrementing, leaving the weak count incorrectly at 1. This causes subsequent drops to leak.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `crates/rudo-gc/src/handles/cross_thread.rs` at three locations (lines 544, 594, 643):

```rust
// Get generation BEFORE inc_weak to detect slot reuse (bug351).
let pre_generation = (*self.ptr.as_ptr()).generation();

(*self.ptr.as_ptr()).inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*self.ptr.as_ptr()).generation() {
    (*self.ptr.as_ptr()).dec_weak();  // BUG: Should use undo_inc_weak
    // ...
}
```

The issue:
1. `inc_weak()` increments weak_count from X to X+1
2. If generation changed (slot reused), we want to undo by setting weak_count back to X
3. But `dec_weak()` with weak_count == 1 does CAS(1, flags) directly - it does NOT decrement!
4. This is because `dec_weak()` at count==1 goes: `count=1 -> compare_exchange -> flags` (no subtraction)

This pattern was **already fixed** for similar cases:
- `ptr.rs:855,870` - Uses `undo_inc_weak` (bug613 fix)
- `ptr.rs:298` - Uses `undo_inc_ref` (bug483 fix)
- `ptr.rs:352` - Uses `undo_inc_ref` (bug287 fix)

But the three locations in `cross_thread.rs` still use the incorrect `dec_weak()`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC requires:
// 1. Create a GcHandle
// 2. Force slot reuse via GC pressure
// 3. Call downgrade during the narrow window when generation changes
// 4. Verify weak_count is incorrect after downgrade
```

The race window is extremely narrow but the pattern was confirmed in similar bugs (bug613, bug483).

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Replace `dec_weak()` with `undo_inc_weak()` in all three locations in `GcHandle::downgrade`:

```rust
if pre_generation != (*self.ptr.as_ptr()).generation() {
    unsafe { GcBox::undo_inc_weak(self.ptr.as_ptr()) }  // FIX
    // or: (*self.ptr.as_ptr()).undo_inc_weak() if public
    return WeakCrossThreadHandle { /* null */ };
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The dec_weak/undo_inc_weak issue is a classic early-return semantics problem. The GC uses reference counts for collection decisions, so an incorrect weak_count means objects may be collected prematurely or live forever.

**Rustacean (Soundness 觀點):**
This is not a UAF bug because dec_weak at count==1 still atomically sets the value, it just doesn't subtract. The object remains valid. However, subsequent drops may leak memory due to mismatched counts.

**Geohot (Exploit 觀點):**
While not an exploitable UAF, the ref_count leak could be weaponized via memory exhaustion DoS if an attacker can trigger the generation-change path repeatedly.