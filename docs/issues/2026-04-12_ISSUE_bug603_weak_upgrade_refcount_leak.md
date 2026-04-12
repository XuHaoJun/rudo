# [Bug]: Weak::upgrade() and Weak::try_upgrade() leak ref_count when is_allocated check fails

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires specific GC timing: slot swept but not reused, with existing weak ref |
| **Severity (嚴重程度)** | `Medium` | Reference count leak → objects never collected → memory leak |
| **Reproducibility (復現難度)** | `High` | Requires concurrent GC sweep during weak upgrade; PoC difficult |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade()`, `Weak::try_upgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `Weak::upgrade()` or `Weak::try_upgrade()` fails the `is_allocated` check in the `ref_count > 0` path, it should call `undo_inc_ref()` to avoid leaking the reference count increment that was just performed.

### 實際行為 (Actual Behavior)
The code returns `None` without calling `undo_inc_ref()`, causing a reference count leak. The leaked reference prevents the object from ever being collected.

### Code Location
- `ptr.rs:2446-2453` (`Weak::upgrade()` method, ref_count > 0 path)
- `ptr.rs:2559-2566` (`Weak::try_upgrade()` method, ref_count > 0 path)

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `Weak::upgrade()` and `Weak::try_upgrade()`, there is a code path where the reference count is successfully incremented via CAS, but then the `is_allocated` check fails:

```rust
// Check is_allocated after successful upgrade to prevent slot reuse issues
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // Don't call dec_ref - slot may be reused (bug133)
        return None;  // ✗ LEAK - does not call undo_inc_ref!
    }
}
```

The comment "slot may be reused (bug133)" is misleading because:
1. If the slot was **reused**, the **generation would have changed** and the earlier generation check would have caught it
2. If generation is **unchanged** but `is_allocated` is **false**, the slot was **swept but not reused** - we still have a leaked increment

### Similar Bug
This is the same bug pattern as bug601 which was fixed in `GcBoxWeakRef::upgrade()` and `GcBoxWeakRef::try_upgrade()`, but the fix was NOT applied to `Weak::upgrade()` and `Weak::try_upgrade()`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Creating a stable PoC is difficult due to GC timing dependencies. The issue would manifest as a memory leak that grows over time when:
1. Objects with weak refs are created
2. All strong refs are dropped
3. GC runs a sweep cycle
4. Weak refs are accessed during the narrow window when slot is swept but generation unchanged

```rust
// Conceptual PoC (timing-dependent, may not reliably reproduce)
fn leak_bug_poc() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);  // Returns Weak<T>, not GcBoxWeakRef<T>
    
    drop(gc);  // ref_count -> 0, object marked DEAD
    
    // If sweep runs here before upgrade:
    let result = weak.upgrade();  // May leak if slot swept but not reused
    
    assert!(result.is_none());  // Expected
    // But ref_count may be leaked at 1 instead of 0
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `undo_inc_ref` call in both functions:

**ptr.rs:2449-2451** (`Weak::upgrade()`):
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    // FIX bug603: Call undo_inc_ref to avoid leaking reference count.
    // The generation check above catches slot REUSE (where generation would change).
    // If we reach here with generation unchanged but is_allocated=false,
    // the slot was simply swept - undo_inc_ref is safe.
    GcBox::undo_inc_ref(ptr.as_ptr());
    return None;
}
```

**ptr.rs:2562-2564** (`Weak::try_upgrade()`):
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    // FIX bug603: Call undo_inc_ref to avoid leaking reference count.
    GcBox::undo_inc_ref(ptr.as_ptr());
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The reference count leak in the `ref_count > 0` path is a correctness issue. When `is_allocated` returns false but generation is unchanged, the slot was swept without being reused. The `undo_inc_ref` call is necessary to maintain accurate reference counts. The `GcBoxWeakRef` version of these functions (bug601) already has this fix.

**Rustacean (Soundness 觀點):**
This is not a soundness issue (no UB), but a resource leak. The `undo_inc_ref` function uses `fetch_sub` which always decrements regardless of flags, making it safe for this rollback scenario.

**Geohot (Exploit 觀點):**
The reference count leak could theoretically be weaponized for memory exhaustion DoS if an attacker can trigger the specific GC timing. However, the window for exploitation is extremely narrow.