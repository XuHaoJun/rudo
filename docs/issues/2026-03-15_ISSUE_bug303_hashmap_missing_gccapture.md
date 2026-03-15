# [Bug]: HashMap<K, V, S> Missing GcCapture Implementation

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | HashMap 是 Rust 標準庫常用容器，開發者會自然嘗試與 Gc 搭配使用 |
| **Severity (嚴重程度)** | High | 缺少 GcCapture 導致無法在 GcCell 中使用 HashMap，且可能導致 incremental marking 時遺漏物件 |
| **Reproducibility (復現難度)** | Very High | 編譯期錯誤，易於驗證 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` trait implementation in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`HashMap<K, V, S>` 在 `trace.rs` 中實現了 `Trace` trait，但 `cell.rs` 中缺少對應的 `GcCapture` 實現。這導致無法在 `GcCell` 中使用包含 GC 指標的 HashMap。

### 預期行為 (Expected Behavior)
開發者應該能夠將 `HashMap<Gc<T>, V>` 或 `HashMap<K, Gc<V>>` 存儲在 `GcCell` 中並調用 `borrow_mut()`，就像使用 `Vec<Gc<T>>` 或 `BTreeMap<K, Gc<V>>` 一樣。

### 實際行為 (Actual Behavior)
編譯錯誤：
```
error[E0277]: the trait bound `HashMap<K, V, S>: GcCapture` is not satisfied
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs` 中：
- `BTreeMap<K, V>` 有 `GcCapture` 實現 (line 543)
- `HashSet<T, S>` 有 `GcCapture` 實現 (line 560)
- 但 `HashMap<K, V, S>` 缺少 `GcCapture` 實現

在 `trace.rs` 中：
- `HashMap<K, V, S>` 有 `Trace` 實現 (line 459)

這是不一致的，導致類型系統層面的錯誤。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, GcCapture};
use std::collections::HashMap;

fn main() {
    let cell = GcCell::new(HashMap::new());
    cell.borrow_mut().insert(Gc::new(1), Gc::new("value"));
}
```

編譯錯誤：
```
error[E0277]: the trait bound `std::collections::HashMap<K, V, S>: GcCapture` is not satisfied
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs` 中添加：

```rust
impl<K: GcCapture + 'static, V: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture for std::collections::HashMap<K, V, S> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for (k, v) in self {
            k.capture_gc_ptrs_into(ptrs);
            v.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
缺少 `GcCapture` 會破壞 incremental marking 的 SATB barrier。當 `GcCell::borrow_mut()` 被調用時，barrier 需要遍歷 HashMap 中的所有 GC 指標以將其標記為黑色。沒有 `GcCapture`，HashMap 內的 GC 指標會被遺漏，導致物件在 incremental GC 期間被錯誤回收。

**Rustacean (Soundness 觀點):**
這是一個類型系統層面的不一致性。`Trace` 已經為 HashMap 實現，暗示它可以與 GC 一起使用，但 `GcCapture` 的缺失導致無法在 `GcCell` 中使用。這違反了 Rust 的 "如果它編譯，它應該能正常工作" 原則。

**Geohot (Exploit 觀點):**
雖然不是傳統意義上的安全漏洞，但這會迫開發者使用較不安全的替代方案（如手動管理的 Vec），這些方案更容易引入 race conditions 或其他錯誤。
