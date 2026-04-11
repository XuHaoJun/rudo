# [Bug]: upgrade() ref_count>0 path leaks ref_count when is_allocated check fails

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires specific GC timing: slot swept but not reused, with existing weak ref |
| **Severity (嚴重程度)** | `Medium` | Reference count leak → objects never collected → memory leak |
| **Reproducibility (復現難度)** | `High` | Requires concurrent GC sweep during weak upgrade; PoC difficult |

---

## 🧩 受影響組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()`, `GcBoxWeakRef::try_upgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `GcBoxWeakRef::upgrade()` or `try_upgrade()` fails the `is_allocated` check in the `ref_count > 0` path, it should call `undo_inc_ref()` to avoid leaking the reference count increment that was just performed.

### 實際行為 (Actual Behavior)
The code returns `None` without calling `undo_inc_ref()`, causing a reference count leak. The leaked reference prevents the object from ever being collected.

### Code Location
- `ptr.rs:772-779` (`upgrade()` method, ref_count > 0 path)
- `ptr.rs:1024-1031` (`try_upgrade()` method, ref_count > 0 path)

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `GcBoxWeakRef::upgrade()` and `try_upgrade()`, there are two code paths for incrementing the reference count:

1. **try_inc_ref_from_zero path** (lines 739-747): When `ref_count == 0` and object is being resurrected:
   ```rust
   if !(*header.as_ptr()).is_allocated(idx) {
       crate::ptr::GcBox::undo_inc_ref(ptr.as_ptr());  // ✓ Correctly undoes
       return None;
   }
   ```

2. **ref_count > 0 path** (lines 772-779): When there are existing strong refs:
   ```rust
   if !(*header.as_ptr()).is_allocated(idx) {
       // Don't call dec_ref - slot may be reused (bug133)
       return None;  // ✗ LEAK - does not call undo_inc_ref!
   }
   ```

The `ref_count > 0` path is missing the `undo_inc_ref()` call. This is inconsistent with the `try_inc_ref_from_zero` path which was fixed in bug525.

The comment "slot may be reused (bug133)" is misleading because:
1. If the slot was **reused**, the **generation would have changed** and the earlier generation check at line 761 would have caught it
2. If generation is **unchanged** but `is_allocated` is **false**, the slot was **swept but not reused** - we still have a leaked increment

### Code Flow Analysis
1. `pre_generation = gc_box.generation()` (line 756)
2. `try_inc_ref_if_nonzero()` succeeds (line 757) - ref_count atomically incremented
3. Generation check passes - `pre_generation == gc_box.generation()` (line 761)
4. `dropping_state()` and `has_dead_flag()` checks pass (line 768)
5. `is_allocated` check **fails** - slot was swept (line 772-778)
6. **Return without undo_inc_ref** → reference count leak

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
    let weak = Gc::downgrade(&gc);
    
    drop(gc);  // ref_count -> 0, object marked DEAD
    
    // If sweep runs here before upgrade:
    let result = weak.upgrade();  // May leak if slot swept but not reused
    
    assert!(result.is_none());  // Expected
    // But ref_count may be leaked at 1 instead of 0
}
```

**Note**: Per the bug hunting guidelines, this is a reference leak (not UAF or soundness issue), so it may not cause immediate observable failure. The symptom is gradual memory growth.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `undo_inc_ref` call in the `ref_count > 0` path at `ptr.rs:775-777`:

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    // FIX bug601: Call undo_inc_ref to avoid leaking reference count.
    // The generation check above catches slot REUSE (where generation would change).
    // If we reach here with generation unchanged but is_allocated=false,
    // the slot was simply swept - undo_inc_ref is safe.
    GcBox::undo_inc_ref(ptr.as_ptr());
    return None;
}
```

Apply the same fix to `try_upgrade()` at `ptr.rs:1027-1029`.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The reference count leak in the `ref_count > 0` path is a correctness issue. When `is_allocated` returns false but generation is unchanged, the slot was swept without being reused. The `undo_inc_ref` call is necessary to maintain accurate reference counts. The `try_inc_ref_from_zero` path (fixed in bug525) already has this call - the `ref_count > 0` path should be consistent.

**Rustacean (Soundness 觀點):**
This is not a soundness issue (no UB), but a resource leak. The `undo_inc_ref` function uses `fetch_sub` which always decrements regardless of flags, making it safe for this rollback scenario. The existing code at line 768 already uses `undo_inc_ref` correctly for the `dropping_state`/`has_dead_flag` check failure case.

**Geohot (Exploit 觀點):**
The reference count leak could theoretically be weaponized for memory exhaustion DoS if an attacker can trigger the specific GC timing. However, the window for exploitation is extremely narrow (between sweep marking slot unallocated and generation increment on reuse). Not a practical exploit vector.

---

## 📎 相關修復記錄 (Related Fix History)

- **bug525**: Fixed same issue in `try_inc_ref_from_zero` path (added is_allocated check with undo_inc_ref)
- **bug133**: Original reason for not calling dec_ref in this path - may need re-evaluation since reasoning appears flawed