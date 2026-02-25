# [Bug]: Lazy Sweep 發生無窮迴圈 - is_allocated 為 true 時 continue 導致無限循環

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發 allocation 與 lazy sweep 發生競爭時才會觸發 |
| **Severity (嚴重程度)** | Critical | 會導致 GC 完全卡死（無窮迴圈），服務完全無回應 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Lazy Sweep (lazy_sweep_page, lazy_sweep_page_all_dead)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 lazy sweep 的實作中，當嘗試將一個已死亡的 slot 回收到 free list 時，如果該 slot 被並發的 allocation 重新分配（`is_allocated(i)` 返回 true），程式會進入無窮迴圈。

### 預期行為 (Expected Behavior)
當 slot 被並發重新分配時，應該跳過該 slot 並繼續處理下一個 slot。

### 實際行為 (Actual Behavior)
1. Lazy sweep 嘗試將死亡 slot 加入 free list
2. 檢測到 `is_allocated(i)` 為 true（slot 被並發分配）
3. 程式執行 `continue`（第 2565 行或第 2683 行）
4. `continue` 跳到內部 `loop` 區塊的開頭（第 2523 行或第 2641 行）
5. 再次嘗試將 slot 加入 free list
6. `is_allocated(i)` 仍然為 true
7. 永遠重複步驟 3-6，導致無窮迴圈

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/gc.rs` 的 `lazy_sweep_page` 函數中（第 2523-2578 行）：

```rust
loop {  // Line 2523
    // ... compare_exchange 邏輯 ...
    Ok(_) => {
        if (*header).is_allocated(i) {  // Line 2532
            // ... 嘗試從 free list 移除 ...
            continue;  // Line 2565 - 回到 loop 開頭，不是 for 迴圈！
        }
        break;  // Line 2567
    }
    // ...
}
```

問題在於：
1. `continue`（第 2565 行）跳到內部 `loop` 區塊的開頭（第 2523 行）
2. 而非跳到外部的 `for` 迴圈
3. 當 `is_allocated(i)` 持續為 true 時，永遠無法離開內部 `loop`

同樣的問題也存在於 `lazy_sweep_page_all_dead` 函數（第 2641-2696 行）。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 需要多執行緒環境：
   - Thread A: 進行 lazy sweep
   - Thread B: 同時進行 allocation，恰好重用同一個 slot
3. 時序條件：Thread A 檢測到 `is_allocated(i)` 為 true 時，Thread B 剛好完成 allocation

理論上可透過 Miri 或 ThreadSanitizer 檢測。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `continue` 改為 `break`，讓內部 `loop` 結束後繼續執行外層 `for` 迴圈：

```rust
// 在 lazy_sweep_page 中：
if (*header).is_allocated(i) {
    // ... 現有邏輯 ...
    break;  // 改為 break，離開內部 loop
}
// break;  // 這個 break 會在 if 外面，永遠不會執行
```

或者，在 `is_allocated(i)` 為 true 時直接 `break`，因為該 slot 已被其他執行緒佔用。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 的設計目標是減少 GC pause 時間，但當有多執行並發 allocation 時，必須妥善處理 slot 被重新分配的情況。此 bug 會完全破壞 lazy sweep 的優勢，因為它會導致 GC 完全停止。

**Rustacean (Soundness 觀點):**
此問題屬於邏輯錯誤（logic error）而非 UB，但會導致程式 hang。在多執行緒環境下，這可能導致整個程式無法響應請求。

**Geohot (Exploit 觀點):**
這是一個 DoS（拒絕服務）向量。攻擊者可以嘗試觸發精確的時序條件來使 GC 完全卡死。雖然不會導致記憶體錯誤，但可以有效癱瘓服務。

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

In both `lazy_sweep_page` and `lazy_sweep_page_all_dead`, changed `continue` to `break` when `is_allocated(i)` is true after CAS success (slot was concurrently allocated). Added `did_reclaim` flag so we only call `clear_allocated` and increment `reclaimed` when we actually added the slot to the free list; when we skip due to concurrent allocation we exit the inner loop and advance to the next slot without corrupting state.
