# [Bug]: Multiple barrier functions read has_gen_old_flag before is_allocated check

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
- **Component:** `heap.rs` - `mark_page_dirty_for_ptr`, `incremental_write_barrier`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
Call `has_gen_old_flag()` only AFTER verifying the slot is allocated via `is_allocated()` check.

### 實際行為
Multiple barrier functions read `has_gen_old_flag()` from GcBox BEFORE checking `is_allocated()`. This can cause reading from a deallocated/reused slot, leading to undefined behavior.

This is the same bug pattern as:
- bug278: `gc_cell_validate_and_barrier` has_gen_old_flag read before is_allocated check
- bug282: `incremental_write_barrier` has_gen_old_flag read before is_allocated check
- bug283: `mark_page_dirty_for_ptr` and `unified_write_barrier` have same issue

But there are additional locations not documented in bug283.

---

## 🔬 根本原因分析 (Root Cause Analysis)

### Location 1: mark_page_dirty_for_ptr - Large object path (heap.rs:2727-2732)

```rust
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Cache flag to avoid TOCTOU between check and barrier (bug149).
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 2729 - READS WITHOUT ANY is_allocated CHECK
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
(NonNull::new_unchecked(h_ptr), 0_usize)
```

**BUG**: For large objects, there's NO is_allocated check at all before reading has_gen_old_flag()!

### Location 2: mark_page_dirty_for_ptr - Small object path (heap.rs:2750-2757)

```rust
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
// Cache flag to avoid TOCTOU between check and barrier (bug149).
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 2754 - READS BEFORE is_allocated CHECK
if (*h.as_ptr()).generation == 0 && !has_gen_old {
    return;
}
```

The is_allocated check comes AFTER at line 2762.

### Location 3: incremental_write_barrier (heap.rs:3039-3051)

```rust
// GEN_OLD early-exit: skip only if page young AND object has no gen_old_flag (bug71).
// Cache flag to avoid TOCTOU between check and barrier (bug133).
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 3043 - READS BEFORE is_allocated CHECK
if (*header.as_ptr()).generation == 0 && !has_gen_old {
    return;
}

// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug220).
if !(*header.as_ptr()).is_allocated(index) {  // LINE 3049 - is_allocated check comes AFTER
    return;
}
```

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

### For Location 1 (large object path):
Add is_allocated check before reading has_gen_old_flag:

```rust
// First validate bounds
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + h_size + size {
    return;
}
let h_ptr = head_addr as *mut PageHeader;

// Validate MAGIC to ensure the large_object_map entry is valid (bug190).
if (*h_ptr).magic != MAGIC_GC_PAGE {
    return;
}

// Skip if slot was swept FIRST (NEW FIX)
if !(*h_ptr).is_allocated(0) {  // Large objects always use index 0
    return;
}

// THEN check gen_old_flag
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
```

### For Location 2 and 3:
Move the is_allocated check before has_gen_old_flag() call:

```rust
// Check is_allocated FIRST
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

**Geohot (Exploit 攻擊觀點):**
If an attacker can control the timing of slot reuse, they might influence the gen_old_flag value. Combined with other vulnerabilities, this could potentially lead to exploit primitives. However, the primary risk is GC correctness failure causing memory leaks or crashes.

---

## 驗證記錄

- [x] Bug exists in current code at heap.rs:2729 (large object path - no is_allocated check at all)
- [x] Bug exists in current code at heap.rs:2754 (small object path - same as bug282)
- [x] Bug exists in current code at heap.rs:3043 (incremental_write_barrier - same as bug282)
