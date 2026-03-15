# [Bug]: GcCell/GcThreadSafeCell borrow_mut 錯誤地在僅有 generational barrier 時標記新指標為黑色

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 每次使用 GcCell::borrow_mut() 在 generational barrier 活躍時都會觸發 |
| **Severity (嚴重程度)** | High | 導致年輕物件無法被minor GC收集，記憶體洩漏 |
| **Reproducibility (復現難度)** | Medium | 需要minor GC + GcCell mutation |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** GcCell, GcThreadSafeCell
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

在 `GcCell::borrow_mut()` 和 `GcThreadSafeCell::borrow_mut()` 中，當 `barrier_active`（= `generational_active || incremental_active`）為 true 時，程式碼會將新的 GC 指標標記為黑色。然而，這是錯誤的行為：

### 預期行為
- **Generational barrier only**: 只需將頁面標記為髒頁（dirty page），使年輕物件在minor GC時被視為根。不應標記為黑色。
- **Incremental marking**: 需要將新指標標記為黑色（Dijkstra insertion barrier），防止新可達物件被遺漏。

### 實際行為
當僅有 generational barrier 活躍時（新指標被錯誤地標記為黑色），導致年輕物件無法在minor GC時被收集。這違背了generational GC的核心目的 - 頻繁收集年輕物件。

### 程式碼位置
- `crates/rudo-gc/src/cell.rs:193-208` - GcCell::borrow_mut()
- `crates/rudo-gc/src/cell.rs:1088-1101` - GcThreadSafeCell::borrow_mut()

---

## 🔬 根本原因分析 (Root Cause Analysis)

```rust
// cell.rs:193-208 (GcCell::borrow_mut)
let barrier_active = generational_active || incremental_active;
if barrier_active {
    // 錯誤：當 generational_active=true 時也會執行
    for gc_ptr in new_gc_ptrs {
        let _ = crate::gc::incremental::mark_object_black(...);
    }
}
```

問題在於 `barrier_active = generational_active || incremental_active` 這行。當僅有 `generational_active` 為 true 時（minor GC期間），這段程式碼仍然會將新指標標記為黑色。

`mark_object_black()` 會在物件的標記Bitmap中設置標記，防止該物件在當前GC週期被收集。但對於generational barrier，我們只希望將頁面標記為髒，而非阻止收集。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect};

#[derive(Trace)]
struct Node {
    value: GcCell<Option<Gc<Node>>>,
}

fn main() {
    // 建立循環引用: a -> b -> a
    let a = Gc::new(Node { value: GcCell::new(None) });
    let b = Gc::new(Node { value: GcCell::new(None) });
    
    // 通過 GcCell 設置引用，觸發generational barrier
    *a.value.borrow_mut() = Some(Gc::clone(&b));
    *b.value.borrow_mut() = Some(Gc::clone(&a));
    
    // Drop strong references
    drop(a);
    drop(b);
    
    // Minor GC - 由於bug，年輕物件可能被錯誤標記為黑色無法收集
    collect();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `GcCell::borrow_mut()` 和 `GcThreadSafeCell::borrow_mut()` 中的條件從：
```rust
if barrier_active {
    // mark_object_black
}
```

改為：
```rust
if incremental_active {  // 僅在incremental marking時標記為黑色
    // mark_object_black
}
```

generational barrier 只需要 `gc_cell_validate_and_barrier`（在第189行已經調用），它會將頁面標記為髒。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
generational GC的核心原則是將物件按年齡分代，年輕物件被頻繁收集。當OLD物件指向YOUNG物件時，需將該頁面標記為髒，使minor GC能掃描這些年輕物件作為根。但標記為「黑色」（不可收集）會阻止這個過程。髒頁機制已經足夠，不需要額外標記。

**Rustacean (Soundness 觀點):**
這不是UB，但會導致記憶體洩漏 - 年輕物件應該被收集但沒有。程式碼邏輯錯誤：混合了generational barrier和incremental barrier的職責。

**Geohot (Exploit 觀點):**
攻擊者可能透過大量創建此類引用來觸發記憶體洩漏，導致記憶體膨脹最終耗盡系統資源。

---

## Resolution (2026-03-15)

**Outcome:** Already fixed.

The fix was applied via bug301. The current implementation in `cell.rs` correctly uses `if incremental_active` (not `barrier_active`) for the `mark_object_black` block:

- **GcCell::borrow_mut()** (lines 192–207): `if incremental_active { ... mark_object_black(...) }`
- **GcThreadSafeCell::borrow_mut()** (lines 1087–1099): `if incremental_active { ... mark_object_black(...) }`

Comments in code: "FIX bug301: mark_object_black should only be called during incremental marking, not generational barrier."

Generational barrier handling remains in `gc_cell_validate_and_barrier()`, which marks pages dirty without marking objects black.
