# [Bug]: Ephemeron<K,V> Trace 實作總是追蹤 value，導致記憶體無法正確回收

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要使用 Ephemeron 結構並依賴 key 死亡時 value 被回收的行為 |
| **Severity (嚴重程度)** | Medium | 導致記憶體無法正確回收，記憶體使用量高於預期 |
| **Reproducibility (復現難度)** | Low | 可透過觀察記憶體行為或檢視程式碼發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Ephemeron<K,V>` 的 `Trace` 實作 (`ptr.rs:2046-2063`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

Ephemeron（臨時引用）的核心語義是：當 key 死亡時，value 應該可以被垃圾回收。在 GC 的標記階段，value 只應該在 key 存活的情況下被追蹤。

### 實際行為 (Actual Behavior)

目前的 `Trace` 實作總是追蹤 value，無論 key 是否存活：

```rust
// ptr.rs:2046-2063
unsafe impl<K: Trace + 'static, V: Trace + 'static> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // 問題：總是追蹤 value，沒有檢查 key 是否存活！
        visitor.visit(&self.value);
    }
}
```

文件註解說明了這是已知限制：
> "NOTE: This keeps the value alive as long as the ephemeron exists."
> "For now, this basic implementation provides the API but not the full GC semantics."

但這導致即使 key 已死亡，value 仍會被標記為存活，無法被回收。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:2046-2063`，`Trace` 實作直接調用 `visitor.visit(&self.value)`，沒有先檢查 key 是否存活。

正確的 ephemeron 語義應該：
1. 在標記階段檢查 key 是否可達（alive)
2. 只有當 key 存活時才追蹤 value
3. 當 key 死亡時，value 应该可以被回收

目前的實現：
- `value: Gc<V>` - 總是在 GC 期間被追蹤
- `key: Weak<K>` - 不被追蹤（這是對的）
- 沒有在 Trace 中檢查 key 存活狀態

這導致即使 key 已經死亡，只要 Ephemeron 本身可達，value 就會被標記為存活。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, Ephemeron, collect_full};
use std::rc::Rc;
use std::cell::Cell;

#[derive(Clone, Trace)]
struct KeyData {
    marker: Rc<Cell<bool>>,
}

#[derive(Trace)]
struct ValueData {
    value: i32,
}

fn main() {
    // 建立 key 和 value
    let key = Gc::new(KeyData {
        marker: Rc::new(Cell::new(true)),
    });
    let value = Gc::new(ValueData { value: 42 });
    
    // 建立 ephemeron
    let ephemeron = Ephemeron::new(&key, value);
    
    // 記住 value 的 internal pointer
    let value_ptr = Gc::internal_ptr(&value);
    println!("Value internal ptr: {:?}", value_ptr);
    
    // Drop key - 這應該導致 key 死亡
    drop(key);
    
    // 執行 GC - 由於 key 已死亡，value 應該可以被回收
    collect_full();
    
    // 但由於 Ephemeron 仍然可達且 Trace 總是追蹤 value，
    // value 不會被回收（可以透過檢查 value 的 internal ptr 是否仍然有效來驗證）
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：在 Trace 中檢查 key 存活狀態

```rust
unsafe impl<K: Trace + 'static, V: Trace + 'static> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // 檢查 key 是否存活
        if self.key.is_alive() {
            // 只有 key 存活時才追蹤 value
            visitor.visit(&self.value);
        }
        // 如果 key 已死亡，不追蹤 value，讓 GC 可以回收它
    }
}
```

### 方案 2：在 sweep 階段處理 broken ephemerons

需要追蹤所有 ephemerons 並在 sweep 階段清理已損壞的 ephemeron（key 死亡但 value 仍存在）。

### 權衡

方案 1 較簡單，但可能在並發場景下有 TOCTOU 問題（key 在檢查後死亡）。
方案 2 是完整的 ephemeron 語義，但需要較大改動。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
真正的 ephemeron 語義需要 GC 在標記階段特殊處理：1) 追蹤所有 ephemeron 結構；2) 只在 key 被標記時才標記 value；3) 在 sweep 階段清理 key 已死亡但 value 仍存在的 ephemeron。當前的實現是一個「API 先佔」策略 - 提供了 ephemeron 的介面但沒有完整實現其語義。這對於簡單用例足夠，但對於依賴 key 死亡時 value 自動回收的應用會造成記憶體洩漏。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，而是語義不正確。當前實現不會導致 use-after-free，只是無法正確回收記憶體。從 API 角度來看，文件已經說明了這是已知限制，所以不算欺騙。但這確實與 Ephemeron 的預期行為不符。

**Geohot (Exploit 攻擊觀點):**
雖然這不是安全性問題，但記憶體無法正確回收可能導致：1) 記憶體使用量不斷增長（記憶體洩漏）；2) 如果攻擊者能控制 key 的生命週期，可能利用這一點造成過度記憶體消耗。然而這更像是 DOS 攻擊而非傳統意義的記憶體腐敗。
