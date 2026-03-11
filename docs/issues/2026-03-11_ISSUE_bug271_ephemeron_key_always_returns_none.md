# [Bug]: Ephemeron::key() Always Returns None - Missing Key Accessor Implementation

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Ephemeron::key() 永遠返回 None，無法獲取 key |
| **Severity (嚴重程度)** | Medium | API 不完整，無法使用 ephemeron 的 key |
| **Reproducibility (重現難度)** | Very Low | 每次都會發生，直接看代碼即可確認 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Ephemeron::key()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Ephemeron::key()` 應該返回 `Option<&Gc<K>>`，其中：
- `Some(&Gc<K>)` 如果 key 仍然 alive
- `None` 如果 key 已被回收

### 實際行為 (Actual Behavior)
`Ephemeron::key()` 目前永遠返回 `None`，無論 key 是否活著。

```rust
// ptr.rs:2399-2406
pub const fn key(&self) -> Option<&Gc<K>> {
    // This is tricky - Weak doesn't give us &Gc<K>, it gives us Option<Gc<K>>
    // For now, we don't expose direct key access
    None
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`Ephemeron` 結構體內部存儲的是 `Weak<K>`：

```rust
// ptr.rs:2372-2377
pub struct Ephemeron<K: Trace + 'static, V: Trace + 'static> {
    /// Weak reference to key - does NOT keep key alive
    key: Weak<K>,
    /// Strong reference to value - keeps value alive IF key is alive
    value:Gc<V>,
}
```

開發者在註釋中說 "Weak doesn't give us &Gc<K>, it gives us Option<Gc<K>>"，但這不是正確的理解。

`Weak<K>` 確實沒有直接實現 `Deref` 到 `Gc<K>`，但可以通過 `upgrade()` 獲取 `Option<Gc<K>>`，然後可以通過某種方式返回引用。

**解決方案**：應該存儲 key 的原始指針，並在升級後返回引用。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, Ephemeron};

#[derive(Trace)]
struct KeyData { value: i32 }
#[derive(Trace)]
struct ValueData { data: String }

fn main() {
    let key = Gc::new(KeyData { value: 42 });
    let value = Gc::new(ValueData { data: "hello".to_string() });
    
    let ephemeron = Ephemeron::new(&key, value);
    
    // 嘗試獲取 key - 永遠返回 None！
    let key_ref = ephemeron.key();
    println!("Key: {:?}", key_ref);  // 輸出: None
    
    // 預期應該返回 Some(&key)
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 修改 `Ephemeron` 結構體，存儲 key 的原始指針：
```rust
pub struct Ephemeron<K: Trace + 'static, V: Trace + 'static> {
    key_ptr: NonNull<GcBox<K>>,  // 新增：存儲 key 的指針
    key: Weak<K>,
    value: Gc<V>,
}
```

2. 實現 `key()` 方法：
```rust
pub fn key(&self) -> Option<&Gc<K>> {
    self.key.upgrade().map(|gc| {
        // 從升級後的 Gc 獲取引用
        // 需要某種方式從 internal_ptr 返回引用
        unsafe { &*Gc::internal_ptr(&gc) }
    })
}
```

或者更簡單地，直接讓 `Weak<K>` 支持返回引用（如果可能的話）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Ephemeron 的核心語義是「當 key 活著時，value 才能訪問」。目前無法獲取 key 引用限制了 ephemeron 的實用性，因爲用戶無法檢查 key 的狀態而無需升級。

**Rustacean (Soundness 觀點):**
這不是 UB 或記憶體安全問題，而是 API 不完整。返回 `None` 會誤導用戶，讓他們以爲 key 已死亡。

**Geohot (Exploit 觀點):**
沒有安全影響，純粹是 API 問題。

---

## 🔗 相關 Issue

- 無直接相關的之前 issue
- 與 `Ephemeron` 設計相關
