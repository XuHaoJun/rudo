# [Bug]: Lazy Sweep 未設置 all_dead = false 導致並發配置時頁面狀態錯誤

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 lazy sweep 過程中並發分配 |
| **Severity (嚴重程度)** | Medium | 可能導致頁面狀態不一致，但不會造成嚴重的記憶體錯誤 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `lazy_sweep_page` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

在 `lazy_sweep_page` 函數中，當並發分配發生時，程式碼未能正確設置 `all_dead = false`。

### 預期行為

當 lazy sweep 過程中發現頁面有存活物件（either weak refs 或並發分配的新物件），`all_dead` 應設置為 `false`，表示頁面並非完全死亡。

### 實際行為

在 `gc.rs:2663`，當 slot 被並發分配時，程式碼只是 break 跳過該 slot，但沒有設置 `all_dead = false`。這導致頁面可能被錯誤地視為「全部死亡」。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/gc.rs:2595-2667`，當處理一個 unmarked (dead) object 時：

1. Line 2595: 檢查 `is_allocated && !is_marked` - 找到一個死亡物件
2. Line 2600-2614: 如果 weak_count == 0，嘗試回收並加入 free list
3. Line 2623-2628: CAS 嘗試認領該 slot
4. Line 2630: 再次檢查 `is_allocated(i)` - 如果返回 true，表示並發配置了新的物件
5. Line 2631-2662: 嘗試回滾 free list 變更
6. **Line 2663: `break` 跳過，但沒有設置 `all_dead = false`！**

`all_dead` 只在以下位置被設置為 false：
- Line 2609: `weak_count > 0`
- Line 2686: 在非 all_dead 模式下發現 marked 物件

但當並發配置發生時，這兩種情況都不會觸發，因此 `all_dead` 保持為 `true`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制，理論上的 PoC：

```rust
// 需要多執行緒環境
// Thread A: 執行 lazy_sweep_page
// Thread B: 同時在該 page 上配置新物件
// 驗證 all_dead 是否正確反映頁面狀態
```

實際上，這個 bug 可能是理論性的，因為 lazy sweep 通常在 STW 期間執行。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `crates/rudo-gc/src/gc/gc.rs:2663` 的 break 之前添加：

```rust
if (*header).is_allocated(i) {
    // Slot was concurrently allocated, page is not all dead
    let next_head = current_free;
    // ... existing rollback code ...
    all_dead = false;  // 添加這行
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 會導致 lazy sweep 對頁面狀態的判斷不正確。在並發 GC 環境中，這可能導致頁面被錯誤地歸類為「全部死亡」，影響後續的記憶體管理決策。但由於 lazy sweep 通常在 STW 期間執行，實際影響可能有限。

**Rustacean (Soundness 觀點):**
這不是傳統的 UB，但可能導致記憶體管理邏輯錯誤。如果頁面被錯誤地視為全部死亡，後續配置策略可能會受到影響。

**Geohot (Exploit 觀點):**
在极端情况下，页面状态错误可能导致内存分配器做出不当决策。但考虑到 lazy sweep 的执行时机，这不太可能被利用。

---

## ✅ 修復記錄 (Fix Record)

- **Date:** 
- **Fix:**

---

## 🔍 驗證記錄 (Verification)

已確認 bug 存在於 `crates/rudo-gc/src/gc/gc.rs:2663`:

```rust
// Line 2630-2663
if (*header).is_allocated(i) {
    // Slot was concurrently allocated - rollback free list changes
    let next_head = current_free;
    // ... rollback code ...
    break; // BUG: 沒有設置 all_dead = false!
```

問題分析:
1. 當 weak_count == 0 且並發配置發生時，程式進入此分支
2. 在 break 之前，all_dead 仍然是 true
3. 如果後續所有 slot 都是並發配置，永遠不會觸發 line 2686 的 else 分支
4. 導致函數返回錯誤的 all_dead 狀態

修復方法: 在 break 之前添加 `all_dead = false;`
