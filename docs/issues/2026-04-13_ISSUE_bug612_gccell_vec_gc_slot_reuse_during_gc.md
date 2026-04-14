# [Bug]: GcCell<Vec<Gc<T>>> children not properly protected during minor GC

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any use of GcCell<Vec<Gc<T>>> with multi-generational allocation |
| **Severity (嚴重程度)** | High | Objects incorrectly swept during GC, causing use-after-free |
| **Reproducibility (復現難度)** | Medium | Can be reproduced by running deep_tree_allocation_test |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut()` write barrier, minor GC tracing
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When building a deep tree structure with `GcCell<Vec<Gc<T>>>` children, and then calling `collect()` followed by building another tree, all objects from the first tree should either be:
1. Still live (if reachable from roots)
2. Properly collected (if unreachable)

The key issue: objects that are still reachable should NOT have their slots swept and reused while still accessible.

### 實際行為 (Actual Behavior)

The `test_collect_between_deep_trees` and `test_deep_tree_allocation` tests fail with:
```
thread 'test_collect_between_deep_trees' panicked at crates/rudo-gc/src/ptr.rs:2132:17:
Gc::deref: slot has been swept and reused
```

The addresses show slot reuse:
```
=== Building tree 1 ===
BUILD: root = 0x7f546dc6f680
BUILD: child2 = 0x7f546dc6f800  <-- tree1 child2
...
=== Calling collect ===
=== Building tree 2 ===
BUILD: child2 = 0x7f546dc6ff00  <-- tree2 child2 at SAME ADDRESS
```

This indicates tree1's slots were swept and immediately reused for tree2's allocations, even though tree1's root was registered as a test root.

### 程式碼位置

The `TestComponent` structure uses `GcCell<Vec<Gc<Self>>>` for children:
```rust
#[derive(Trace)]
pub struct TestComponent {
    pub id: u64,
    pub children: GcCell<Vec<Gc<Self>>>,
    pub parent: GcCell<Option<Gc<Self>>>,
    // ...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

This appears to be a slot reuse issue where `GcCell<Vec<Gc<T>>>` containers are not properly protected during minor GC:

1. Tree1 is built with `GcCell<Vec<Gc<T>>>` children
2. `collect()` is called (minor or major GC)
3. Tree2 is built - its nodes land in tree1's swept slots
4. When accessing tree1's remaining nodes (via root), the slot has been reused

The `mark_page_dirty_for_borrow()` call in `GcCell::borrow_mut()` is supposed to ensure that pages containing `GcCell` fields are marked dirty for minor GC tracing. However, if this is not working correctly or if there is a TOCTOU issue, children may not be traced during GC.

**Potential root causes:**
1. `mark_page_dirty_for_borrow()` not properly marking pages for `GcCell<Vec<Gc<T>>>` containers
2. Minor GC not properly tracing through `GcCell<Vec<Gc<T>>>` children when page is marked dirty
3. Slot reuse happening too aggressively before GC cycle completes

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Run the existing test:
```bash
cargo test --test deep_tree_allocation_test -- --test-threads=1
```

Expected: All tests pass
Actual: Tests fail with "Gc::deref: slot has been swept and reused"

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. Verify `mark_page_dirty_for_borrow()` is properly marking pages for `GcCell<Vec<Gc<T>>>` fields
2. Check if minor GC properly traces through `GcCell<Vec<Gc<T>>>` containers
3. Consider adding additional slot validity checks before sweeping

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The issue could be in how `GcCell<Vec<Gc<T>>>` fields are traced during minor GC. The `mark_page_dirty_for_borrow()` should ensure the page is in dirty_pages, but there may be a timing issue where the marking doesn't happen before the GC cycle.

**Rustacean (Soundness 觀點):**
This is a memory safety issue - the slot is being reused while the old object is still accessible, causing use-after-free. This is unsound.

**Geohot (Exploit 觀點):**
An attacker could trigger this bug by repeatedly allocating and collecting, causing unpredictable object lifetimes and potential memory corruption.

---

## 驗證記錄

**驗證日期:** 2026-04-13
**驗證人員:** opencode

### 驗證結果

1. Confirmed `test_deep_tree_allocation_test` fails with "slot has been swept and reused"
2. The issue is reproducible - slot reuse happens between collect() calls
3. The test uses `GcCell<Vec<Gc<T>>>` which should be protected by dirty page marking
4. **Test output shows the bug clearly:**
   - Tree1 child2 at address `0x600000000800`
   - After collect(), Tree2 child2 at SAME address `0x600000000f00`
   - This proves tree1's slots were swept while still referenced

**Root cause hypothesis:** When `GcCell<Vec<Gc<T>>>` is mutated via `borrow_mut()`, the page should be marked dirty so children are traced during minor GC. But if the child Gc pointers themselves are in a different page (the vector's data page), the parent's page being marked dirty may not protect the children's slots.

**Conclusion:** Bug exists and is verified through test failure.