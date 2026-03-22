# [Bug]: mark_new_object_black 返回 true 時未清除已回收 slot 的 mark

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking + lazy sweep + slot sweep+reuse race |
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

`mark_new_object_black` 函數在以下條件下返回 `true` 但未清除 mark：
1. `set_mark(idx)` 成功設置 mark
2. 之後 `is_allocated(idx)` 返回 `false`（slot 被 sweep）
3. 但 `generation` 發生了變化

問題是：當 `is_allocated` 為 `false` 時，slot 已經死亡，不可能被 reuse。如果 generation 發生變化但 `is_allocated` 仍為 `false`，這表示存在不一致的狀態，但我們不應該在這種情況下返回 `true`。

### 預期行為 (Expected Behavior)
當 `is_allocated` 為 `false` 時，無論 generation 是否匹配，都應該清除 mark 並返回 `false`。

### 實際行為 (Actual Behavior)
當 `is_allocated` 為 `false` 且 generation 不匹配時，返回 `true` 但不清除 mark，留下 stale mark。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**檔案:** `crates/rudo-gc/src/gc/incremental.rs:1073-1077`

問題代碼：
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        return true;  // BUG: Returns true without clearing the mark!
    }
    (*header.as_ptr()).clear_mark_atomic(idx);
    return false;
}
```

對比 `mark_object_black` 的正確處理模式（lines 1138-1155）：
```rust
Ok(true) => {
    let marked_generation = (*gc_box).generation();
    if (*h).is_allocated(idx) {
        return Some(idx);
    }
    // Slot was swept between our check and try_mark.
    // Verify generation hasn't changed to distinguish swept from swept+reused.
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object, don't clear.
        return None;
    }
    // Slot was swept but not reused - safe to clear mark.
    (*h).clear_mark_atomic(idx);
    return None;
}
```

關鍵差異：
- `mark_object_black` 在 `is_allocated` 為 `false` 時，**只有當 generation 匹配**才清除 mark 並返回
- `mark_new_object_black` 在 `is_allocated` 為 `false` 且 generation **不匹配**時，返回 `true` 而不清除

但這裡有個問題：當 `is_allocated` 為 `false` 時，generation 不匹配意味著什麼？
- 如果 slot 被 reuse，`is_allocated` 應該為 `true`（reuse 需要先 allocate）
- `is_allocated` 為 `false` 意味著 slot 已經被 sweep 回收

所以當 `is_allocated` 為 `false` 且 generation 不匹配時，這是一個不應該到達的狀態。但代碼沒有正確處理這個邊界情況。

**正確的邏輯應該是：**
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        // 理論上不應該發生，但為了安全還是清除
        (*header.as_ptr()).clear_mark_atomic(idx);
    } else {
        (*header.as_ptr()).clear_mark_atomic(idx);
    }
    return false;
}
```

或者更簡潔：
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return false;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要並髮場景來觸發:
// 1. 啟用 incremental marking
// 2. 分配對象 A，調用 mark_new_object_black(A)
// 3. 觸發 lazy sweep 回收 A 的 slot（is_allocated 變為 false）
// 4. 在 is_allocated 檢查之前，slot 被重新分配給 B（generation 改變）
// 5. mark_new_object_black 檢查 is_allocated（false）和 generation（不匹配）
// 6. 返回 true 但未清除 mark
//
// 注意：這個 race window 非常小，但理論上存在
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `mark_new_object_black` 中的邏輯：

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    let current_generation = (*gc_box).generation();
    if current_generation == marked_generation {
        (*header.as_ptr()).clear_mark_atomic(idx);
    }
    // else: generation changed but is_allocated is false - slot is dead, don't return true
    return false;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 當 `is_allocated` 為 `false` 時，slot 應該是 dead state
- Generation 改變通常表示 slot 被 reuse，但 reuse 需要 `is_allocated = true`
- 如果 `is_allocated` 為 `false` 且 generation 改變，可能是某種 edge case race
- 為了防禦性編程，應該總是清除 mark 並返回 `false`

**Rustacean (Soundness 觀點):**
- 返回 `true` 當 `is_allocated` 為 `false` 是邏輯錯誤
- `true` 應該表示"對象被成功標記"
- 死亡對象不應該被認為是"成功標記"

**Geohot (Exploit 觀點):**
- Stale mark 可能被利用來操縱 GC 行為
- 如果攻擊者能觸發特定時序，可能造成 mark 狀態混淆
- 這可能導致對象被錯誤地保留或回收

(End of file - total 162 lines)