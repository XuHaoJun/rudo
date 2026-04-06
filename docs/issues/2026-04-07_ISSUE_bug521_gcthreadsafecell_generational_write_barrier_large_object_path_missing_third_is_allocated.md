# [Bug]: GcThreadSafeCell::generational_write_barrier large object path missing third is_allocated check before set_dirty

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep and write barrier on large object |
| **Severity (嚴重程度)** | High | Could set dirty bit on wrong slot after sweep and reuse |
| **Reproducibility (復現難度)** | High | Requires precise timing of slot reuse during lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::generational_write_barrier` (cell.rs large object path)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為

The `generational_write_barrier` function should have consistent `is_allocated` checks in both the large object and small object paths before calling `set_dirty`. The small object path was fixed for bug520, but the **large object path** is missing the third `is_allocated` check.

### 實際行為

In `GcThreadSafeCell::generational_write_barrier` **large object path** (lines 1388-1404), `set_dirty` is called without verifying the slot wasn't swept between the generation check and the dirty bit set.

**Buggy code (cell.rs 1388-1404):**
```rust
// Large object path - MISSING third is_allocated check!
if !(*header).is_allocated(0) {      // FIRST CHECK
    return;
}
// Second is_allocated check - prevents TOCTOU race (bug459)
if !(*header).is_allocated(0) {      // SECOND CHECK
    return;
}
// GEN_OLD early-exit check
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// MISSING: Third is_allocated check HERE before set_dirty!
(*header).set_dirty(0);  // TOCTOU: slot could be swept now!
heap.add_to_dirty_pages(NonNull::new_unchecked(header));
```

**Compare to small object path (lines 1420-1447) - CORRECT:**
```rust
if index < (*header.as_ptr()).obj_count as usize {
    if !(*header.as_ptr()).is_allocated(index) {      // FIRST CHECK
        return;
    }
    if !(*header.as_ptr()).is_allocated(index) {      // SECOND CHECK
        return;
    }
    // GEN_OLD early-exit check
    let has_gen_old = (*gc_box_addr).has_gen_old_flag();
    if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
        return;
    }
    // FIX bug520: Third is_allocated check before set_dirty
    if !(*header.as_ptr()).is_allocated(index) {      // THIRD CHECK - PRESENT
        return;
    }
    (*header.as_ptr()).set_dirty(index);
    heap.add_to_dirty_pages(header);
}
```

The large object path is missing this third check and is therefore vulnerable to the same TOCTOU bug that bug520 fixed for the small object path.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The race scenario for the large object path:
1. Thread A: First `is_allocated` passes at line 1389
2. Thread A: Second `is_allocated` passes at line 1393
3. Thread A: `has_gen_old` read and generation check passes at line 1400
4. **Between generation check (line 1400) and set_dirty (line 1403)**: slot is swept and potentially reused
5. `set_dirty` is called on the potentially wrong slot

The same pattern was fixed for:
- bug498: `incremental_write_barrier` small object path
- bug499: `incremental_write_barrier` large object path
- bug520: `GcCell::generational_write_barrier` small object path (only)

But `GcThreadSafeCell::generational_write_barrier` large object path was overlooked.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable generational barrier and lazy sweep
2. Allocate a large object in old generation
3. Create a reference via `GcThreadSafeCell::borrow_mut` to trigger `generational_write_barrier`
4. Trigger lazy sweep to reclaim the large object slot concurrently
5. Allocate a new object in the reused slot (if sweep reused without new allocation, generation would be same and check would pass anyway)
6. The barrier may call `set_dirty` on the new object incorrectly

**Note:** This is a race condition that requires precise timing.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add a third `is_allocated` check in `GcThreadSafeCell::generational_write_barrier` large object path, after the generation check but before `set_dirty`:

```rust
// FIX bug521: Add third is_allocated check before set_dirty - prevents TOCTOU.
if !(*header).is_allocated(0) {
    return;
}
(*header).set_dirty(0);
heap.add_to_dirty_pages(NonNull::new_unchecked(header));
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The dirty page tracking relies on accurate dirty bit setting. If a slot is reused and we incorrectly set the dirty bit, the GC may process the wrong slots during marking. This could lead to missing references or incorrect write barrier behavior.

**Rustacean (Soundness 觀點):**
This is a TOCTOU bug where we set a dirty bit on a slot that may have been swept and reused since our last check. While not a traditional memory safety issue, it corrupts GC state and could lead to incorrect collection.

**Geohot (Exploit 觀點):**
By controlling allocation patterns and timing, an attacker could potentially cause the dirty bit to be set on the wrong object, which might be exploitable in combination with other GC bugs.

---

## 相關 Bug

- bug520: `GcCell::generational_write_barrier` small object path missing third is_allocated check (fixed)
- bug498: `incremental_write_barrier` small object path missing third is_allocated check (fixed)
- bug499: `incremental_write_barrier` large object path missing third is_allocated check (fixed)
- bug459: `incremental_write_barrier` missing second is_allocated check (fixed)
- bug247: Initial is_allocated check before has_gen_old read