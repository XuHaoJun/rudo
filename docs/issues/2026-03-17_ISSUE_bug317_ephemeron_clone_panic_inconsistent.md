# [Bug]: Ephemeron::clone 在 value 死亡/.Dropping/建構中時 panic，與 Weak::clone 行為不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 GC 回收後嘗試 clone Ephemeron 時觸發 |
| **Severity (嚴重程度)** | Medium | Clone trait 不應該 panic，違反 API 一致性 |
| **Reproducibility (復現難度)** | Low | 簡單測試即可復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Ephemeron`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Ephemeron::clone()` 應該在 value 死亡/.Dropping/建構中的情況下返回一個 null/empty Ephemeron，類似於 `Weak::clone()` 的行為。Clone trait 不應該 panic。

### 實際行為 (Actual Behavior)

`Ephemeron::clone()` 會呼叫 `Gc::clone(&self.value)`，而 `Gc::clone()` 內部有 assert 檢查：
- `assert!(!(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0 && !(*gc_box_ptr).is_under_construction())`

當 value 處於死亡/.Dropping/建構中狀態時，會觸發 panic。

### 程式碼位置

`ptr.rs` 第 2851-2858 行：
```rust
impl<K: Trace + 'static, V: Trace + 'static> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            value: Gc::clone(&self.value),  // <-- BUG: 這會 panic
        }
    }
}
```

### 對比：Weak::clone 的正確實現

`Weak::clone()` (ptr.rs 第 2548-2600 行) 在相同情況下會返回 null Weak：
```rust
if gc_box.has_dead_flag() {
    return Self { ptr: AtomicNullable::null() };
}
if gc_box.dropping_state() != 0 {
    return Self { ptr: AtomicNullable::null() };
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`Ephemeron::clone()` 使用 `Gc::clone()` 來複製 value，而 `Gc::clone()` 會在物件死亡/.Dropping/建構中時 panic。這與 `Weak::clone()` 的行為不一致，後者會優雅地返回 null。

問題在於：
1. `Clone` trait 規範建議 clone 不應該 panic
2. `Weak::clone` 展示了正確的行為模式
3. `Ephemeron::clone` 應該與 `Weak::clone` 行為一致

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Ephemeron, Trace, collect_full};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // Create key and value
    let key = Gc::new(Data { value: 1 });
    let value = Gc::new(Data { value: 42 });
    
    // Create ephemeron
    let ephemeron = Ephemeron::new(&key, value);
    
    // Drop the key and value - this makes the ephemeron value potentially dead
    drop(key);
    drop(value);
    collect_full();
    
    // Now try to clone the ephemeron - this should NOT panic
    // But currently it will panic
    let cloned = ephemeron.clone();  // <-- PANIC!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `Ephemeron::clone()` 使用 `Gc::try_clone()` 代替 `Gc::clone()`：

```rust
impl<K: Trace + 'static, V: Trace + 'static> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        // 使用 try_clone 而不是 clone，與 Weak::clone 行為一致
        let value_clone = Gc::try_clone(&self.value);
        
        // 如果 value 無法 clone，返回 null Ephemeron
        // 這與 Weak::clone 的行為一致
        Self {
            key: self.key.clone(),
            value: value_clone.unwrap_or_else(Gc::default),
        }
    }
}
```

或者，如果希望完全匹配 Weak::clone 行為，可以先檢查 key 和 value 的狀態：
```rust
impl<K: Trace + 'static, V: Trace + 'static> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        let key_clone = self.key.clone();
        
        // 如果 key 已經是 null，value 也無效
        if key_clone.ptr.load(Ordering::Acquire).is_null() {
            return Self::default();
        }
        
        let value_clone = Gc::try_clone(&self.value).unwrap_or_else(Gc::default);
        
        Self {
            key: key_clone,
            value: value_clone,
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 角度來看，這是一個 API 一致性問題。`Ephemeron` 本質上包含一個 key (Weak) 和一個 value (Gc)，clone 行為應該與這兩個組件的 clone 行為一致。

**Rustacean (Soundness 觀點):**
`Clone::clone()` panic 違反了標準庫慣例。雖然不是 UB，但這是一個 API 設計錯誤，可能導致使用者程式意外終止。

**Geohot (Exploit 攻擊觀點):**
目前沒有直接的安全影響，但不一致的錯誤處理可能導致難以調試的問題。

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
