# [Bug]: IncrementalMarkState::transition_to has TOCTOU Race Condition

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒並發呼叫 transition_to，且時序精確配合 |
| **Severity (嚴重程度)** | High | 可能導致 phase 狀態不一致，影響 GC 正確性 |
| **Reproducibility (復現難度)** | High | 需精確控制執行緒時序才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `IncrementalMarkState::transition_to` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`IncrementalMarkState::transition_to` 函數存在 TOCTOU (Time-Of-Check-Time-Of-Use) 競爭條件。

### 預期行為
- phase 轉換應該是原子性的，確保狀態機的有效轉換
- 只允許有效的 phase 轉換（如 Idle → Snapshot, Marking → FinalMark）

### 實際行為
- 檢查 phase 有效性與設定新 phase 是分離的兩個操作
- 兩個執行緒可能同時通過有效性檢查，導致其中一個設定無效的 phase 狀態

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs:304-310`:

```rust
pub fn transition_to(&self, new_phase: MarkPhase) -> bool {
    let current = self.phase();        // Step 1: 讀取當前 phase
    if !self.is_valid_transition(current, new_phase) {  // Step 2: 檢查轉換有效性
        return false;
    }
    self.set_phase(new_phase);         // Step 3: 設定新 phase
    true
}
```

問題：
1. **Step 1** 讀取 `current` phase（使用 `Ordering::SeqCst`）
2. **Step 2** 檢查轉換是否有效
3. **Step 3** 設定新 phase

在 Step 1-3 之間，另一個執行緒可能已經改變了 phase，導致：
- 執行緒 A: Idle → 讀取 phase = Idle
- 執行緒 B: Idle → 讀取 phase = Idle → 通過檢查 → 設為 Snapshot
- 執行緒 A: 通過檢查（基於舊的 Idle）→ 設為 Marking（無效轉換！）

這導致 phase 從 Idle 直接跳到 Marking，繞過了 Snapshot 階段，破壞了 incremental marking 的正確性。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC 需要多執行緒並發調用 transition_to
// 需 Miri 或 ThreadSanitizer 驗證
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用 compare-and-swap (CAS) 來實現原子性的 phase 轉換：

```rust
pub fn transition_to(&self, new_phase: MarkPhase) -> bool {
    let current = self.phase.load(Ordering::SeqCst);
    if !self.is_valid_transition(from_raw_phase(current), new_phase) {
        return false;
    }
    // 使用 CAS 確保轉換的原子性
    self.phase
        .compare_exchange(current, new_phase as usize, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}
```

或者在 `set_phase` 內部加入有效性檢查，並使用 CAS。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 incremental marking 中，phase 狀態機的正確性至關重要。每个 phase 都有其语义：
- Idle: GC 空閒
- Snapshot: 拍攝根集快照
- Marking: 標記可達物件
- FinalMark: 最終標記
- Sweeping: 清理階段

錯誤的 phase 轉換會破壞 SATB 不變性，導致存活物件被錯誤回收。

**Rustacean (Soundness 觀點):**
這不是傳統意義的 UB，但可能導致記憶體安全問題：
- Phase 錯誤可能導致 write barrier 行為不一致
- 可能導致 double-sweep 或遺漏標記

**Geohot (Exploit 觀點):**
此 TOCTOU 可被利用來：
- 跳過 Snapshot 階段，使 OLD→YOUNG 引用未被記錄
- 導致 young 物件在 minor GC 時被錯誤回收
- 造成 use-after-free

攻擊需要精確時序控制，但配合其他 bug 可能更容易觸發。

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Replaced the non-atomic load-check-store sequence in `IncrementalMarkState::transition_to()` with a single `compare_exchange` operation. The phase transition is now atomic: we load the current value, validate the transition, then CAS to the new phase only if the stored value still matches our read. If another thread changed the phase in between, the CAS fails and we return `false`. Tracing side effects run only after a successful CAS.
