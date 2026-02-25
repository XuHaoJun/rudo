# [Bug]: GcBoxWeakRef::try_upgrade TOCTOU - is_dead_or_unrooted 檢查與 inc_ref人之間的 Race 導致 Use-After-Free

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：try_upgrade 執行時物件正在被 drop |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::try_upgrade()`, `ptr.rs:530-584`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBoxWeakRef::try_upgrade()` 存在 TOCTOU (Time-Of-Check-Time-Of-Use) race condition。在 `is_dead_or_unrooted()` 檢查和最終的 `inc_ref()` 調用之間存在 race window，允許返回對已死亡物件的 Gc 指標。

### 預期行為 (Expected Behavior)
當物件已死亡（`ref_count == 0` 或 `DEAD_FLAG` 設置）時，`try_upgrade()` 應返回 `None`。

### 實際行為 (Actual Behavior)
1. Thread A 調用 `try_upgrade()`，通過 `is_dead_or_unrooted()` 檢查（ref_count > 0）
2. Thread B 開始 drop 物件：遞減 `ref_count` 到 0，設置 `DEAD_FLAG`
3. Thread A 載入 `ref_count = 0`（line 572）
4. Thread A 檢查 `current_count == usize::MAX`（line 573） - 通過（因為是 0）
5. Thread A 調用 `inc_ref()` - 從 0 增加到 1！→ **Use-After-Free!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:566-578`：

```rust
// ref_count > 0, check again if still alive
if gc_box.is_dead_or_unrooted() {  // line 566
    return None;
}

// Check for overflow (for consistency with public Weak<T>::try_upgrade)
let current_count = gc_box.ref_count.load(Ordering::Acquire);  // line 572
if current_count == usize::MAX {  // line 573
    return None;
}

// Object is alive and has strong refs - increment normally
gc_box.inc_ref();  // line 578
```

問題：
1. `is_dead_or_unrooted()` 內部檢查 `ref_count == 0`（line 305）
2. 如果 ref_count > 0，程式碼繼續執行
3. 在 line 572 再次載入 ref_count
4. 檢查只針對 `usize::MAX`，不檢查 0
5. 如果 ref_count 在這之間變成 0，會呼叫 `inc_ref()` 錯誤地「復活」已死亡物件

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景，理論上的攻擊序列：
1. 建立一個 Gc 物件和其 Weak 引用
2. Thread A 呼叫 `try_upgrade()`，通過初始檢查
3. Thread B 在此時開始 drop 物件
4. Thread A 的 ref_count 載入返回 0
5. Thread A 通過 overflow 檢查（0 != MAX）
6. Thread A 呼叫 `inc_ref()` 返回 Gc 到已死亡物件

建議使用 model checker（如 loom）驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 line 572-578 之後添加第二層驗證：

```rust
// Check for overflow (for consistency with public Weak<T>::try_upgrade)
let current_count = gc_box.ref_count.load(Ordering::Acquire);
if current_count == usize::MAX {
    return None;
}

// Second check: verify object wasn't dropped between check and increment
if current_count == 0 || gc_box.is_dead_or_unrooted() {
    return None;
}

// Object is alive and has strong refs - increment normally
gc_box.inc_ref();
```

或者，使用類似 `try_inc_ref_from_zero()` 的模式，確保原子性。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 RC upgrade TOCTOU 問題。`try_upgrade` 應該在 ref_count 載入和 increment之間確保原子性，或者在 increment 後再次驗證物件狀態。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當物件已死亡時（ref_count == 0），不應允許 increment 操作。現有的檢查順序存在 race window。

**Geohot (Exploit 攻擊觀點):**
雖然需要精確的時序控制，但理論上可以通過構造並發場景來觸發此 bug，導致 use-after-free。如果物件記憶體被重新分配，攻擊者可能可以控制指標指向的內容。

---

## 備註

此問題與 bug119 不同：
- bug119: GcBoxWeakRef::upgrade 中 dropping_state 檢查與 try_inc_ref_from_zero CAS 之間的 race
- 本 bug: try_upgrade 中 is_dead_or_unrooted 檢查與 inc_ref()人之間的 race
