# [Bug]: mark_and_trace_incremental 缺少 clear_mark_atomic 導致殘留標記位元

**Status:** Fixed
**Tags:** Verified

**Fixed by:** Adding `clear_mark_atomic(idx)` call at line 2455 in `gc/gc.rs`

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 try_mark 成功後 slot 被回收的時序下觸發 |
| **Severity (嚴重程度)** | High | 可能導致記憶體洩漏或錯誤回收存活物件 |
| **Reproducibility (復現難度)** | Medium | 需特定時序條件 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`gc/gc.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

### 預期行為
當 `try_mark(idx)` 成功設置標記後，若後續發現該 slot 已被回收（`is_allocated` 返回 false），應清除已設置的標記位元，確保死物件不會被錯誤視為存活。

### 實際行為
在 `mark_and_trace_incremental` 函數中，當 `try_mark` 成功但 slot 未分配時，函數直接返回，**未清除標記位元**。

---

## 🔬 根本原因分析 (Root Cause Analysis)

位於 `crates/rudo-gc/src/gc/gc.rs:2454-2457`:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        return;  // BUG: 返回時未清除 try_mark 設置的標記!
    }
```

對比同檔案 `mark_object` 函數 (lines 2398-2406) 的正確實現:

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;
        }
        (*header.as_ptr()).clear_mark_atomic(idx);  // 正確清除標記
        return;
    }
    // ...
}
```

`mark_and_trace_incremental` 缺少 `clear_mark_atomic(idx)` 調用，導致已設置的標記位元残留在已回收的 slot 上。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要以下時序條件:
1. 執行 incremental marking
2. 在 `try_mark(idx)` 成功後、讀取 `generation()` 前
3. slot 被 sweep 回收 (`is_allocated` 變 false)
4. 函數返回但未清除標記

修復建議:
```rust
// gc.rs:2454-2457 應改為:
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);  // 新增: 清除標記
        return;
    }
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `crates/rudo-gc/src/gc/gc.rs:2455-2457` 的 return 前添加 `clear_mark_atomic(idx)` 調用，與 `mark_object` 函數的處理邏輯保持一致。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
殘留的標記位元會導致 sweep 時將已回收的 slot 視為存活，阻止記憶體回收。長期下來可能導致記憶體洩漏，尤其在大量 incremental marking 的工作負載下。

**Rustacean (Soundness 觀點):**
這是邏輯錯誤而非 UB，但會造成記憶體資源管理問題。標記位元殘留本身不會造成 use-after-free，因為標記只影響回收決策。

**Geohot (Exploit 觀點):**
需要精確時序控制才能利用此 bug，實用性低。但長期記憶體洩漏仍可作為 denial-of-service 攻擊向量。