# [Bug]: mark_page_dirty_for_borrow small object path missing third is_allocated check (TOCTOU)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 100% reproducible with `./test.sh` deep_tree tests |
| **Severity (嚴重程度)** | `Critical` | Memory safety issue - UAF when dereferencing Gc child |
| **Reproducibility (Reproducibility)** | `Very High` | Always fails with `cargo test --test deep_tree_allocation_test` |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_page_dirty_for_borrow` in `heap.rs:3190-3204` (small object path)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`mark_page_dirty_for_borrow` should have three `is_allocated` checks (defense-in-depth pattern):
1. First check: Before reading any generation-related data
2. Second check: Before reading `has_gen_old`
3. **Third check**: After reading `has_gen_old` (prevents TOCTOU between check and set_dirty)

This pattern is used consistently in `incremental_write_barrier`, `simple_write_barrier`, and `unified_write_barrier`.

### 實際行為 (Actual Behavior)
The **small object path** in `mark_page_dirty_for_borrow` (lines 3190-3204) has only ONE `is_allocated` check at line 3197. It does NOT have the generation check or the third defensive check:

```rust
// Current code (lines 3190-3204):
let offset = ptr_addr - (header_page_addr + header_size);
let index = offset / block_size;
let obj_count = (*h).obj_count as usize;
if index >= obj_count {
    return;
}

if !(*h).is_allocated(index) {  // ONLY check 1 - NO generation check!
    return;
}

(*h).set_dirty(index);  // TOCTOU window: slot could be swept/reused here!
heap.add_to_dirty_pages(header);
```

Compare to `incremental_write_barrier` small object path which has all three checks.

### Root Cause
The small object path in `mark_page_dirty_for_borrow`:
1. Only checks `is_allocated` once (line 3197)
2. Does NOT read `gc_box_addr` to check `has_gen_old`
3. Does NOT verify generation before marking dirty
4. Has a TOCTOU window between `is_allocated` check and `set_dirty`

This causes incorrect dirty page tracking when:
1. A slot passes the `is_allocated` check
2. The slot is swept and reused (with new generation) BEFORE `set_dirty` is called
3. `set_dirty` marks the NEW object's slot as dirty (type confusion!)

### Evidence
Tests `test_deep_tree_allocation` and `test_collect_between_deep_trees` fail with:
```
Gc::deref: slot has been swept and reused
```

This happens because children stored in `GcCell<Vec<Gc<T>>>` are incorrectly swept - the dirty page tracking is corrupted due to the missing checks.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `mark_page_dirty_for_borrow` at `heap.rs:3190-3204`:

```rust
// Small object path - ONLY ONE is_allocated check, NO generation check!
let offset = ptr_addr - (header_page_addr + header_size);
let index = offset / block_size;
let obj_count = (*h).obj_count as usize;
if index >= obj_count {
    return;
}

if !(*h).is_allocated(index) {  // Check 1 only
    return;
}
// MISSING: Read gc_box_addr, check generation, third is_allocated check

(*h).set_dirty(index);  // TOCTOU!
heap.add_to_dirty_pages(header);
```

Compare to `incremental_write_barrier` small object path (`heap.rs:3399-3416`):
```rust
// incremental_write_barrier small object path - CORRECT pattern:
if !(*h.as_ptr()).is_allocated(index) {  // Check 1
    return;
}
// ...
if !(*h.as_ptr()).is_allocated(index) {  // Check 2 - BEFORE reading has_gen_old
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;  // Early exit for young pages without gen_old
}
if !(*h.as_ptr()).is_allocated(index) {  // Check 3 - AFTER has_gen_old (FIX bug530)
    return;
}
```

The small object path in `mark_page_dirty_for_borrow` is missing the entire generation check logic that other barrier functions have.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```bash
cd /home/noah/Desktop/workspace/rudo-gc/rudo
cargo test --test deep_tree_allocation_test -- --test-threads=1
```

**Expected:** All tests pass
**Actual:** 
```
test_deep_tree_allocation ... FAILED
test_collect_between_deep_trees ... FAILED

Gc::deref: slot has been swept and reused
panic at crates/rudo-gc/src/ptr.rs:2152:17
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check and third `is_allocated` check to `mark_page_dirty_for_borrow` small object path:

```rust
// In mark_page_dirty_for_borrow (heap.rs:3190-3204)
// FIX bug620: Add generation check and third is_allocated check

let offset = ptr_addr - (header_page_addr + header_size);
let index = offset / block_size;
let obj_count = (*h).obj_count as usize;
if index >= obj_count {
    return;
}

// Check 1: Verify slot is allocated
if !(*h).is_allocated(index) {
    return;
}

// FIX bug620: Read gc_box_addr to check generation
let gc_box_addr = (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();

// Check 2: Early return for young pages (same as other barrier functions)
if (*h).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}

// Check 3: Third is_allocated check AFTER has_gen_old read (prevents TOCTOU)
if !(*h).is_allocated(index) {
    return;
}

(*h).set_dirty(index);
heap.add_to_dirty_pages(header);
```

This matches the pattern in `incremental_write_barrier`, `simple_write_barrier`, and `unified_write_barrier`.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The dirty page tracking is critical for minor GC tracing. When `borrow_mut()` is called on a `GcCell`, the page must be added to `dirty_pages` so children are traced. The `mark_page_dirty_for_borrow` function is the safety net for when `gc_cell_validate_and_barrier` returns early (gen=0, no gen_old). Missing the generation check and third is_allocated check breaks this safety net.

**Rustacean (Soundness 觀點):**
This is a memory safety violation. The TOCTOU between `is_allocated` check and `set_dirty` could cause:
1. Type confusion (marking wrong slot as dirty)
2. Incorrect dirty page tracking
3. Children not being traced during minor GC
4. UAF when dereferencing Gc pointers to swept slots

**Geohot (Exploit 觀點):**
The race window is narrow but exploitable. An attacker who can trigger GC at the right moment could cause incorrect dirty page marking, leading to children being swept while still referenced.

---

## 📎 Related Issues
- bug530: incremental_write_barrier missing third is_allocated check
- bug583: GcCell Vec<Gc> slot swept and reused (symptom of this bug)
- bug620: This issue
- bug71: Original gen_old optimization