# [Bug]: `rendezvous_ack_counter` 機制未完成 - 增加後從未被正確使用

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 目前使用 `active == 1` 作為替代方案，功能正常 |
| **Severity (嚴重程度)** | Low | 不會導致記憶體錯誤，但為未完成的基礎設施 |
| **Reproducibility (復現難度)** | N/A | 非實際 bug，而是未完成的程式碼 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `IncrementalMarkState::rendezvous_ack_counter`, `stop_all_mutators_for_snapshot()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`rendezvous_ack_counter` 應該追蹤已進入 rendezvous 的執行緒數量。當收集器需要停止所有 mutator 執行緒時，應該等待所有 mutator 都進入 rendezvous 並遞增計數器。

### 實際行為 (Actual Behavior)

`rendezvous_ack_counter` 的實現從未完成：

1. **僅收集器增加計數器**：`stop_all_mutators_for_snapshot()` 中，收集器呼叫 `state.increment_rendezvous_ack()`，但這只會將計數器設為 1

2. **Mutator 從不遞增計數器**：在 `enter_rendezvous()` 中，執行緒進入 rendezvous 時只遞減 `active_count`，從不遞增 `rendezvous_ack_counter`

3. **計數器從未被檢查**：`stop_all_mutators_for_snapshot()` 只檢查 `active == 1`，從不檢查 `rendezvous_ack_count()`

### 程式碼位置

`incremental.rs` 第 514-550 行：
```rust
fn stop_all_mutators_for_snapshot() {
    let state = IncrementalMarkState::global();
    let registry = crate::heap::thread_registry().lock().unwrap();

    crate::heap::GC_REQUESTED.store(true, std::sync::atomic::Ordering::Release);

    for tcb in &registry.threads {
        tcb.gc_requested
            .store(true, std::sync::atomic::Ordering::Release);
    }

    state.increment_rendezvous_ack();  // <-- 只增加一次，變成 1

    drop(registry);

    loop {
        let registry = crate::heap::thread_registry().lock().unwrap();
        let active = registry
            .active_count
            .load(std::sync::atomic::Ordering::Acquire);
        // ...
        // The rendezvous_ack_counter was never fully wired: mutators never
        // increment it, so ack_count >= thread_count would never hold.  <-- 註解承認問題
        if active == 1 {
            break;
        }
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`rendezvous_ack_counter` 機制是為了提供一個獨立的共識追蹤而設計的，但從未被完成：

1. **設計意圖**：每個 mutator 進入 rendezvous 時應遞增計數器，收集器等待 `ack_count >= thread_count`

2. **實際實現**：
   - `increment_rendezvous_ack()` 和 `reset_rendezvous_ack()` 都有實現
   - `rendezvous_ack_count()` 也有實現，但從未被調用
   - 收集器只增加一次（變成 1），從不等待這個值達到特定閾值

3. **替代方案**：當前使用 `active == 1` 作為信號，表示所有 mutator 都已進入 rendezvous 並遞減了 `active_count`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

這個問題不會導致實際錯誤，因為替代機制 `active == 1` 運作正常。以下是展示計數器未被使用的程式碼分析：

```rust
// 在 stop_all_mutators_for_snapshot 中：
state.increment_rendezvous_ack();  // 計數器變成 1
loop {
    let active = registry.active_count.load(Ordering::Acquire);
    if active == 1 {  // 只檢查 active_count，不檢查 rendezvous_ack_counter
        break;
    }
}

// 永遠不會執行到：
// let ack_count = state.rendezvous_ack_count();
// if ack_count >= thread_count { ... }
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩個選項：

**選項 1：完成機制（如果需要）**
```rust
// 在 enter_rendezvous() 中添加：
state.increment_rendezvous_ack();

// 在 stop_all_mutators_for_snapshot() 中修改：
loop {
    // ...
    let ack_count = state.rendezvous_ack_count();
    let thread_count = registry.threads.len();
    if ack_count >= thread_count {
        break;
    }
}
```

**選項 2：移除未使用的程式碼（如果不需要）**
```rust
// 刪除 rendezvous_ack_counter 相關的所有程式碼：
// - rendezvous_ack_counter 欄位
// - increment_rendezvous_ack()
// - rendezvous_ack_count()
// - reset_rendezvous_ack()
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`rendezvous_ack_counter` 機制似乎是借鑒自其他 GC 實現的設計，但從未被完成。當前的 `active == 1` 檢查實際上提供了必要的同步保證。這個未完成的機制不會造成問題，但可能會造成長期維護的困擾。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，因為沒有違反記憶體安全不變量。`active == 1` 的實現是正確的。這個問題更像是不完整的功能代碼。

**Geohot (Exploit 攻擊觀點):**
不可利用。這個未完成的機制不會創建任何安全漏洞。

---

## 備註

- 這是一個 **代碼品質/完整性問題**，不是 **memory safety bug**
- 現有的 `active_count` 機制運作正常
- 未來如果需要更複雜的 rendezvous 協議，可能需要完成這個機制

---

## Status History

- 2026-03-23: Bug reported
