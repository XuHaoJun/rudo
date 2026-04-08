# [Bug]: unified_write_barrier missing third is_allocated check after has_gen_old read

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Can trigger when slot is swept between generation check and return |
| **Severity (嚴重程度)** | Medium | Could set dirty bit on wrong slot or corrupt data structures |
| **Reproducibility (復現難度)** | Medium | Requires precise timing between sweep and barrier |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unified_write_barrier` (heap.rs:3133-3224), small object path
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The `unified_write_barrier` function should have three `is_allocated` checks in its small object path (matching `gc_cell_validate_and_barrier` and `incremental_write_barrier` patterns):
1. First check before reading `has_gen_old` (bug463 fix)
2. Second check before reading `has_gen_old` (TOCTOU prevention)
3. **Third check AFTER reading `has_gen_old` and verifying generation** to ensure slot wasn't swept before returning

### 實際行為 (Actual Behavior)
The small object path in `unified_write_barrier` (lines 3192-3208) is missing the third `is_allocated` check after the generation check. Compare:

**gc_cell_validate_and_barrier** (lines 3091-3107) - HAS the fix:
```rust
// Second is_allocated check before reading has_gen_old
if !(*h).is_allocated(index) { return; }
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug531: Third is_allocated check AFTER has_gen_old read
if !(*h).is_allocated(index) { return; }
```

**incremental_write_barrier** (lines 3301-3323) - HAS the fix:
```rust
if !(*h.as_ptr()).is_allocated(index) { return; }
// Second is_allocated check BEFORE reading has_gen_old
if !(*h.as_ptr()).is_allocated(index) { return; }
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug530: Third is_allocated check AFTER has_gen_old read
if !(*h.as_ptr()).is_allocated(index) { return; }
```

**unified_write_barrier** (lines 3192-3208) - **MISSING the third check**:
```rust
if !(*h.as_ptr()).is_allocated(index) { return; }
// Second is_allocated check BEFORE reading has_gen_old
if !(*h.as_ptr()).is_allocated(index) { return; }
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// MISSING: Third is_allocated check AFTER has_gen_old read!
return (h, index);  // <-- Returns without verification
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `unified_write_barrier` function was updated to add the second `is_allocated` check (bug463 fix) but the third check was not added to match the pattern in `gc_cell_validate_and_barrier` (bug531) and `incremental_write_barrier` (bug530).

After reading `has_gen_old` and verifying the generation, the slot could be swept by concurrent lazy sweep before we return the `(header, index)` tuple. The third check is needed to prevent returning a stale slot index.

---

## 🛠️ 建議修復方案 (Suggested Fix)

Add the third `is_allocated` check in `unified_write_barrier` small object path, matching the pattern in `gc_cell_validate_and_barrier` (bug531) and `incremental_write_barrier` (bug530):

```rust
// After line 3207, add:
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The pattern of three `is_allocated` checks in write barriers is well-established for TOCTOU prevention. The `unified_write_barrier` function must match the consistency of `gc_cell_validate_and_barrier` and `incremental_write_barrier`.

**Rustacean (Soundness 觀點):**
Returning a stale slot index could lead to incorrect dirty tracking or memory corruption. This is a correctness issue, not just a performance issue.

**Geohot (Exploit 觀點):**
While this is primarily a correctness bug, a stale slot index could potentially be exploited in edge cases where an attacker could influence GC timing.

---

## 驗證記錄

**驗證日期:** 2026-04-08
**驗證人員:** opencode

### 驗證結果

1. Compared `unified_write_barrier` (heap.rs:3192-3208) with `gc_cell_validate_and_barrier` (heap.rs:3091-3107) - latter has third check
2. Compared with `incremental_write_barrier` (heap.rs:3301-3323) - has third check
3. `unified_write_barrier` small object path is missing the third `is_allocated` check after has_gen_old read
4. This matches the bug530/bug531 pattern but was not applied to `unified_write_barrier`

**Status: Open** - Needs fix similar to bug531 fix in gc_cell_validate_and_barrier