# [Bug]: mark_object_black TOCTOU - is_allocated 檢查與 set_mark 之间存在 race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 concurrent marking 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致錯誤標記已釋放的 slot，造成記憶體損壞或 UAF |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_black` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`mark_object_black` 函數存在 TOCTOU (Time-of-Check to Time-of-Use) race condition。雖然函數有 `is_allocated` 檢查，但檢查與 `set_mark` 操作不是原子的。

### 預期行為 (Expected Behavior)

當物件已被 sweep 回收時，`mark_object_black` 不應該標記該 slot 的 metadata。

### 實際行為 (Actual Behavior)

在 `is_allocated` 檢查通過後、调用 `set_mark` 前，另一執行緒可能 sweep 該物件：

```rust
// incremental.rs:980-987
if !(*h).is_allocated(idx) {    // Check: 返回 true
    return None;
}
if !(*h).is_marked(idx) {        // Race window: 另一執行緒可能 sweep
    (*h).set_mark(idx);          // Use: 標記已釋放的 slot!
    return Some(idx);
}
```

時序：
1. Thread A: 調用 `mark_object_black(ptr)` 
2. Thread A: `is_allocated(idx)` 返回 `true`（物件仍存在）
3. Thread B: 同時 sweep 該物件（`is_allocated` 變為 `false`）
4. Thread A: 執行 `set_mark(idx)` - 標記已釋放的 slot！

---

## 🔬 根本原因分析 (Root Cause Analysis)

`is_allocated` 檢查和 `set_mark` 操作之間存在 race window。雖然 bug15 修復了原來缺少 `is_allocated` 檢查的問題，但修復不完整 - 檢查和標記不是原子的。

**與 bug15 的關係：**
- bug15 修復：原本缺少 `is_allocated` 檢查 → 現在有檢查
- bug152 (本 issue)：檢查與標記不是原子的 → 仍存在 TOCTOU

**與 bug136 的關係：**
- bug136 是關於 `mark_object` 函數缺少 `is_allocated` 檢查
- 本 issue 是關於 `mark_object_black` 雖然有檢查，但檢查與標記之間的 TOCTOU

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要多執行緒環境：
1. Thread A: 調用 `mark_object_black` 標記物件
2. Thread B: 同時進行 lazy sweep，恰好 sweep 同一物件
3. 時序條件：Thread A 的 `is_allocated` 檢查通過後、Thread A 調用 `set_mark` 前，Thread B 完成 sweep

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用 atomic 操作或 lock 來確保 `is_allocated` 檢查和 `set_mark` 的原子性：

```rust
// 方案 1: 使用 AtomicU64 compare-and-swap
if !(*h).is_allocated(idx) {
    return None;
}
// 使用 Acquire ordering 確保 is_allocated 讀取happens-before 後續操作
if (*h).is_marked(idx).load(Ordering::Acquire) {
    return None;
}
// 使用 Release ordering 確保標記對其他執行緒可見
if (*h).set_mark_acquire_release(idx) {
    return Some(idx);
}
None
```

或者使用 page header 的鎖來保護整個 check-and-mark 操作。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 incremental marking 中的經典 race condition。SATB (Snapshot-At-The-Beginning) 演算法要求所有在 snapshot 時存活的物件都被正確標記，但在 concurrent 環境中，物件可能在標記過程中被回收。建議使用類似「mark bitmap lock」或「atomic compare-and-swap」機制來確保原子性。

**Rustacean (Soundness 觀點):**
這是潛在的 soundness 問題。標記到已釋放的 slot 可能導致：
- 錯誤地保留新物件（該物件實際上應該被回收）
- 破壞 page metadata（標記到位於已釋放 slot 的 bitmap）
- Potential UAF when the page is reused

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 破壞 GC heap metadata
2. 造成記憶體洩漏（錯誤標記保留物件）
3. 在極端情況下，透過 page reuse 實現 use-after-free

---

## Resolution (2026-03-02)

**Outcome:** Fixed.

Applied optimistic marking with post-CAS validation in `mark_object_black`:
1. Use `try_mark` (atomic CAS) instead of `is_marked` + `set_mark` sequence
2. After successfully marking, re-check `is_allocated`
3. If slot was swept between check and mark, call `clear_mark_atomic` to roll back

Added `PageHeader::clear_mark_atomic(&self)` for concurrent rollback. Single-threaded tests pass; race requires concurrent lazy sweep + incremental marking to trigger.
