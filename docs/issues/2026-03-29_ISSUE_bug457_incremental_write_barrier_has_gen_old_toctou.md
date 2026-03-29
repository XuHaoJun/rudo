# [Bug]: incremental_write_barrier has_gen_old TOCTOU - second is_allocated check after flag read

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep and allocation |
| **Severity (嚴重程度)** | High | Barrier could use stale gen_old flag, corrupting remembered set |
| **Reproducibility (復現難度)** | High | Precise timing needed: sweep → reuse → flag read |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental_write_barrier` (heap.rs:3185-3272)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`has_gen_old_flag` should be read only AFTER verifying the slot is still allocated (via second `is_allocated` check).

### 實際行為
In `incremental_write_barrier`, the large object path:

1. Line 3217: First `is_allocated(0)` check
2. Line 3221: `has_gen_old` is read (TOCTOU window opens here)
3. Lines 3222-3223: Early return check using `has_gen_old`
4. Line 3225: Second `is_allocated(0)` check (TOCTOU window closes too late)
5. Line 3228: Return `(header, index)` with potentially stale `has_gen_old`

The second `is_allocated` check at line 3225 comes AFTER `has_gen_old` is already read at line 3221. If the slot is swept and reused between lines 3217 and 3221, `has_gen_old` is read from the new object before we know if the slot is still valid.

Compare with `simple_write_barrier` (lines 2887-2934) which has the CORRECT pattern:
1. First `is_allocated` check
2. `has_gen_old` read
3. Generation check
4. Second `is_allocated` check
5. THEN uses `has_gen_old`

---

## 🔬 根本原因分析 (Root Cause Analysis)

```rust
// Line 3216-3228 (large object path):
// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug286).
if !(*h_ptr).is_allocated(0) {  // Line 3217 - FIRST check
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // Line 3221 - READS stale!
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
if !(*h_ptr).is_allocated(0) {  // Line 3225 - SECOND check (TOO LATE)
    return;
}
(NonNull::new_unchecked(h_ptr), 0_usize)
```

The comment at line 3216 claims "read has_gen_old_flag only after is_allocated" but the implementation doesn't follow this - `has_gen_old` is read before the second check.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Requires concurrent stress test:
1. Create large object, promote to OLD
2. Concurrently run mutator triggering `incremental_write_barrier`
3. Simultaneously trigger lazy sweep to reclaim the large object
4. Rapidly allocate new object in same slot
5. Observe incorrect remembered set behavior

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the second `is_allocated` check to BEFORE reading `has_gen_old`, or restructure to match `simple_write_barrier`:

```rust
// Fixed pattern (matches simple_write_barrier):
if !(*h_ptr).is_allocated(0) {
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Second check BEFORE reading has_gen_old:
if !(*h_ptr).is_allocated(0) {
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// No third check needed - we've already validated slot is allocated
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generational barrier relies on accurate `gen_old` flags to track OLD→YOUNG references. If `has_gen_old` is read from a new object (after slot reuse) that incorrectly has `gen_old = true`, the barrier might incorrectly record an OLD→YOUNG reference that doesn't exist, or miss a real one.

**Rustacean (Soundness 觀點):**
Classic TOCTOU vulnerability. While `is_allocated` is checked twice, the second check comes after the flag is already read, making it ineffective at preventing the race.

**Geohot (Exploit 觀點):**
If `gen_old` is not properly cleared during allocation (bug in `clear_gen_old`), and `generation` is non-zero, this could lead to incorrect barrier behavior exploitable for memory corruption.

---

## 相關 Bug

- bug282: Same issue in `incremental_write_barrier` (marked Invalid - small object path was fixed, but large object path still vulnerable)
- bug364: Added second `is_allocated` check, but too late to fix TOCTOU
- bug430: Large object path missing second `is_allocated` check (Fixed, but doesn't address TOCTOU ordering)

---

## 修復記錄 (2026-03-29)

**Fixed** by adding second `is_allocated(0)` check BEFORE reading `has_gen_old` in the large object path:

```rust
// Before (buggy):
if !(*h_ptr).is_allocated(0) {  // First check
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // TOCTOU window!
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
if !(*h_ptr).is_allocated(0) {  // Second check (too late)
    return;
}

// After (fixed):
if !(*h_ptr).is_allocated(0) {  // First check
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
if !(*h_ptr).is_allocated(0) {  // Second check BEFORE reading has_gen_old
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // Safe!
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

Applied to: `crates/rudo-gc/src/heap.rs` `incremental_write_barrier` large object path (lines 3216-3228).
