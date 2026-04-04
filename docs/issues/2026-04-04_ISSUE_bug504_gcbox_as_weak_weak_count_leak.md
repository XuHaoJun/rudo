# [Bug]: GcBox::as_weak Weak Count Leak - dec_weak Not Called When Slot Swept

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Concurrent GC with lazy sweep can trigger this race window |
| **Severity (嚴重程度)** | Critical | Weak reference count leak leads to memory never being reclaimed |
| **Reproducibility (復現難度)** | Medium | Requires concurrent lazy sweep to trigger |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak` in `crates/rudo-gc/src/ptr.rs`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `GcBox::as_weak` detects the slot was swept (via `is_allocated` returning false) after `inc_weak` was called, it should call `dec_weak` to undo the `inc_weak` call, then return a null `GcBoxWeakRef`.

### 實際行為 (Actual Behavior)
When `is_allocated` returns false after `inc_weak` was called, the function returns a null weak reference WITHOUT calling `dec_weak`. This causes a weak reference count leak:
- Weak references never get cleaned up (memory leak)
- The object may never be reclaimed if only weak refs remain (due to leaked weak_count)

This is the same bug pattern as bug331 (GcHandle::try_resolve_impl ref_count leak) and bug332 (GcHandle::downgrade weak_count leak) but in `GcBox::as_weak`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `GcBox::as_weak` - ptr.rs:575-605:

```rust
// Line 586: inc_weak is called
(*NonNull::from(self).as_ptr()).inc_weak();

// Lines 589-592: Generation check catches slot REUSE
if pre_generation != (*NonNull::from(self).as_ptr()).generation() {
    (*NonNull::from(self).as_ptr()).dec_weak();  // ← CORRECTLY undoes inc_weak
    return GcBoxWeakRef::null();
}

// Lines 597-600: BUG - is_allocated returns false but dec_weak NOT called!
if !(*header.as_ptr()).is_allocated(idx) {
    // Don't call dec_weak - slot may be reused (bug133)  ← WRONG COMMENT!
    return GcBoxWeakRef::null();  // ← BUG: weak_count leaked!
}
```

The comment at line 598 claims "Don't call dec_weak - slot may be reused (bug133)" but this is incorrect:
1. If the slot was REUSED, the generation would have changed at line 589
2. If we reach line 597 with generation unchanged but is_allocated=false, the slot was simply SWEPT and added to free list (NOT reused)
3. In this case, we MUST call dec_weak to balance our inc_weak

The race condition:
1. Thread calls `as_weak()` on a GcBox
2. Object passes initial liveness checks and generation check
3. `inc_weak()` is called (line 586) - increments weak_count
4. **Between step 3 and the is_allocated check**, lazy sweep runs and adds the slot to free list (is_allocated=false)
5. `is_allocated` returns false (line 597)
6. Function returns null weak handle WITHOUT calling `dec_weak` - BUG!

Consequences:
- weak_count is leaked at 1 on a slot that's in the free list
- During sweep_phase2, since weak_count > 0, the slot is NOT reclaimed
- When a new object is later allocated in this slot, weak_count is still 1
- Next GC cycle: slot appears to have weak refs, prevents proper reclamation

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This requires concurrent lazy sweep:
1. Create Gc object with only weak references
2. Start background thread doing continuous lazy sweep
3. Main thread calls `Gc::downgrade()` / `Gc::as_weak()` in tight loop
4. Observe weak_count increasing without bound (memory leak)

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `dec_weak` before returning null when slot is not allocated:

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    (*NonNull::from(self).as_ptr()).dec_weak();  // Undo the inc_weak
    return GcBoxWeakRef::null();
}
```

The generation check at line 589 already catches slot REUSE (where dec_weak would target wrong object). If we reach line 597 with generation unchanged but is_allocated=false, the slot was simply swept - dec_weak is safe.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic reference count leak bug. The GC must ensure that every `inc_weak` has a corresponding `dec_weak`. The comment referencing "bug133" is a misapplication - that fix was about TOCTOU ordering, not about skipping reference count operations. This leak will cause objects with only weak references to never be collected, leading to memory growth over time.

**Rustacean (Soundness 觀點):**
This is a memory leak bug, not a safety violation (no UB). The weak reference count corruption scenario could lead to incorrect weak reference behavior. The code incorrectly reasons that skipping `dec_weak` protects against slot reuse, but this reasoning is backwards - the proper fix is to always balance inc_weak/dec_weak when is_allocated fails with unchanged generation.

**Geohot (Exploit 觀點):**
The exploitation path would require:
1. Controlling the timing of lazy sweep (difficult but possible with GcScheduler knobs)
2. Allocating a new object in the swept slot
3. The leaked weak_count would keep the old object alive even after all weak refs are dropped

---

## 🔗 相關 Issue

- bug331: GcHandle::try_resolve_impl - ref_count leak (Fixed)
- bug332: GcHandle::downgrade - weak_count leak (Fixed)
- bug400: GcBox::as_weak missing generation check (Fixed)
- bug240: GcBox::as_weak TOCTOU (Fixed)
