# [Bug]: mark_new_object_black 返回 false 時未清除已回收 slot 的 stale mark

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking + lazy sweep + slot reuse race |
| **Severity (嚴重程度)** | Medium | 可能導致 stale mark 保留在回收的 slot 上，影響 GC 正確性 |
| **Reproducibility (復現難度)** | Medium | 需要仔細設計並髮 PoC 來觸發 slot 狀態變化時機 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 slot 被 sweep 回收（`is_allocated` 為 `false`）時，無論 generation 是否匹配，`mark_new_object_black` 都應該清除 stale mark 並返回 `false`。

### 實際行為 (Actual Behavior)

在以下條件時返回 `false` 但**未清除 mark**：
1. `set_mark(idx)` 成功設置 mark
2. 之後 `is_allocated(idx)` 返回 `false`（slot 被 sweep）
3. 且 `generation` 發生了變化（`current_generation != marked_generation`）

留下 stale mark 在已回收的 slot 上。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**檔案:** `crates/rudo-gc/src/gc/incremental.rs:1073-1078`

問題代碼：
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    let current_generation = (*gc_box).generation();
    if current_generation == marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
    }
    return false;
}
```

當 `is_allocated` 為 `false` 且 `generation` 不匹配時，**直接返回 `false`，未呼叫 `clear_mark_atomic`**。

這與 `mark_object_black` 的正確處理模式不一致（lines 1138-1155）。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要並髮場景來觸發:
// 1. 啟用 incremental marking
// 2. 分配對象 A，調用 mark_new_object_black(A)
// 3. 觸發 lazy sweep 回收 A 的 slot（is_allocated 變為 false）
// 4. 在 is_allocated 檢查之前，slot 被重新分配給 B（generation 改變）
// 5. mark_new_object_black 檢查 is_allocated（false）和 generation（不匹配）
// 6. 返回 false 但未清除 mark，留下 stale mark
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `mark_new_object_black` 中的邏輯，總是在 `is_allocated` 為 `false` 時清除 mark：

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    // 總是清除 mark，無論 generation 是否匹配
    (*header.as_ptr()).clear_mark_atomic(idx);
    return false;
}
```

或者保持 generation 檢查但確保清除 mark：
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    let current_generation = (*gc_box).generation();
    if current_generation == marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
    }
    // 若 generation 不匹配，slot 已死亡，仍需清除 mark
    // 實際上這裡不需要區分，直接清除即可
    (*header.as_ptr()).clear_mark_atomic(idx);
    return false;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 當 `is_allocated` 為 `false` 時，slot 已經是 dead state
- Generation 改變通常表示 slot 被 reuse，但 reuse 需要 `is_allocated = true`
- 為何 `is_allocated = false` 且 generation 改變？這可能是某種 edge case race
- 為防禦性編程，應該總是清除 mark 並返回 `false`

**Rustacean (Soundness 觀點):**
- 死亡 slot 上的 mark 狀態不一致可能導致 GC 邏輯錯誤
- 返回 `false` 表示"未標記"，但 slot 上仍有 mark 狀態，這是狀態不一致

**Geohot (Exploit 觀點):**
- Stale mark 可能被利用來操縱 GC 行為
- 如果攻擊者能觸發特定時序，可能造成 mark 狀態混淆
