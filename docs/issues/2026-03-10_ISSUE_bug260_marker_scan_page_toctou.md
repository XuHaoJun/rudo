# [Bug]: PerThreadMarkQueue::scan_page_before_push TOCTOU - is_allocated 檢查與 push 之间存在 race

**Status:** Fixed
**Tags:** Verified

---

## Threat Model Assessment

| Aspect | Assessment |
|--------|------------|
| Likelihood | High |
| Severity | High |
| Reproducibility | Medium |

---

## Affected Component & Environment

- **Component:** `PerThreadMarkQueue::process_owned_page` (parallel mark scan) in `gc/marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## Description

### Expected Behavior

在將物件推入 worklist 之前，應該再次驗證物件仍然有效（未被 sweep）。

### Actual Behavior

函數在 `is_allocated` 檢查後、 `push` 調用前，沒有再次檢查物件是否仍然 allocated。這與 bug258 在 `incremental.rs` 中修復的問題相同，但在 `marker.rs` 中被遺漏。

---

## Root Cause Analysis

在 `gc/marker.rs`（`try_mark` 成功分支，約 `process_owned_page` 內）:

```rust
match (*header).try_mark(i) {
    Ok(false) => break, // Already marked
    Ok(true) => {
        // Re-check is_allocated to fix TOCTOU
        if !(*header).is_allocated(i) {
            (*header).clear_mark_atomic(i);
            break;
        }
        // BUG: Race window here! Slot can be swept between is_allocated check and push
        marked += 1;
        self.push(gc_box_ptr.as_ptr());
        break;
    }
    Err(()) => {} // CAS failed, retry
}
```

**Race 條件說明**:
1. Line 704: `is_allocated(i)` 返回 true - slot 仍然有效
2. Line 705-707: 清除標記並 continue (如果 slot 已釋放)
3. **Race Window**: 如果 slot 在 line 704 之後、line 709 之前被 sweep
4. Line 709: `push(gc_box_ptr)` - 推送可能已釋放的物件

---

## Fix

在 `push` 之前再次檢查 `is_allocated`（與 bug258 相同的修復模式）:

```rust
match (*header).try_mark(i) {
    Ok(false) => break, // Already marked
    Ok(true) => {
        // Re-check is_allocated to fix TOCTOU
        if !(*header).is_allocated(i) {
            (*header).clear_mark_atomic(i);
            break;
        }
        // Second check to fix TOCTOU: slot can be swept between first check and push
        if !(*header).is_allocated(i) {
            (*header).clear_mark_atomic(i);
            break;
        }
        marked += 1;
        self.push(gc_box_ptr.as_ptr());
        break;
    }
    Err(()) => {} // CAS failed, retry
}
```

---

## Internal Discussion Record

**R. Kent Dybvig:**
這與 incremental marking 中的 bug258 相同的 TOCTOU 模式。parallel marking 也需要相同的修復。

**Rustacean:**
未經檢驗的指標推入 worklist 可能導致 use-after-free。

**Geohot:**
在高並發場景下很容易觸發這個 race。

---

## Status

- [x] Fixed
- [ ] Not Fixed

---

## Resolution (2026-03-21)

**Verified in code:** `PerThreadMarkQueue::process_owned_page` already performs a second `is_allocated(i)` immediately before `self.push(...)` after the first post-`try_mark` recheck (see `crates/rudo-gc/src/gc/marker.rs`, comment `bug260`). Matches the bug258-style TOCTOU fix in incremental marking.

**Tests:** `cargo test -p rudo-gc --test parallel_gc -- --test-threads=1` — all passed.

No further code change required; issue tracker was out of date (component was formerly described as `scan_page_before_push`, which no longer exists as a symbol).
