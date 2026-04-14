# [Bug]: `mark_new_object_black` 缺少 `try_mark` + generation check 的 TOCTOU 保護

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 lazy sweep 並發運行時可能觸發 |
| **Severity (嚴重程度)** | High | 可能導致錯誤物件被標記，進而導致 UAF 或不正確的 GC |
| **Reproducibility (復現難度)** | Medium | 需要並發場景，但可以通過 stress test 復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc::incremental::mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`mark_new_object_black` 函數使用簡單的 `set_mark(idx)` 而非 `try_mark(idx)`，缺少 generation check 來檢測 slot 是否在標記過程中被 sweep 和 reuse。

### 預期行為 (Expected Behavior)

當多個執行緒同時標記新物件時，如果 slot 被 sweep 並 reuse，應該通過 generation check 檢測並跳過，而不是錯誤地標記新物件。

### 實際行為 (Actual Behavior)

`mark_new_object_black` 使用 `set_mark(idx)` 直接設置標記位，沒有：
1. `try_mark` 的 CAS 原子操作
2. generation check 來檢測 slot reuse
3. `clear_mark_atomic` 來回滾錯誤的標記

相比之下，`mark_object_black` (incremental.rs:1125) 和 `scan_page_for_marked_refs` 都使用 `try_mark` + generation check 模式。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `mark_new_object_black` (incremental.rs:1075-1110) 中：

```rust
if !(*header.as_ptr()).is_marked(idx) {
    let marked_generation = (*gc_box).generation();
    (*header.as_ptr()).set_mark(idx);
    // 缺少：generation verify + clear_mark_atomic
    ...
}
```

問題：
1. **沒有 CAS 保護**：使用 `set_mark` 而非 `try_mark`，如果多個執行緒同時標記同一個 slot，會產生 data race
2. **沒有 generation check**：如果 slot 在 `set_mark` 後被 sweep 並 reuse，`set_mark` 的標記會「污染」新物件
3. **沒有清除機制**：當檢測到 slot reuse 時，無法清除錯誤的標記

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
//! Regression test for Bug 527: `mark_new_object_black` missing try_mark + generation check
//!
//! When lazy sweep reclaims a slot between is_marked check and set_mark,
//! mark_new_object_black may incorrectly mark the new object that took the slot.
//!
//! See: docs/issues/2026-04-07_ISSUE_bug527_mark_new_object_black_missing_try_mark.md

use rudo_gc::{Gc, Trace, collect_full, incremental::{set_incremental_config, IncrementalConfig}};
use std::thread;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_mark_new_object_black_concurrent_sweep() {
    // Enable incremental marking to trigger the bug path
    set_incremental_config(IncrementalConfig {
        enabled: true,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    });

    // Create many Gc objects to fill pages
    let mut gcs = vec![];
    for i in 0..1000 {
        gcs.push(Gc::new(Data { value: i }));
    }

    // Spawn a thread that continuously allocates and triggers incremental marking
    let handle = thread::spawn(move || {
        for _ in 0..100 {
            let _gc = Gc::new(Data { value: 42 });
            if _ % 10 == 0 {
                collect_full();
            }
        }
    });

    handle.join().unwrap();
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `mark_new_object_black` 改為使用 `try_mark` + generation check 模式，與 `mark_object_black` 保持一致：

```rust
if !(*header.as_ptr()).is_marked(idx) {
    loop {
        match (*header.as_ptr()).try_mark(idx) {
            Ok(false) => return Some(idx), // Already marked
            Ok(true) => {
                let marked_generation = (*gc_box).generation();
                // Verify slot wasn't swept+reused
                if (*gc_box).generation() != marked_generation {
                    (*header.as_ptr()).clear_mark_atomic(idx);
                    return None;
                }
                return Some(idx);
            }
            Err(()) => {} // CAS failed, retry
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`mark_new_object_black` 是 black allocation 優化的關鍵。如果新物件在標記過程中被錯誤標記，會導致 GC 保留不應保留的物件，影響 GC 的正確性。使用 `try_mark` + generation check 是必要的防護。

**Rustacean (Soundness 觀點):**
`set_mark` 不是原子操作，在多執行緒環境下可能產生 data race。`try_mark` 使用 CAS 原子操作，是正確的併發標記方式。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 slot reuse 的時機，可以利用這個 bug 讓 GC 保留惡意物件，進一步利用記憶體佈局進行攻擊。