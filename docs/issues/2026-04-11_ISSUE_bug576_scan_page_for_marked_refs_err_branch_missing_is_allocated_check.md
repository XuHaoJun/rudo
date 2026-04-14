# [Bug]: scan_page_for_marked_refs CAS retry in Err branch without is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires CAS failure to occur simultaneously with slot deallocation by lazy sweep |
| **Severity (嚴重程度)** | High | UB - reading generation from deallocated slot during retry |
| **Reproducibility (重現難度)** | Very Low | Requires Miri or ThreadSanitizer to detect |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_marked_refs` (gc/incremental.rs:910)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

When CAS fails in `try_mark` and the slot has been deallocated by lazy sweep, the loop should check `is_allocated` before retrying.

### 實際行為 (Actual Behavior)

The `Err(())` branch in `scan_page_for_marked_refs` (incremental.rs:910) does nothing and retries the loop without checking if the slot is still allocated:

```rust
Err(()) => {} // CAS failed, retry ← BUG: No is_allocated check!
```

If lazy sweep deallocates the slot between CAS failures, the retry reads from a deallocated slot, causing undefined behavior.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `gc/incremental.rs:910`

```rust
loop {
    match (*header).try_mark(i) {
        Ok(false) => {
            // Already marked by another thread; move to next slot.
            break;
        }
        Ok(true) => {
            // Has is_allocated checks before reading generation
            // ...
        }
        Err(()) => {} // BUG: No is_allocated check!
    }
}
```

**對比 `worker_mark_loop` (marker.rs:943) - bug575 已修復:**

`worker_mark_loop` 已經修復了這個問題模式 (bug575)：

```rust
Err(()) => {
    // Check is_allocated before retry to avoid UB from deallocated slot.
    if !(*header.as_ptr()).is_allocated(idx) {
        break;
    }
}
```

但 `scan_page_for_marked_refs` 仍然有這個錯誤模式。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Requires ThreadSanitizer or Miri to detect the data race between CAS failure and slot deallocation.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check in `Err` branch, matching the pattern in `worker_mark_loop` (bug575 fix):

```rust
Err(()) => {
    // Check is_allocated before retry to avoid UB from deallocated slot.
    if !(*header).is_allocated(i) {
        break;
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The CAS failure could occur after lazy sweep deallocates the slot. The retry should verify the slot is still allocated before continuing.

**Rustacean (Soundness 觀點):**
Reading from a deallocated slot is undefined behavior. The `is_allocated` check must be performed before any retry.

**Geohot (Exploit 觀點):**
While the race window is small, an attacker who could influence GC timing might trigger UB by causing the CAS to fail exactly when lazy sweep deallocates the slot.

---

## 相關 Issue

- bug575: Same bug pattern in worker_mark_loop and worker_mark_loop_with_registry (fixed)
- bug574: Same bug pattern in mark_object_minor, mark_object, mark_and_trace_incremental (fixed)
- bug573: Same bug pattern in mark_and_push_to_worker_queue (fixed)
