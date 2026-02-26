# [Bug]: Ephemeron::upgrade() TOCTOU race condition - key alive check and value clone not atomic

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 key 存活的短暫時間窗口內呼叫 upgrade |
| **Severity (嚴重程度)** | Medium | 可能導致 use-after-free 或存取無效物件 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Ephemeron::upgrade()` in `ptr.rs:2055-2062`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`Ephemeron::upgrade()` 函數存在 TOCTOU (Time-of-Check-Time-of-Use) 競爭條件。

### 預期行為
當 key 存活時，`upgrade()` 應該返回 `Some(Gc<V>)`，否則返回 `None`。

### 實際行為
`upgrade()` 調用 `is_key_alive()` 檢查 key 是否存活，然後調用 `Gc::try_clone(&self.value)` 克隆 value。問題是這兩個操作不是原子的 - 在檢查和克隆之間，key 或 value 可能會變得無效。

```rust
// ptr.rs:2055-2062
pub fn upgrade(&self) -> Option<Gc<V>> {
    if self.is_key_alive() {  // <-- 檢查 key 存活
        // Clone the Gc to return - this increments the ref count
        Gc::try_clone(&self.value)  // <-- 克隆 value (中間可能會有其他線程改變狀態)
    } else {
        None
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. `is_key_alive()` 返回 true（key 存活）
2. 在調用 `Gc::try_clone(&self.value)` 之前，key 突然變得無效（例如被 GC 回收）
3. `Gc::try_clone()` 可能返回 `Some`（如果 value 仍然有效）或 `None`（如果 value 也變得無效）
4. 如果返回 `Some`，調用者可能會獲得一個 value Gc，但其 key 已經無效

這導致不一致的行爲：
- 如果 key 變得無效，我們預期返回 `None`
- 但由於 TOCTOU，可能會錯誤地返回 `Some`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確控制時序：
1. 創建一個 Ephemeron，其中 key 和 value 都是 GC 對象
2. 在另一個線程中，同時進行：
   - 刪除 key 的所有強引用（使其變得無效）
   - 調用 `ephemeron.upgrade()`
3. 如果時序正確，可能會在 key 變得無效的瞬間調用 upgrade，導致不一致的行爲

---

## 🛠️ 建議修復方案 (Suggested Fix)

將 `is_key_alive()` 檢查和 `try_clone()` 合併为一個原子操作：

```rust
pub fn upgrade(&self) -> Option<Gc<V>> {
    // 直接嘗試升級 key，這會原子地檢查並遞增引用計數
    if let Some(_key_gc) = self.key.upgrade() {
        // Key 存活，現在安全地克隆 value
        Gc::try_clone(&self.value)
    } else {
        None
    }
}
```

或者，使用更嚴格的方法：

```rust
pub fn upgrade(&self) -> Option<Gc<V>> {
    // 確保 key 存活的同時不釋放鎖
    let key_valid = self.key.upgrade().is_some();
    if !key_valid {
        return None;
    }
    
    // Key 存活，現在克隆 value
    Gc::try_clone(&self.value)
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Ephemeron 的核心語義是「當 key 存活時，value 才應該可達」。TOCTOU 破壞了這個不變性，可能導致 value 在 key 無效後仍然可達，這與 GC 的預期行爲不一致。

**Rustacean (Soundness 觀點):**
雖然這可能不會導致傳統意義上的 use-after-free（因爲 value 仍然有效），但它確實破壞了 Ephemeron 的語義，導致不應該存活的 value 仍然可以被訪問。

**Geohot (Exploit 觀點):**
在並發環境中，攻擊者可能利用這個 TOCTOU 來繞過 GC 的安全檢查，特別是在依賴 Ephemeron 進行資源管理的應用程序中。

---

**相關 Bug:**
- bug57: Ephemeron trace 行爲（已修復）
- bug76: Ephemeron clone 創建 null value（已修復）
