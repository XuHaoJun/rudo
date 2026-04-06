# [Bug]: lazy_sweep_page 在 slot 重用前錯誤呼叫 set_dead()，與 lazy_sweep_page_all_dead 行為不一致

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需觸發 lazy-sweep 且 page 上所有物件都是 dead |
| **Severity (嚴重程度)** | Medium | 可能導致 DEAD_FLAG 設置不正確 |
| **Reproducibility (復現難度)** | High | 需精確控制物件狀態與 lazy-sweep 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/gc.rs`, `lazy_sweep_page`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當物件即將被重用（加入 free list）時，不應該設置 `DEAD_FLAG`。
當物件即將被保留（不重用）但標記為 dead 時，才應該設置 `DEAD_FLAG`。

### 實際行為 (Actual Behavior)
在 `lazy_sweep_page` 的 else branch（gc.rs:2633-2636）：
```rust
} else {
    (*gc_box_ptr).set_dead();  // 錯誤：物件即將被重用，不應設置 DEAD_FLAG
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    (*gc_box_ptr).clear_is_dropping();
```

在 `lazy_sweep_page_all_dead` 的 else branch（gc.rs:2762-2765）：
```rust
} else {
    // 正確：沒有呼叫 set_dead()
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    (*gc_box_ptr).clear_is_dropping();
```

`lazy_sweep_page` 錯誤地在物件被重用前設置了 `DEAD_FLAG`，而 `lazy_sweep_page_all_dead` 正確地沒有這樣做。

---

## 🔬 根本原因分析 (Root Cause Analysis)

兩個函數的 else branch 條件相同：`!(weak_count > 0 && !dead_flag)`，即 `weak_count == 0 || dead_flag == true`。

在這個條件下：
- `weak_count == 0`：物件沒有 weak references，將被重用
- `dead_flag == true`：物件已經有 DEAD_FLAG，將被重用

物件即將被重用加入 free list 時，根據 gc.rs:2813 的註解：
```rust
// Clear DEAD_FLAG, GEN_OLD_FLAG, UNDER_CONSTRUCTION_FLAG, and is_dropping so
// reused slots don't inherit stale state.
```

重用 slots 時不應該繼承 stale state，包括 `DEAD_FLAG`。

但 `lazy_sweep_page` 在這個 branch 呼叫了 `set_dead()`，導致 `DEAD_FLAG` 在 slot 重用前被設置。這可能導致：
1. 重用後的新物件繼承了錯誤的 DEAD_FLAG
2. 後續的 `has_dead_flag()` 檢查可能返回錯誤結果

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `lazy-sweep` feature
2. 建立一個 Gc 物件，確保它成為 dead（無 strong refs）
3. 觸發 lazy sweep，確保 `lazy_sweep_page` 被調用
4. 觀察物件的 `DEAD_FLAG` 狀態是否正確

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `lazy_sweep_page` 的 else branch 移除 `set_dead()` 呼叫：

```rust
} else {
    // 移除 set_dead() 呼叫
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    (*gc_box_ptr).clear_is_dropping();
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
當物件即將被重用加入 free list 時，不應該設置 DEAD_FLAG。這是 consistent memory management 的基本原則。

**Rustacean (Soundness 觀點):**
設置 DEAD_FLAG 後立即重用 slot 可能導致新物件繼承錯誤的狀態，這是一個潛在的內存安全問題。

**Geohot (Exploit 攻擊觀點):**
如果新物件繼承了 DEAD_FLAG，可能導致 double-free 或其他記憶體相關問題。