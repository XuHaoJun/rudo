# [Bug]: RefCell GcCapture 使用 try_borrow() 導致靜默失敗 - GC 指標可能遺漏

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者使用 RefCell 進行 interior mutability，且可能在 GC 期間持有可變借用 |
| **Severity (嚴重程度)** | Critical | 導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | Medium | 需要在 GC 期間保持 RefCell 的可變借用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture for RefCell<T>` in `cell.rs:659-671`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcCapture` 實作應該捕獲所有包含的 GC 指標，確保在 GC 追蹤期間所有可達物件都被正確標記。

### 實際行為 (Actual Behavior)
`RefCell<T>` 的 `GcCapture` 實作使用 `try_borrow()` 來獲取內部值。當 RefCell 已經被可變借用 (`RefMut`) 時，`try_borrow()` 會返回 `Err`，導致函數靜默失敗，不捕獲任何 GC 指標。

這與之前 bug86 的修復有關：
- **bug86**: `borrow()` 在可變借用存在時會 panic → 修復改用 `try_borrow()`
- **新 bug**: `try_borrow()` 失敗時靜默跳過，導致 GC 指標遺漏

### 程式碼位置 (`cell.rs:659-671`)
```rust
impl<T: GcCapture + 'static> GcCapture for RefCell<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Ok(value) = self.try_borrow() {
            value.capture_gc_ptrs_into(ptrs);
        }
        // 當 try_borrow() 失敗時，靜默跳過，不捕獲任何指標
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **bug86 修復的副作用**: 之前使用 `borrow()` 會在無法獲取借用時 panic，這被視為一個問題。修復改用 `try_borrow()` 避免 panic，但引入新的靜默失敗問題。

2. **違反 GcCapture 契約**: `GcCapture` trait 的契約是「捕獲所有 GC 指標」。當 `try_borrow()` 失敗時，實現沒有履行這個契約。

3. **與 RwLock 的不一致性**: 比較 `cell.rs:680-684` 中的 `RwLock` 實現：
   ```rust
   // RwLock 使用 blocking read() 並檢查 Ok(guard)
   if let Ok(guard) = self.read() {
       guard.capture_gc_ptrs_into(ptrs);
   }
   ```
   兩者都使用 `try_*` 模式，但 RwLock 在這種情況下的影響較小（因為通常是讀取佔用較長時間）。

4. **GC 正確性問題**: 如果 GC 在 RefCell 被可變借用時運行， contained 的 GC 指標不會被捕獲，導致：
   - 指標指向的物件可能被錯誤回收
   - 產生 Use-After-Free
   - 記憶體安全問題

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要 test-util feature
use rudo_gc::{Gc, GcCell, Trace, collect_full, register_test_root};
use std::cell::RefCell;
use std::rc::Rc;
use std::cell::Cell;

#[derive(Clone, Trace)]
struct Inner {
    value: Rc<Cell<bool>>,
}

#[test]
fn test_refcell_gccapture_silent_failure() {
    // 建立外部參考追蹤
    let marker = Rc::new(Cell::new(false));
    
    // 建立包含 Gc 的 RefCell
    let cell = Gc::new(RefCell::new(Inner { value: marker.clone() }));
    register_test_root(&cell);
    
    // 初始 Rc count 應該是 1
    let initial_count = Rc::strong_count(&marker);
    assert_eq!(initial_count, 1, "Initial count should be 1");
    
    // 獲取可變借用（這會阻止 try_borrow 成功）
    let mut_borrow = cell.borrow_mut();
    
    // 嘗試 GC - 這應該觸發 capture_gc_ptrs_into
    // 但由於 try_borrow 失敗，Inner 中的 GC 指標不會被捕獲
    collect_full();
    
    // -drop mut_borrow
    drop(mut_borrow);
    
    // 問題：如果 bug 存在，marker 應該已被回收（count = 0）
    // 因為 Inner 沒有被視為 root
    let after_count = Rc::strong_count(&marker);
    
    // 如果 bug 修復，這應該是 1；否則是 0（UAF 發生）
    assert_eq!(after_count, 1, "BUG: Inner was incorrectly collected!");
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 選項 1: 使用 blocking borrow (推薦)
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let value = self.borrow(); // 會 panic 如果已有可變借用
    value.capture_gc_ptrs_into(ptrs);
}
```
優點：明確失敗，不是靜默失敗。與其他實現一致。
缺點：可能 panic（但這是明確的失敗模式）。

### 選項 2: 記錄並處理失敗
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Ok(value) = self.try_borrow() {
        value.capture_gc_ptrs_into(ptrs);
    } else {
        // 記錄警告或使用除錯設施
        // 考慮使用 panic! 而不是靜默失敗
    }
}
```

### 選項 3: 文檔化限制
如果選擇保持當前實現，需要在 `GcCapture` trait 文檔中明確說明：
- RefCell 在有活躍可變借用時可能無法捕獲指標
- 使用者應避免在 GC 期間持有 RefCell 的可變借用

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從傳統 GC 的角度來看，這是一個正確性問題。SATB (Snapshot-At-The-Beginning) barrier 必須捕獲所有可能被覆蓋的舊值。指標捕獲的靜默失敗違反了「在標記開始時可達的所有物件保持可達」的基本不變量。這可能導致記憶體回收錯誤，類似於傳統 GC 中的遺漏 root。

**Rustacean (Soundness 觀點):**
從 Rust 安全角度來看，這是一個令人擔憂的模式，因為它通過靜默失敗提供了**不安全的行為**。`GcCapture` trait 合約暗示「捕獲所有 GC 指標」，但實現沒有履行這個合約。這種「fail gracefully」的方式在 unsafe 上下文中尤其危險（write barriers, tracing），其中不正確的行為可能導致未定義的記憶體損壞。

**Geohot (Exploit 觀點):**
雖然這不是傳統的競爭條件（因為它在單個執行緒內），但它是一種更廣義的 TOCTOU 問題。「檢查」(try_borrow) 可能在想要捕獲的時候失敗。靜默失敗創造了一個視窗：
- 保持可變借用
- GC 觸發（例如並發 GC 的後台標記執行緒）
- 指標未被捕獲
- 物件被錯誤回收
- 稍後訪問時發生 UAF

在 async 上下文或啟用並發 GC 功能時，這一點特別令人擔憂。

---

## Resolution (2026-03-14)

**Outcome:** Fixed.

The fix was applied: `RefCell<T>`'s `GcCapture::capture_gc_ptrs_into` now uses blocking `borrow()` instead of `try_borrow()` (cell.rs:665-672). When the RefCell is mutably borrowed, `borrow()` panics — explicit failure is preferable to silent UAF. Regression tests in `tests/bug223_refcell_gccapture_silent_failure.rs` verify correct capture when not borrowed and panic when mutably borrowed.

