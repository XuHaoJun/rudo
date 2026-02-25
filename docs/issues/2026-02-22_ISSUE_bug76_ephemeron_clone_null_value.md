# [Bug]: Ephemeron::clone() creates null value Gc when original value is dead/dropping

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 Ephemeron 的 value Gc 死亡或正在 dropping 時觸發 |
| **Severity (嚴重程度)** | Medium | 導致不一致的 API 行為，可能造成程式邏輯錯誤 |
| **Reproducibility (復現難度)** | Low | 容易重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Ephemeron::clone()` (ptr.rs:2076-2085)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當克隆一個 `Ephemeron<K, V>` 時：
- key (Weak<K>) 應該被克隆
- value (Gc<V>) 應該被克隆

如果原始的 value Gc 是有效的，克隆後的 Ephemeron 應該也有一個有效的 value Gc。

### 實際行為 (Actual Behavior)

在 `ptr.rs:2080-2083`：

```rust
value: Gc::try_clone(&self.value).unwrap_or_else(|| Gc {
    ptr: AtomicNullable::null(),
    _marker: PhantomData,
}),
```

當 `Gc::try_clone(&self.value)` 失敗時（因為 value 是 dead 或 in dropping state），代碼會創建一個 NULL Gc。這導致：

1. 原始 Ephemeron 有一個有效的 value Gc
2. 克隆後的 Ephemeron 有一個 NULL value Gc

這與 Weak::clone 的行為不一致：
- Weak::clone 只是簡單地複製指標，不檢查對象的存活狀態
- 但 Ephemeron 的 value 是強引用 (Gc)，克隆時應該保持一致性

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在 `ptr.rs:2076-2085` 的 `Clone` 實現：

```rust
impl<K: Trace + 'static, V: Trace + 'static> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),  // Weak 克隆 - 簡單复制指針
            value: Gc::try_clone(&self.value).unwrap_or_else(|| Gc {
                // BUG: 當 try_clone 失敗時，創建 NULL Gc
                ptr: AtomicNullable::null(),
                _marker: PhantomData,
            }),
        }
    }
}
```

問題分析：
1. `key` 是 Weak<K>，克隆行爲：簡單复制 Weak 指針
2. `value` 是 Gc<V>，克隆行爲：調用 try_clone，如果失敗則創建 NULL Gc

這導致不一致的行爲：當原始 value Gc 死亡時，克隆會產生一個 NULL value。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Ephemeron, Trace, collect_full};

#[derive(Trace)]
struct Key {
    value: i32,
}

#[derive(Trace)]
struct Value {
    data: String,
}

fn main() {
    let key = Gc::new(Key { value: 42 });
    let value = Gc::new(Value { data: "hello".to_string() });
    
    let ephemeron = Ephemeron::new(&key, value);
    
    // Drop the value Gc
    drop(value);
    
    // Trigger GC to clean up
    collect_full();
    
    // Now try to clone the ephemeron
    let cloned = ephemeron.clone();
    
    // The cloned ephemeron has a NULL value!
    // This is inconsistent behavior
    println!("Original upgrade: {:?}", ephemeron.upgrade());
    println!("Cloned upgrade: {:?}", cloned.upgrade());
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩種修復方案：

### 方案 1：使用 Gc::clone 行爲（推薦）

使用 `Gc::clone` 而不是 `Gc::try_clone`，因爲克隆行爲應該與原始终止一致：

```rust
impl<K: Trace + 'static, V: Trace + 'static> Clone for Ephemeron<K, V> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            value: Gc::clone(&self.value),  // 使用 clone 而不是 try_clone
        }
    }
}
```

這保證：
- 如果原始 value Gc 有效，克隆也有效
- 如果原始 value Gc 無效，克隆也會 panic（與 Gc::clone 一致）

### 方案 2：文檔化並保持當前行爲

如果這是預期行爲，需要在文檔中說明：

> 當克隆一個 Ephemeron 時，如果 value Gc 已經死亡或正在 dropping，克隆將包含一個 NULL value Gc。這允許克隆 "跟隨" 原始對象的生命周期。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Ephemeron 的語義是 "只有當 key 可達時，value 才可達"。克隆行爲應該與原始终止一致。如果原始 value Gc 有效，克隆應該也有效。這與 Weak::clone 的行爲類似 - 簡單复制指針，不檢查存活狀態。

**Rustacean (Soundness 觀點):**
NULL Gc 可能導致程式邏輯錯誤。當使用者克隆一個 Ephemeron 並嘗試使用其 value 時，可能會遇到意外的 NULL 引用，導致困惑或 panic。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個不一致性來觸發意外的程式行爲。當克隆產生 NULL value 時，後續對 value 的操作可能會導致 panic 或其他非預期行爲。

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Applied Option 1 (recommended): replaced `Gc::try_clone(...).unwrap_or_else(|| Gc { ptr: null, ... })` with `Gc::clone(&self.value)` in `Ephemeron::clone()`. Cloning now matches `Gc::clone` semantics: valid value → clone succeeds; dead/dropping value → panic (consistent). All 24 ephemeron tests pass.
