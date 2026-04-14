# [Bug]: GcCell::generational_write_barrier small object path missing third is_allocated check before set_dirty

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep and write barrier on small object |
| **Severity (嚴重程度)** | High | Could set dirty bit on wrong slot after sweep and reuse |
| **Reproducibility (復現難度)** | High | Requires precise timing of slot reuse during lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::generational_write_barrier` (cell.rs small object path)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

In `GcCell::generational_write_barrier`, the **small object path** is missing a third `is_allocated` check AFTER the generation check but BEFORE calling `set_dirty`. This creates a TOCTOU race where:

1. Slot passes first `is_allocated` check
2. Slot passes second `is_allocated` check (before reading `has_gen_old`)
3. Slot passes generation check and `has_gen_old` read
4. **Between generation check and set_dirty**: slot is swept and reused
5. `set_dirty` is called on the wrong (new) object

### 預期行為

The barrier should verify slot is still allocated before calling `set_dirty`, similar to `incremental_write_barrier` which has a third `is_allocated` check before recording to remembered buffer.

### 實際行為

`generational_write_barrier` small object path calls `set_dirty` without verifying the slot wasn't swept between the generation check and the dirty bit set.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Buggy code in cell.rs `generational_write_barrier` small object path (lines 1420-1441):**

```rust
if index < (*header.as_ptr()).obj_count as usize {
    // Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
    if !(*header.as_ptr()).is_allocated(index) {      // FIRST CHECK
        return;
    }
    // Second is_allocated check - prevents TOCTOU race (bug459)
    if !(*header.as_ptr()).is_allocated(index) {      // SECOND CHECK
        return;
    }
    // GEN_OLD early-exit: skip if page young AND object has no gen_old_flag
    let gc_box_addr = (header_page_addr + header_size + index * block_size)
        as *const GcBox<()>;
    let has_gen_old = (*gc_box_addr).has_gen_old_flag();
    if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0
        && !has_gen_old
    {
        return;
    }
    // MISSING: Third is_allocated check HERE before set_dirty!
    (*header.as_ptr()).set_dirty(index);  // TOCTOU: slot could be swept now!
    heap.add_to_dirty_pages(header);
}
```

**Compare to `incremental_write_barrier` which has the correct pattern (lines 1348-1361):**

```rust
// Third is_allocated check AFTER has_gen_old read - prevents TOCTOU (bug498).
// Must verify slot is still allocated before recording to remembered set.
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
heap.record_in_remembered_buffer(header);
```

**The race scenario:**
1. Thread A: First `is_allocated` passes at line 1422
2. Thread A: Second `is_allocated` passes at line 1426
3. Thread A: `has_gen_old` read and generation check passes
4. Thread B: Lazy sweep deallocates the slot
5. Thread C: New allocation reuses the slot
6. Thread A: Calls `set_dirty` on the new object (wrong!)
7. Thread A: Adds page to dirty_pages list

The dirty bit is now incorrectly set for the new object, which shouldn't be in the dirty set at all.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable generational barrier and lazy sweep
2. Allocate a small object in old generation (has `generation > 0`)
3. Create a reference via `GcCell::borrow_mut` to trigger `generational_write_barrier`
4. Trigger lazy sweep to reclaim the small object slot concurrently
5. Allocate a new object in the reused slot
6. The barrier may call `set_dirty` on the new object incorrectly

**Note:** This is a race condition that requires precise timing. Similar to bug498/bug499 which added third `is_allocated` checks to `incremental_write_barrier`.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add a third `is_allocated` check in `GcCell::generational_write_barrier` small object path, after the generation check but before `set_dirty`:

```rust
if index < (*header.as_ptr()).obj_count as usize {
    // Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247).
    if !(*header.as_ptr()).is_allocated(index) {
        return;
    }
    // Second is_allocated check - prevents TOCTOU race (bug459)
    if !(*header.as_ptr()).is_allocated(index) {
        return;
    }
    // GEN_OLD early-exit: skip if page young AND object has no gen_old_flag
    let gc_box_addr = (header_page_addr + header_size + index * block_size)
        as *const GcBox<()>;
    let has_gen_old = (*gc_box_addr).has_gen_old_flag();
    if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0
        && !has_gen_old
    {
        return;
    }
    // FIX bug520: Third is_allocated check before set_dirty - prevents TOCTOU.
    // If slot was swept after generation check but before set_dirty,
    // we'd set dirty bit on wrong slot.
    if !(*header.as_ptr()).is_allocated(index) {
        return;
    }
    (*header.as_ptr()).set_dirty(index);
    heap.add_to_dirty_pages(header);
}
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

- bug498: `incremental_write_barrier` small object path missing third is_allocated check (fixed)
- bug499: `incremental_write_barrier` large object path missing third is_allocated check (fixed)
- bug459: `incremental_write_barrier` missing second is_allocated check (fixed)
- bug247: Initial is_allocated check before has_gen_old read