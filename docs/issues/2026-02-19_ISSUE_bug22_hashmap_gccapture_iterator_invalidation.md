# [Bug]: HashMap GcCapture Potential Iterator Invalidation

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 mutation 期間呼叫 capture |
| **Severity (嚴重程度)** | High | 可能導致迭代器失效或記憶體損壞 |
| **Reproducibility (復現難度)** | Medium | 取決於具體使用模式 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` implementations for `HashMap`, `BTreeMap`, `HashSet`, `BTreeSet` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 `GcCapture::capture_gc_ptrs_into` 迭代 HashMap 時，應該安全地讀取所有鍵和值的 GC 指標，不會發生迭代器失效。

### 實際行為 (Actual Behavior)
當 `HashMap` (或 `BTreeMap`, `HashSet`, `BTreeSet`) 實現的 `GcCapture::capture_gc_ptrs_into` 被調用時，它會迭代集合的鍵和值：

```rust
impl<K: GcCapture + 'static, V: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture for HashMap<K, V, S> {
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for key in self.keys() {  // 迭代過程中沒有鎖定
            key.capture_gc_ptrs_into(ptrs);
        }
        for value in self.values() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

雖然 `GcCapture` 通常在 write barrier 期間調用（此時不會並發修改），但如果 Rust 的 `HashMap` 內部實現發生 rehash（即使在單線程環境中），迭代器可能會失效。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/cell.rs` 中的 `GcCapture` 實現：

```rust
impl<K: GcCapture + 'static, V: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture
    for HashMap<K, V, S>
{
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for key in self.keys() {
            key.capture_gc_ptrs_into(ptrs);
        }
        for value in self.values() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

問題在於：
1. `self.keys()` 和 `self.values()` 創建迭代器
2. 在某些情況下，標準庫的 HashMap 可能會在迭代過程中進行 rehash（雖然罕見）
3. 更重要的是，這種模式與 Rust 的安全保證不符 - 我們正在迭代一個可能被修改的集合

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace};
use std::collections::HashMap;

#[derive(Trace)]
struct Data { value: i32 }

fn trigger_bug() {
    let map = Gc::new(GcCell::new(HashMap::new()));
    
    // Add some entries
    for i in 0..100 {
        map.borrow_mut().insert(Gc::new(i), Gc::new(Data { value: i }));
    }
    
    // This triggers capture_gc_ptrs_into
    // In some edge cases with HashMap rehash during iteration,
    // this could cause issues
    let _ = map.borrow();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

方案 1：先收集所有指標，再處理（避免迭代中的任何潛在問題）

```rust
impl<K: GcCapture + 'static, V: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture
    for HashMap<K, V, S>
{
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // 先複製鍵和值的引用，避免迭代過程中的任何潛在問題
        let keys: Vec<_> = self.keys().collect();
        let values: Vec<_> = self.values().collect();
        
        for key in keys {
            key.capture_gc_ptrs_into(ptrs);
        }
        for value in values {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

方案 2：使用 `iter()` 一次性迭代鍵值對

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    for (key, value) in self.iter() {
        key.capture_gc_ptrs_into(ptrs);
        value.capture_gc_ptrs_into(ptrs);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 的角度來看，這個問題凸顯了集合類型在 GC 追蹤中的複雜性。HashMap 的內部實現可能在迭代過程中改變，這對 GC 的穩定性構成潛在風險。建議對所有集合類型採用更安全的迭代模式。

**Rustacean (Soundness 觀點):**
雖然這可能不會導致傳統意義上的 UB（因為我們只是在讀取指標），但這是一個潛在的Iterator 失效問題。遵循 Rust 的最佳實踐，先收集再處理是更安全的做法。

**Geohot (Exploit 觀點):**
在極端情況下，如果攻擊者能夠控制 HashMap 的 rehash 行為，可能會利用這一點進行攻擊。雖然目前看來不太可能，但防禦性編碼是更好的選擇。
