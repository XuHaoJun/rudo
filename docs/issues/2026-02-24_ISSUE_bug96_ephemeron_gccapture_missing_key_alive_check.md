# [Bug]: Ephemeron GcCapture 實現不一致 - 未檢查 key 是否存活

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 GcCell 內使用 Ephemeron 並依賴 key 死亡時 value 被回收 |
| **Severity (嚴重程度)** | Medium | 導致 SATB barrier 期間不正確地保留 value記憶體，與 Trace 語義不一致 |
| **Reproducibility (復現難度)** | Low | 可透過檢視程式碼發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Ephemeron<K,V>` 的 `GcCapture` 實作 (`ptr.rs:2151-2166`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

`Ephemeron` 的核心語義是：當 key 死亡時，value 應該可以被垃圾回收。因此：

1. **`Trace` 實現**：正確地在 key 存活時才追蹤 value
2. **`GcCapture` 實現**：應該與 `Trace` 一致，在 key 存活時才捕獲 value

### 實際行為

**`Trace` 實現（正確）**：
```rust
// ptr.rs:2116-2125
unsafe impl<K: Trace + 'static, V: Trace + 'static> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // 正確：檢查 key 是否存活
        if self.is_key_alive() {
            visitor.visit(&self.value);
        }
    }
}
```

**`GcCapture` 實現（錯誤）**：
```rust
// ptr.rs:2151-2166
impl<K: Trace + 'static, V: Trace + 'static> GcCapture for Ephemeron<K, V> {
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // BUG: 總是捕獲 value，沒有檢查 key 是否存活！
        self.value.capture_gc_ptrs_into(ptrs);
        
        // 正確：只捕獲存活的 key
        if let Some(key_gc) = self.key.try_upgrade() {
            key_gc.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

`GcCapture` 總是捕獲 value，導致即使 key 已死亡，value 仍被視為 GC root，無法被回收。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:2158-2160`，`GcCapture` 實現直接調用 `self.value.capture_gc_ptrs_into(ptrs)`，沒有先檢查 key 是否存活。

這與 `Trace` 實現不一致：
- `Trace::trace` 正確檢查 `is_key_alive()`
- `GcCapture::capture_gc_ptrs_into` 缺少此檢查

**影響**：
1. 在 SATB barrier 期間，不正確地保留 value 記憶體
2. 與 Ephemeron 的預期語義不一致
3. 導致即使 key 死亡，value 也無法被回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, Ephemeron, collect_full};
use std::cell::RefCell;
use std::rc::Rc;
use std::cell::Cell;

#[derive(Clone, Trace)]
struct KeyData {
    marker: Rc<Cell<bool>>,
}

#[derive(Trace)]
struct ValueData {
    value: RefCell<i32>,
}

#[derive(Trace)]
struct Container {
    // Ephemeron stored inside GcCell - uses GcCapture
    ephemeron: RefCell<Ephemeron<KeyData, ValueData>>,
}

fn main() {
    let key = Gc::new(KeyData {
        marker: Rc::new(Cell::new(true)),
    });
    let value = Gc::new(ValueData { value: RefCell::new(42) });
    
    let ephemeron = Ephemeron::new(&key, value);
    let container = Gc::new(Container {
        ephemeron: RefCell::new(ephemeron),
    });
    
    // 獲取 value 的內部指標
    let value_ptr = Gc::internal_ptr(&value);
    println!("Value internal ptr: {:?}", value_ptr);
    
    // Drop key
    drop(key);
    
    // 使用 GcCell 的 borrow_mut 來觸發 GcCapture
    {
        let mut borrow = container.ephemeron.borrow_mut();
        // 這會調用 GcCapture::capture_gc_ptrs_into
        // 由於 GcCapture 不檢查 key 是否存活，value 會被錯誤地捕獲
    }
    
    // 執行 GC - 由於 key 已死亡且 GcCapture 錯誤地捕獲了 value，
    // value 不會被回收（與 Trace 語義不一致）
    collect_full();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcCapture::capture_gc_ptrs_into` 中添加 key 存活檢查：

```rust
impl<K: Trace + 'static, V: Trace + 'static> GcCapture for Ephemeron<K, V> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // 修復：先檢查 key 是否存活
        if self.is_key_alive() {
            // 只有 key 存活時才捕獲 value
            self.value.capture_gc_ptrs_into(ptrs);
            
            // 捕獲 key（如果仍然存活）
            if let Some(key_gc) = self.key.try_upgrade() {
                key_gc.capture_gc_ptrs_into(ptrs);
            }
        }
        // 如果 key 已死亡，不捕獲任何指標，讓 GC 可以回收
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Ephemeron 的核心語義是 key 死亡時 value 應該可以被回收
- `GcCapture` 用於 SATB barrier，應與 `Trace` 語義一致
- 當 key 死亡時捕獲 value 會導致 SATB 期間不正確地保留記憶體

**Rustacean (Soundness 觀點):**
- 這不是 soundness 問題，而是 API 語義不一致
- `Trace` 正確檢查 key 存活狀態，但 `GcCapture` 不檢查
- 導致使用 `GcCell<Ephemeron>` 時行為與預期不符

**Geohot (Exploit 攻擊觀點):**
- 攻擊者可能利用此不一致性讓 value 記憶體無法釋放
- 導致記憶體消耗增加，是一種潜在的 DOS 攻擊向量

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Wrapped `GcCapture::capture_gc_ptrs_into` for `Ephemeron<K,V>` in `if self.is_key_alive()` — only captures value and key when key is alive. When key is dead, nothing is captured, allowing GC to collect the value. Aligns with `Trace` semantics.
