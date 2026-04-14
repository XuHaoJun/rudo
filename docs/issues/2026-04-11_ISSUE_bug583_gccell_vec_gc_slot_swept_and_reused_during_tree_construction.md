# [Bug]: GcCell Vec<Gc> slot swept - generational barrier not adding young pages to dirty_pages

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 100% reproducible with `./test.sh` |
| **Severity (嚴重程度)** | `Critical` | Memory safety issue - UAF when dereferencing Gc |
| **Reproducibility (Reproducibility)** | `Very High` | Always fails with `./test.sh` |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut`, `gc_cell_validate_and_barrier` (heap.rs:2979), minor GC sweep
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When building a deep tree structure with parent/child relationships using `GcCell<Vec<Gc<T>>>`, all nodes should remain accessible. When `borrow_mut()` is called on a GcCell in a young page, the generational barrier should add the page to dirty_pages so children are traced during minor GC.

### 實際行為 (Actual Behavior)
Two tests in `deep_tree_allocation_test.rs` fail:
- `test_deep_tree_allocation`
- `test_collect_between_deep_trees`

Both fail with:
```
Gc::deref: slot has been swept and reused
panicked at crates/rudo-gc/src/ptr.rs:2132:17
```

### Root Cause Identified

In `gc_cell_validate_and_barrier` (heap.rs:3019-3023):
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;  // BUG: Returns early for young pages without adding to dirty_pages!
}
```

When a GcCell is in a young page (generation=0, gen_old=false):
1. `borrow_mut()` is called on the GcCell
2. The generational barrier fires but returns early
3. The page is NOT added to dirty_pages
4. During minor GC, the page is NOT scanned
5. Children stored in the GcCell are NOT traced
6. Children are incorrectly swept

This is a correctness bug - the gen_old optimization for avoiding unnecessary dirty page scans is breaking the invariant that test_roots and their transitive closure must be traced during minor GC.

### Evidence

The issue manifests when:
1. Tree2's root (0x600000000d80) is registered as test_root
2. Child2 is allocated at 0x600000000f00
3. `root.add_child(child2)` triggers `borrow_mut()` on root's GcCell
4. Root's page is young (gen=0, gen_old=false) - barrier returns early
5. Root's page not in dirty_pages - minor GC doesn't scan it
6. Child2 is traced through root but NOT through dirty page scan (root not dirty)
7. Child2's slot is incorrectly swept (not traced as child of root)
8. Dereferencing child2's Gc finds swept slot -> panic

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `gc_cell_validate_and_barrier` at heap.rs:3019-3023:

```rust
// Skip barrier only if page is young AND object has no gen_old_flag (bug71).
// Cache flag to avoid TOCTOU between check and barrier (bug114).
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;  // BUG: Early return prevents dirty page tracking!
}
```

The comment says "Skip barrier only if page is young" - this is the gen_old optimization. But it incorrectly skips adding the page to dirty_pages even when `borrow_mut()` is actively modifying the GcCell.

The issue is:
1. gen_old is set AFTER promotion to track OLD→YOUNG references
2. But BEFORE gen_old is set, the page won't be in dirty_pages
3. So children in GcCells on young pages won't be traced during minor GC
4. Even if the parent (root) is a test_root

The fix should ensure that when `borrow_mut()` is called, the page is added to dirty_pages so children are traced. The gen_old optimization should only affect whether we RECORD the OLD→YOUNG reference for later scanning, not whether we SCAN the page at all.

### Code Flow Analysis

1. `GcCell::borrow_mut()` calls `gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active)`
2. `gc_cell_validate_and_barrier` checks if ptr is in GC heap
3. For young page (gen=0, gen_old=false): returns early at line 3023
4. `unified_write_barrier` is NOT called
5. Page NOT added to dirty_pages
6. Minor GC marks root (via test_roots) but doesn't scan root's page
7. Children in GcCell NOT traced -> incorrectly swept

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
```

Both fail with "Gc::deref: slot has been swept and reused"

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

The fix should ensure that `borrow_mut()` always adds the page to dirty_pages, regardless of gen_old flag. The gen_old flag should only affect whether we RECORD the OLD→YOUNG reference for the remembered set, not whether we scan the page.

**Option A (Recommended):** Remove the early return for gen=0 in `gc_cell_validate_and_barrier`, but keep the gen_old check for the actual barrier recording:

```rust
// In gc_cell_validate_and_barrier (heap.rs:3019-3023)
// REMOVE the early return, but the barrier recording below already handles gen_old correctly
// Actually the issue is the entire function returns early

// Instead, we should always call unified_write_barrier but let it handle the gen_old check
```

**Option B:** Always add to dirty_pages in `borrow_mut()` regardless of barrier result:

In `GcCell::borrow_mut()` (cell.rs:194-197):
```rust
// FIX bug583: Always trigger barrier and add to dirty_pages.
// The gen_old optimization should only affect remembered set recording,
// not whether children are traced during minor GC.
if generational_active || incremental_active {
    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active);
}
// ADD: Always mark page dirty if generational barrier might be needed
if generational_active {
    // Ensure page is in dirty_pages for minor GC tracing
    // ... 
}
```

**Option C:** Ensure gen_old is set before any GcCell modification that could trigger minor GC:

This would require modifying when gen_old is set, which is complex.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The gen_old optimization was designed to avoid scanning pages that definitely don't have OLD→YOUNG references. But when `borrow_mut()` is called, we're ABOUT TO create a potential OLD→YOUNG reference (if the parent is old and child is young). The barrier should fire to add the page to dirty_pages so the child is traced. The gen_old check should only skip the REMEMBERED SET recording, not the dirty page tracking.

**Rustacean (Soundness 觀點):**
This is a memory safety violation - UAF when dereferencing a Gc pointer. The slot was swept while we still held a reference to it. The fix is straightforward - ensure pages are scanned during minor GC when they're actively being modified.

**Geohot (Exploit 觀點):**
The narrow window between when `borrow_mut()` returns and when children are accessed could be exploited if an attacker could trigger GC at the right moment. But since this is a local memory issue (not cross-thread), exploitability is limited.

---

## 📎 Related Issues
- bug71: Original gen_old optimization
- bug114: TOCTOU fix for barrier state caching
- bug506: GcCell unconditional marking fix
- bug484: Related GcCell write barrier issue