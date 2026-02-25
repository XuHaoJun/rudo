# [Bug]: GcBoxWeakRef::upgrade TOCTOU - dropping_state 檢查與 try_inc_ref_from_zero CAS 之間的 Race 導致 Use-After-Free

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：upgrade 執行時物件正在被 drop |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()`, `GcBoxWeakRef::try_upgrade()`, `ptr.rs:422-457, 530-584`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBoxWeakRef::upgrade()` 和 `GcBoxWeakRef::try_upgrade()` 存在 TOCTOU (Time-Of-Check-Time-Of-Use) race condition。即使 Bug75 修復了 `dropping_state` 檢查的順序（在 `try_inc_ref_from_zero` 之前檢查），仍然存在一個 race window 允許返回對已死亡物件的 Gc 指標。

### 預期行為 (Expected Behavior)
當物件正在被 drop（`dropping_state != 0`）時，`upgrade()` 應返回 `None`，不應允許復活已死亡的物件。

### 實際行為 (Actual Behavior)
即使 `dropping_state()` 檢查在 `try_inc_ref_from_zero()` 之前，在這兩者之間仍然存在一個 race window：
1. Thread A 檢查 `dropping_state() != 0` → 返回 `false`（物件不在 dropping 狀態）
2. Thread B 開始 drop 物件：設置 `dropping_state = 1`，設置 `DEAD_FLAG`，遞減 `ref_count` 到 `0`
3. Thread A 執行 `try_inc_ref_from_zero()`：載入 `ref_count = 0`，檢查通過，CAS 成功
4. Thread A 返回 `Some(Gc { ... })` → **Use-After-Free!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:439-449`：

```rust
// 檢查 dropping_state - 在 line 439
if gc_box.dropping_state() != 0 {
    return None;
}

// Race window: 在這裡另一個執行緒可以：
// 1. 設置 dropping_state = 1
// 2. 設置 DEAD_FLAG
// 3. 遞減 ref_count 到 0

// Try atomic transition from 0 to 1 (resurrection) - 在 line 444
if gc_box.try_inc_ref_from_zero() {
    // BUG: 沒有第二層檢查！直接返回 Gc
    return Some(Gc {
        ptr: AtomicNullable::new(ptr),
        _marker: PhantomData,
    });
}
```

`try_inc_ref_from_zero()` 內部會檢查：
- `(flags & Self::DEAD_FLAG) != 0` - 可能已設置
- `ref_count != 0` - 可能已變成 0

但在載入和 CAS 之間，另一個執行緒可以修改這些值，導致 CAS 成功但物件已死亡。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景，理論上的攻擊序列：
1. 建立一個即將被 drop 的 Gc object（ref_count = 1）
2. Thread A 呼叫 `upgrade()`，通過 `dropping_state() != 0` 檢查
3. Thread B 在此時開始 drop 物件
4. Thread A 的 `try_inc_ref_from_zero()` CAS 成功
5. Thread A 返回 `Some(Gc)` 到已死亡物件

建議使用 model checker（如 loom）驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_inc_ref_from_zero()` 成功後添加第二層驗證：

```rust
// Try atomic transition from 0 to 1 (resurrection)
if gc_box.try_inc_ref_from_zero() {
    // Second check: verify object wasn't dropped between check and CAS
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        // Undo the increment and return None
        gc_box.dec_ref();
        return None;
    }
    return Some(Gc { ... });
}
```

或者，在 `try_inc_ref_from_zero()` 內部添加 `dropping_state` 檢查，使其成為原子操作的一部分。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 RC upgrade TOCTOU 問題。解決方案是確保檢查和 CAS 是原子操作，或者在 CAS 之後進行第二層驗證。Bug75 只修復了檢查順序，但沒有消除 race window。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。即使有外部 weak reference 存在，當物件正在被 drop 時也不應允許 upgrade 成功。需要在 CAS 之後再次驗證物件狀態。

**Geohot (Exploit 攻擊觀點):**
雖然需要精確的時序控制，但理論上可以通過構造並發場景來觸發此 bug，導致 use-after-free。如果物件記憶體被重新分配，攻擊者可能可以控制指標指向的內容。
