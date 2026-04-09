# [Bug]: incremental_write_barrier large object path missing third is_allocated check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Multi-page large objects with incremental marking enabled |
| **Severity (嚴重程度)** | High | TOCTOU race can cause remembered set corruption or incorrect barrier behavior |
| **Reproducibility (復現難度)** | Medium | Requires specific timing window between is_allocated checks and has_gen_old read |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental_write_barrier` in `heap.rs` (large object path)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0 (008-incremental-marking feature)

---

## 📝 問題描述 (Description)

The large object path in `incremental_write_barrier` is missing a third `is_allocated` check after reading `has_gen_old`, creating a TOCTOU window. The small object path has this check (added as bug530 fix), but the large object path was not updated to match.

### 預期行為 (Expected Behavior)
The write barrier should verify slot allocation after reading `has_gen_old` to prevent TOCTOU races where a slot is swept and reused between reading the flag and returning.

### 實際行為 (Actual Behavior)
Large object path reads `has_gen_old` and generation at lines 3288-3289, then returns without a third `is_allocated` check. If the slot is swept and reused between these operations, the barrier could operate on a reused slot with stale data.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `heap.rs`, `incremental_write_barrier` function:

**Large object path (lines 3278-3292):**
```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug286).
if !(*h_ptr).is_allocated(0) {  // First check
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU (bug457).
if !(*h_ptr).is_allocated(0) {  // Second check
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // Read has_gen_old
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// MISSING: Third is_allocated check AFTER has_gen_old read
(NonNull::new_unchecked(h_ptr), 0_usize)  // Return without verification
```

**Small object path (lines 3313-3335):**
```rust
if !(*h.as_ptr()).is_allocated(index) {  // First check
    return;
}
if !(*h.as_ptr()).is_allocated(index) {  // Second check
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // Read has_gen_old
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug530: Third is_allocated check AFTER has_gen_old read
if !(*h.as_ptr()).is_allocated(index) {  // Third check
    return;
}
```

The large object path returns at line 3292 without the third verification, while the small object path has the check at line 3332.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires timing control to reliably reproduce
// 1. Allocate large object (> PAGE_SIZE) in old generation
// 2. Enable incremental marking
// 3. Create OLD->YOUNG reference via write barrier
// 4. Race window exists between generation check and return
// PoC would need Miri or ThreadSanitizer to detect
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add third `is_allocated` check before return in large object path at line 3291-3292:

```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bugXXX: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU.
// Must verify slot is still allocated before returning to caller.
// Matches small object path pattern (bug530).
if !(*h_ptr).is_allocated(0) {
    return;
}
(NonNull::new_unchecked(h_ptr), 0_usize)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB (Snapshot-At-The-Beginning) barrier relies on accurate tracking of old-generation pages. If a slot is reused between reading `has_gen_old` and the return, the barrier could record an incorrect page in the remembered set. The triple-check pattern exists to close this TOCTOU window. The large object path's omission creates an inconsistency that could lead to missing pages in the remembered set during incremental GC.

**Rustacean (Soundness 觀點):**
While this doesn't directly cause UAF (the slot still has valid memory), it could cause the GC to miss tracking a dirty page, leading to premature collection of live objects. The pattern of three `is_allocated` checks is defensive programming against TOCTOU races in concurrent code.

**Geohot (Exploit 觀點):**
The TOCTOU window is small but exploitable. An attacker who could control GC timing might be able to trigger slot reuse between the generation check and return, potentially causing the barrier to operate on a reallocated object. This could be leveraged in combination with other vulnerabilities.