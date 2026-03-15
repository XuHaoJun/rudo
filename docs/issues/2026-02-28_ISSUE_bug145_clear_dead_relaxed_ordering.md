# [Bug]: GcBox::clear_dead 使用 Relaxed Ordering 導致潛在 Race Condition

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要高並發場景：sweep/allocation 執行 clear_dead 的同時有 GC 讀取 |
| **Severity (嚴重程度)** | Medium | 可能導致新分配的物件被錯誤視為 dead，但不會造成 memory safety 問題 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制才能穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::clear_dead()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBox::clear_dead()` 函數在清除 `DEAD_FLAG` 時使用 `Ordering::Relaxed`（ptr.rs:373-376）。當 slot 被重用於新分配時，clear_dead() 被調用以清除 DEAD_FLAG，但使用 Relaxed ordering 可能導致並發的 GC 執行緒讀取到過時的值。

### 預期行為 (Expected Behavior)

當 slot 被重用於新分配時，`DEAD_FLAG` 應該被清除，且清除操作應該對並發的 GC 執行緒可見。

### 實際行為 (Actual Behavior)

由於 `clear_dead()` 使用 `Ordering::Relaxed`，並發執行 GC 的執行緒在調用 `has_dead_flag()` 時可能會看到過時的值（DEAD_FLAG 仍被設定），導致新分配的物件被錯誤地視為 dead。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **Slot 重用路徑** (heap.rs): 當 slot 被重用於新分配時，`clear_dead()` 使用 `Ordering::Relaxed` 清除 DEAD_FLAG

2. **潛在 Race Condition**:
   - 執行緒 A (GC sweep): 在物件上設定 DEAD_FLAG
   - 執行緒 B (allocation): 重用 slot，調用 `clear_dead()` 使用 Relaxed ordering
   - 執行緒 C (並發 GC marking): 調用 `has_dead_flag()` - 可能看到過時的值
   - **結果**: 新分配的物件被錯誤地視為 dead，在 GC 標記階段被跳過

3. **相關函數**:
   - `clear_dead()` (ptr.rs:373-376): 使用 `fetch_and(!DEAD_FLAG, Ordering::Relaxed)`
   - `has_dead_flag()` (ptr.rs:320-322): 使用 `load(Ordering::Relaxed)`
   - `set_dead()` (ptr.rs:325-327): 使用 `fetch_or(DEAD_FLAG, Ordering::Relaxed)`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要高並發場景才能穩定復現：

```rust
// 需要多執行緒並發測試
// 1. 建立大量物件並讓其死亡
// 2. 同時進行 GC 執行
// 3. 同時進行新物件分配（重用 slot）
// 4. 驗證新物件是否被錯誤標記為 dead
```

**注意**: 由於需要精確的時序控制，此 bug 難以穩定復現。建議使用 ThreadSanitizer 或 Miri 輔助驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **選項 A**: 將 `clear_dead()` 改為使用 `Ordering::Release`，確保清除操作對後續讀取可見
2. **選項 B**: 將 `has_dead_flag()` 改為使用 `Ordering::Acquire`，確保讀取能看到之前的清除操作

類似於 bug143 的修復方案，應該確保 clear/has 配對使用適當的 memory ordering 來建立 happens-before 關係。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題與 bug143（clear_gen_old 使用 Relaxed）屬於同一類別。在 GC 中，flag 的清除必須對並發的 GC 執行緒可見，否則會導致錯誤的行為。使用 Release ordering 清除 flag 可以確保清除操作在後續的 GC 標記之前完成。

**Rustacean (Soundness 觀點):**
此問題不會導致 memory safety 問題（不會造成 UAF 或 double free），但可能導致邏輯錯誤（新物件被錯誤回收）。Relaxed ordering 在此場景下確實不足，需要更強的 ordering 來確保正確性。

**Geohot (Exploit 觀點):**
此 bug 可能被利用來實現特定的攻擊場景，例如：通過精心設計的時序，使新分配的物件被錯誤視為 dead，進而影響 GC 的正確性。但實際利用難度較高。
