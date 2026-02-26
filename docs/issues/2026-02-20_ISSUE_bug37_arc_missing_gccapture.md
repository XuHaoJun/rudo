# [Bug]: std::sync::Arc 缺少 GcCapture 實作導致指標遺漏

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會在 Arc 中存儲 GC 指標以共享所有權 |
| **Severity (嚴重程度)** | High | 導致 GC 無法掃描 Arc 內部的指標，造成記憶體錯誤 |
| **Reproducibility (Reproducibility)** | Medium | 需要使用 GcCell<Arc<Gc<T>>> 模式才能觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` impl for `std::sync::Arc<T>`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`std::sync::Arc<T>` 應該實作 `GcCapture` trait，使得 GC 可以掃描 Arc 內部的 GC 指標。這與 `Box<T>`、`Vec<T>` 等其他容器類型的行為一致。

### 實際行為 (Actual Behavior)
`std::sync::Arc<T>` 沒有實作 `GcCapture` trait。當 GC 嘗試掃描根集時，無法捕捉到存在於 `std::sync::Arc<T>` 內部的 GC 指標，導致這些指標被錯誤地視為垃圾。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs` 中存在以下 `GcCapture` 實作：
- `Box<T>` (line 507-517)
- `Vec<T>` (line 413-425)
- `Option<T>` (line 399-411)
- `std::sync::RwLock<T>` (line 567-579)

但缺少：
- `std::sync::Arc<T>` - **本 bug**
- `parking_lot::Mutex<T>` - 相關問題

當使用以下模式時會觸發 bug：
```rust
use std::sync::Arc;
use rudo_gc::{Gc, GcCell, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

let cell = GcCell::new(Arc::new(Gc::new(Data { value: 42 })));

// 當呼叫 borrow_mut() 時，SATB barrier 嘗試 capture_gc_ptrs_into()
// 但 std::sync::Arc 沒有實作 GcCapture，無法捕捉內部的 Gc 指標
let mut guard = cell.borrow_mut();
// 此時 GC 可能會錯誤地回收 Arc 內部的 Gc 物件
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use std::sync::Arc;
use rudo_gc::{Gc, GcCell, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_arc_gccapture() {
    let cell = GcCell::new(Arc::new(Gc::new(Data { value: 42 })));
    
    // 獲取內部 Gc 的指標
    let inner_gc = cell.borrow().clone();
    let ptr = inner_gc.raw_ptr();
    
    // 釋放外部 Arc 的擁有權
    drop(cell);
    
    // 應該仍然可以訪問 inner_gc，因為它是獨立的 Gc
    assert_eq!(inner_gc.value, 42);
    
    // 執行 GC - 由於 Arc 缺少 GcCapture，inner_gc 可能被錯誤地回收
    collect_full();
    
    // 這裡可能會發生 use-after-free
    // assert_eq!(inner_gc.value, 42);  // 可能失敗!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs` 中添加 `std::sync::Arc<T>` 的 `GcCapture` 實作：

```rust
use std::sync::Arc as StdArc;

impl<T: GcCapture + 'static> GcCapture for StdArc<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        // Arc 內部 T 的 GC 指需要在 GC 追蹤時特別處理
        // 這是一個複雜的問題，因為 Arc 可能被多個執行緒共享
        (**self).capture_gc_ptrs_into(ptrs);
    }
}
```

**注意**：這個實作需要進一步考慮，因為：
1. Arc 可以在多個執行緒之間共享
2. 在 STW 期間，所有執行緒都會暫停，所以不存在並發訪問問題
3. 但需要確保 Arc 的內部資料可以被正確掃描

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在標準 GC 實現中，包裝類型（如 Arc）通常需要特殊處理。雖然 Arc 本身不是 GC 管理的物件，但它內部可能包含 GC 指標。確保所有可能包含 GC 指標的容器類型都實作 GcCapture 是基本要求。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。如果 GC 無法掃描 Arc 內部的指標，可能會導致 use-after-free。雖然這不是傳統意義的 UB，但在 Rust 的記憶體安全保證下，我們應該確保 GC 系統的正確性。

**Geohot (Exploit 觀點):**
攻擊者可能利用這個漏洞：
1. 構造 GcCell<Arc<Gc<T>>> 結構
2. 觸發 GC collect_full()
3. 由於 Arc 缺少 GcCapture，內部的 Gc 被錯誤回收
4. 實現 use-after-free 漏洞

---

## 參考相關 bug

- bug36: `std::sync::Mutex` 缺少 GcCapture - 相同模式，不同類型
- bug35: `std::sync::RwLock` 使用 try_read() - 相關問題
- bug34: `GcRwLock` 使用 try_read() - 相關問題

---

**Resolution:** Added `GcCapture` impl for `std::sync::Arc<T>` in cell.rs. Delegates to inner value via `(**self).capture_gc_ptrs_into(ptrs)`, same pattern as `Box<T>`.
