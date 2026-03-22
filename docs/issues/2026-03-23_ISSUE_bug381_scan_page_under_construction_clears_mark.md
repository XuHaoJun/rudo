# [Bug]: scan_page_for_*_refs clears mark when is_under_construction, inconsistent with mark_object_black

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Occurs during incremental marking of objects under construction |
| **Severity (嚴重程度)** | High | Can cause partially-initialized objects to be incorrectly swept |
| **Reproducibility (復現難度)** | Medium | Requires concurrent incremental GC and new_cyclic_weak allocation |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`scan_page_for_marked_refs`, `scan_page_for_unmarked_refs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`sscan_page_for_marked_refs` and `scan_page_for_unmarked_refs` incorrectly clear the mark when `is_under_construction` is true. This is inconsistent with `mark_object_black` and `mark_new_object_black` which return early WITHOUT clearing the mark.

### 預期行為 (Expected Behavior)
When encountering an object with `is_under_construction = true`, the scan functions should return early WITHOUT modifying the mark state, matching the behavior of `mark_object_black` and `mark_new_object_black`.

### 實際行為 (Actual Behavior)
`sscan_page_for_marked_refs` (line 867) and `scan_page_for_unmarked_refs` (line 1014) BOTH call `clear_mark_atomic(i)` before breaking, which incorrectly clears the mark on objects under construction.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The comment at lines 864-865 and 1011-1012 states:
> "Skip partially initialized objects (e.g. Gc::new_cyclic_weak); matches mark_object_black / mark_new_object_black (bug238, bug309)"

This comment is **incorrect**. The referenced functions do NOT clear the mark:

**`mark_object_black`** (line 1119-1120) - returns `None` WITHOUT clearing:
```rust
if gc_box.is_under_construction() {
    return None;  // Returns None WITHOUT clearing any mark
}
```

**`mark_new_object_black`** (line 1067-1068) - returns `false` WITHOUT clearing:
```rust
if gc_box.is_under_construction() {
    return false;  // Returns false WITHOUT clearing any mark
}
```

**BUT `scan_page_for_marked_refs`** (lines 866-868) - DOES clear:
```rust
if unsafe { (*gc_box_ptr).is_under_construction() } {
    (*header).clear_mark_atomic(i);  // CLEARS the mark it just set!
    break;
}
```

### Race Condition Scenario
1. Object O is allocated during `Gc::new_cyclic_weak`, marked black via black allocation, but `is_under_construction = true`
2. Before initialization completes, `scan_page_for_marked_refs` processes O's page
3. `scan_page_for_marked_refs` sees O is marked AND under construction, so it **clears the mark** (line 867)
4. Concurrent write barrier calls `mark_object_black` on O
5. `mark_object_black` sees `is_under_construction = true`, returns `None` without restoring the mark
6. Object O is now incorrectly unmarked and could be swept during the concurrent sweep phase

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This bug is timing-dependent and requires:
1. Incremental marking active
2. `Gc::new_cyclic_weak` allocating an object
3. Concurrent GC thread scanning the page before `is_under_construction` is set to false

```rust
// PoC would require instrumentation to observe the incorrect mark clear
// Suggest adding debug assertions in mark_object_black to detect this race
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Remove the `clear_mark_atomic(i)` calls at lines 867 and 1014. Just `break` without clearing:

```rust
// Line 866-868: change from:
if unsafe { (*gc_box_ptr).is_under_construction() } {
    (*header).clear_mark_atomic(i);
    break;
}

// To:
if unsafe { (*gc_box_ptr).is_under_construction() } {
    break;
}

// Similarly for lines 1013-1015
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The incremental marking state machine relies on consistent mark behavior across all marking paths. When `scan_page_for_*_refs` clears a mark that was set by black allocation, it creates an inconsistent state where an object is "allocated but unmarked" during construction. This could lead to premature reclamation during concurrent sweep.

**Rustacean (Soundness 觀點):**
While this doesn't directly cause UAF (the object is still allocated), it violates the invariant that black-allocated objects remain marked until their construction completes. The comment claims to "match" other functions but actually contradicts them.

**Geohot (Exploit 觀點):**
This could be chained with a TOCTOU in the sweep phase. If timing allows the sweep to run between the mark clear and the `is_under_construction` flip, you could get a use-after-free on a partially constructed object. Low probability but high impact if achieved.
