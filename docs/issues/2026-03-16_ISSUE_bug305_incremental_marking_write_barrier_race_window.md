# [Bug]: Incremental Marking Race Window - Write Barrier Not Guaranteed Active Before Mutators Resume

**Status:** Invalid
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Only affects incremental marking path, requires specific timing |
| **Severity (嚴重程度)** | High | Can cause memory leaks (missed OLD→YOUNG references) |
| **Reproducibility (復現難度)** | Medium | Requires timing-dependent test with release builds |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (gc/incremental.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

在 `execute_snapshot` 函數中，設置 `MarkPhase::Marking` 和恢復 mutators言之間存在一個 race window。雖然有 `debug_assert!` 來檢查 write barrier 是否處於 active 狀態，但這個檢查只在 debug build 中有效。

### 預期行為 (Expected Behavior)
Write barrier 應該在恢復 mutators 之前確保處於 active 狀態，以防止遺漏 OLD→YOUNG 引用。

### 實際行為 (Actual Behavior)
在 release build 中，phase 設置為 Marking 後，mutators 可能會在 write barrier 完全激活之前恢復執行。這可能導致在 incremental marking 期間創建的新引用未被正確追蹤。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs:640-645`：

```rust
state.set_phase(MarkPhase::Marking);
debug_assert!(
    write_barrier_needed(),
    "Write barrier must be active before resuming mutators"
);
resume_all_mutators();
```

問題：
1. `set_phase(MarkPhase::Marking)` 在 line 640 調用
2. `debug_assert!` 只在 debug build 中有效 (line 641-644)
3. `resume_all_mutators()` 在 line 645 調用

在 release build 中，沒有任何機制確保 write barrier 在 mutators 恢復之前已經激活。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要在 release build 中運行，且需要精確的時序
// 這個 bug 很難穩定重現，因為它依賴於 CPU 調度和時序

#[test]
fn test_incremental_race_window() {
    use rudo_gc::gc::incremental::{IncrementalConfig, IncrementalMarkState};
    use rudo_gc::test_util;
    
    test_util::reset();
    
    // 配置 incremental marking
    let config = IncrementalConfig {
        enabled: true,
        increment_size: 10,
        max_dirty_pages: 10,
        remembered_buffer_len: 8,
        slice_timeout_ms: 10,
    };
    IncrementalMarkState::global().set_config(config);
    
    // ... 需要精確時序的測試
}
```

Note: 此 bug 需要在 release build 中運行，且需要極端的時序條件才能穩定觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `debug_assert!` 改為在 release build 中也執行的檢查：

```rust
state.set_phase(MarkPhase::Marking);
if !write_barrier_needed() {
    // Fallback to STW if write barrier is not ready
    // Or wait until barrier is active
    state.request_fallback(FallbackReason::WriteBarrierNotReady);
}
resume_all_mutators();
```

或者在 `resume_all_mutators()` 內部添加屏障，確保 write barrier 激活後才恢復 mutators。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 incremental marking 中，SATB 屏障必須在 mutators 恢復後立即生效。任何在 snapshot 之後創建的引用都應該被記錄。如果 barrier 在 mutators 恢復後才激活，這段時間窗口內的引用將被遺漏，導致可能存活的年輕對象被錯誤回收。

**Rustacean (Soundness 觀點):**
這不是傳統的 UB，但可能導致內存泄漏（對象被錯誤回收）。debug_assert! 只在調試時有效，在 release build 中會成為一個 silent bug。

**Geohot (Exploit 觀點):**
雖然這是一個內存泄漏問題，但在極端情況下，如果攻擊者能夠控制 GC 時機，可能會利用這個窗口來實現某些攻擊場景（例如，故意觸發特定的 GC 時序來導致對象過早回收）。

---

## Resolution (2026-03-21)

**Outcome:** Invalid — no separate “activate barrier” step exists beyond publishing phase.

**Rationale:**

1. `set_phase(MarkPhase::Marking)` uses `AtomicUsize::store(..., Ordering::SeqCst)` (`incremental.rs`). Mutators consult `phase()` which loads with `SeqCst`. There is no additional hardware or global flag that must be turned on after phase transition; “barrier active” for incremental SATB is exactly `phase == Marking` (see `is_write_barrier_active()`).

2. `execute_snapshot` runs only from `collect_major_incremental`, which is entered only when `IncrementalMarkState::is_enabled()` is true (`gc.rs`). At snapshot start, `reset_fallback()` clears `fallback_requested`. No code path in `execute_snapshot` sets fallback before `set_phase(Marking)`. So `write_barrier_needed()` (enabled ∧ ¬fallback ∧ Marking) holds at the `debug_assert` in normal operation.

3. Ordering: the GC thread performs the `SeqCst` phase store before `resume_all_mutators()`. Woken mutators’ first `phase()` / `is_incremental_marking_active()` load synchronizes with that store; this is not a release-build-only hole left by `debug_assert!`.

**Note:** Concurrent `set_config` disabling incremental while another thread is inside `execute_snapshot` could make `write_barrier_needed()` false while phase is still `Marking`; that would be a distinct API/contract issue, not the “resume before barrier” window described here.

**Verification:** `cargo test -p rudo-gc test_execute_snapshot --all-features -- --test-threads=1` (passes).
