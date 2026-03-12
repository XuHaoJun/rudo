# [Bug]: mark_page_dirty_for_ptr and unified_write_barrier read has_gen_old_flag before is_allocated check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Occurs when slot is reused after deallocation during concurrent GC |
| **Severity (嚴重程度)** | High | Reads from deallocated memory - potential UB |
| **Reproducibility (復現難度)** | Medium | Requires specific timing with lazy sweep and slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap.rs` - `mark_page_dirty_for_ptr`, `unified_write_barrier`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
Call `has_gen_old_flag()` only AFTER verifying the slot is allocated via `is_allocated()` check.

### 實際行為
Functions `mark_page_dirty_for_ptr` and `unified_write_barrier` read `has_gen_old_flag()` from GcBox BEFORE checking `is_allocated()`. This can cause reading from a deallocated/reused slot, leading to undefined behavior.

This is the same bug pattern as:
- bug278: `gc_cell_validate_and_barrier` has_gen_old_flag read before is_allocated check
- bug282: `incremental_write_barrier` has_gen_old_flag read before is_allocated check

But these specific locations were not documented.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `mark_page_dirty_for_ptr` (heap.rs:2729 and 2754):
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 2729/2754 - READ BEFORE CHECK
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
// ... later ...
if !(*header.as_ptr()).is_allocated(index) {  // LINE 2762 - is_allocated check comes AFTER
    return;
}
```

In `unified_write_barrier` (heap.rs:2944 and 2969):
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 2944/2969 - READ BEFORE CHECK
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
// ... later ...
if !(*header.as_ptr()).is_allocated(index) {  // LINE 2977 - is_allocated check comes AFTER
    return;
}
```

The slot may have been deallocated and reused by the time `has_gen_old_flag()` is called, causing UB.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This bug is latent and requires specific timing:
1. Allocate object in young generation
2. Object gets promoted to old generation (gen_old_flag set)
3. Object gets swept (deallocated) during lazy sweep
4. Slot gets reused for new allocation
5. Write barrier fires on the new object - reads stale gen_old_flag from reused slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the `is_allocated` check BEFORE calling `has_gen_old_flag()`:

```rust
// Skip if slot was swept FIRST
if !(*header.as_ptr()).is_allocated(index) {
    return;
}

// THEN check gen_old_flag
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generational barrier relies on accurate gen_old_flag to skip unnecessary barrier work. Reading from a deallocated slot could return arbitrary values, causing either: (1) unnecessary barrier work if stale flag indicates young, or (2) missed barriers if stale flag indicates old. Both degrade GC correctness.

**Rustacean (Soundness 觀點):**
Reading `has_gen_old_flag()` from a deallocated/reused slot is undefined behavior in Rust. The slot's memory may contain arbitrary bit patterns that could cause: (1) panic from unexpected flag combinations, (2) incorrect barrier decisions leading to memory leaks or use-after-free.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of slot reuse, they might influence the gen_old_flag value. Combined with other vulnerabilities, this could potentially lead to exploit primitives. However, the primary risk is GC correctness failure causing memory leaks or crashes.
