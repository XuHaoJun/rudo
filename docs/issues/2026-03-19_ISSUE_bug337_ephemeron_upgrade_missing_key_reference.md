# [Bug]: Ephemeron upgrade 缺少 key_gc 引用保持，導致 Value 可能被錯誤回收

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要Value依賴Key才能存活的特定使用場景 |
| **Severity (嚴重程度)** | High | 可能導致Value被錯誤回收，造成use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要循環引用場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs` - `Ephemeron::upgrade()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`Ephemeron::upgrade()` 應該在升級Key後保持強引用，確保在克隆Value期間Key保持存活。

### 實際行為
在 `Ephemeron::upgrade()` (ptr.rs:2838-2849) 中：
```rust
pub fn upgrade(&self) -> Option<Gc<V>> {
    if let Some(key_gc) = self.key.upgrade() {
        // Key is alive and we hold a ref. Now safely clone the value.
        Gc::try_clone(&self.value)
    } else {
        None
    }
}
```

問題在於 `key_gc` 雖然被創建但從未被使用來保持Key存活。當 `try_clone` 完成後，`key_gc` 立即離開作用域，如果Value依賴Key才能存活（例如Value內部包含對Key的 Weak 引用），Value可能已被錯誤回收。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼

```rust
// ptr.rs:2838-2849
pub fn upgrade(&self) -> Option<Gc<V>> {
    if let Some(key_gc) = self.key.upgrade() {
        // key_gc 只用於檢查key是否存活，但從未用於保持key存活
        // 當 try_clone 返回後，key_gc 立即被丟棄
        Gc::try_clone(&self.value)
    } else {
        None
    }
}
```

### 邏輯缺陷

1. `self.key.upgrade()` 創建一個強引用 `key_gc`
2. `key_gc` 只用於維持Key存活
3. `Gc::try_clone(&self.value)` 調用期間，`key_gc` 仍在作用域內
4. **但是**，`try_clone` 完成後 `key_gc` 立即被丟棄
5. 如果Value依賴Key才能存活（例如Value內部有 Weak<Key>），Key可能被回收
6. 這可能導致Value被錯誤回收

### 具體場景

```rust
#[derive(Trace)]
struct Node {
    // Value依賴Key才能存活
    weak_ref: Weak<Node>,  
}

let key = Gc::new(Node { weak_ref: Weak::new() });
let value = Gc::new(Node { weak_ref: Gc::downgrade(&key) });

let ephemeron = Ephemeron::new(&key, value);

// 調用 upgrade:
// 1. key_gc = key.upgrade() - Key被引用
// 2. value.try_clone() - 克隆Value
// 3. key_gc 被丟棄 - Key可能因此被回收
// 4. 如果Value只被Key引用，Value也被回收
// 5. 返回的克隆Value可能指向已被回收的內存！
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `Ephemeron::upgrade()` 確保 `key_gc` 在Value克隆期間和返回後都保持活躍：

```rust
pub fn upgrade(&self) -> Option<Gc<V>> {
    if let Some(key_gc) = self.key.upgrade() {
        // 先克隆Value，保持引用
        let value_clone = Gc::try_clone(&self.value)?;
        
        // key_gc 在此處仍然保持Key存活
        // 當函數返回後，key_gc 被丟棄，但Value克隆已經完成
        Some(value_clone)
    } else {
        None
    }
}
```

或者使用 `std::mem::forget` 延遲丟棄：

```rust
pub fn upgrade(&self) -> Option<Gc<V>> {
    if let Some(key_gc) = self.key.upgrade() {
        let result = Gc::try_clone(&self.value);
        // 保持 key_gc 存活直到Value克隆完成
        std::mem::forget(key_gc);
        result
    } else {
        None
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Ephemeron語義要求Value只在Key存活時才能訪問。當前實現存在TOCTOU窗口：Key可能在Value克隆完成後、調用者使用Value前被回收。這與標準ephemeron實現不符。

**Rustacean (Soundness觀點):**
這可能導致use-after-free。如果Value依賴Key的存活狀態，在key_gc被丟棄後訪問返回的Value克隆可能觸發未定義行為。

**Geohot (Exploit攻擊觀點):**
理論上可利用此bug：
1. 創建循環引用的ephemeron
2. 調用upgrade()
3. 在key_gc丟棄後、Value使用前觸發GC
4. 可能導致use-after-free

但實際利用難度較高，需要精確控制GC時序。

---

## Resolution (2026-03-21)

**Outcome:** Invalid — misunderstanding of Rust drop order.

The current `Ephemeron::upgrade()` implementation is correct. In Rust, the `if let Some(key_gc) = self.key.upgrade()` binding keeps `key_gc` alive until the end of its enclosing block (the closing `}`). `Gc::try_clone(&self.value)` is called inside that block, so the key is held alive during the entire clone operation.

After `Gc::try_clone` succeeds, the returned `Option<Gc<V>>` holds an independent strong reference to the value (via `try_inc_ref_if_nonzero`). Even if `key_gc` is subsequently dropped (and the key eventually collected), the returned `Gc<V>` cannot be collected — it has its own strong ref.

The "具體場景" step 4 ("如果Value只被Key引用，Value也被回收") is incorrect: the returned clone already holds a strong ref to the value before `key_gc` is dropped. The suggested fix is functionally identical to the current code.

All 26 ephemeron tests pass with the current implementation (`cargo test -p rudo-gc --test ephemeron`).
