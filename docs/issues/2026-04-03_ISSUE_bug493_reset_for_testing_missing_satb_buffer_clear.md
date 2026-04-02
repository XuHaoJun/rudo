# [Bug]: reset_for_testing does not clear cross-thread SATB buffers, causing test pollution

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Every test that uses cross-thread SATB leaves entries in buffers |
| **Severity (嚴重程度)** | Low | Only affects tests, not production code |
| **Reproducibility (復現難度)** | Very Low | Always reproducible when tests run in order |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `reset_for_testing` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`reset_for_testing()` does not clear the cross-thread SATB buffers (`CROSS_THREAD_SATB_BUFFER`, `CROSS_THREAD_SATB_OVERFLOW_BUFFER`, `CROSS_THREAD_SATB_EMERGENCY_BUFFER`). When tests use `GcThreadSafeCell::borrow_mut()` which pushes to these buffers, the entries remain after the test. Subsequent tests that expect empty buffers may fail or behave incorrectly.

### 預期行為 (Expected Behavior)
`reset_for_testing()` should clear all global GC state so tests start with a clean slate.

### 實際行為 (Actual Behavior)
Cross-thread SATB buffers retain entries from previous tests, causing:
- `test_cross_thread_satb_dual_overflow_no_entry_loss` to fail when run after `test_cross_thread_borrow_mut_gc_correctness`
- The first push goes to overflow/emergency instead of main buffer because buffer already has entries

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `heap.rs`, `reset_for_testing()` (line 4044) clears:
- GC_REQUESTED flag
- Thread registry
- Segment manager
- Local heap

But it does NOT clear:
- `CROSS_THREAD_SATB_BUFFER`
- `CROSS_THREAD_SATB_OVERFLOW_BUFFER`
- `CROSS_THREAD_SATB_EMERGENCY_BUFFER`

The `reset()` function in `test_util` (lib.rs:250) calls `reset_for_testing()` and also clears `CROSS_THREAD_SATB_SIZE_OVERRIDE`, but not the buffers themselves.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Run the following tests in order:
```bash
cargo test --test gc_thread_safe_cell --features test-util -- --test-threads=1 test_cross_thread_borrow_mut_gc_correctness test_cross_thread_satb_dual_overflow_no_entry_loss
```

Result: `test_cross_thread_satb_dual_overflow_no_entry_loss` FAILS

But when run alone:
```bash
cargo test --test gc_thread_safe_cell --features test-util -- --test-threads=1 test_cross_thread_satb_dual_overflow_no_entry_loss
```

Result: Test PASSES

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add buffer clearing to `reset_for_testing()` in `heap.rs`:

```rust
pub unsafe fn reset_for_testing() {
    // ... existing code ...

    // Clear cross-thread SATB buffers
    CROSS_THREAD_SATB_BUFFER.lock().clear();
    CROSS_THREAD_SATB_OVERFLOW_BUFFER.lock().clear();
    CROSS_THREAD_SATB_EMERGENCY_BUFFER.lock().clear();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Test isolation is crucial for reliable testing. The cross-thread SATB buffers are global state that should be reset between tests to ensure each test starts with a clean state.

**Rustacean (Soundness 觀點):**
This is a test infrastructure bug, not a production code bug. The buffers are properly cleared during normal GC operation (in execute_final_mark), but `reset_for_testing()` bypasses this.

**Geohot (Exploit 觀點):**
Not exploitable - this only affects test infrastructure.