# [Bug]: RefCell GcCapture 使用 borrow() 導致 panic

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 開發者常見使用 RefCell 進行 interior mutability |
| **Severity (嚴重程度)** | High | 導致 GC 運行時 panic，而非優雅處理 |
| **Reproducibility (復現難度)** | Medium | 需要在 GC 運行時剛好處於 mutable borrow 狀態 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** RefCell GcCapture implementation
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`RefCell<T>` 的 `GcCapture` 實作使用 `self.borrow()` 來獲取內部值，當 RefCell 已經被 mutable borrow 時會觸發 panic。

### 預期行為 (Expected Behavior)
應該使用 `try_borrow()` 並在 borrow 失敗時優雅地跳過 capturing，類似 `RwLock` 的實作方式。

### 實際行為 (Actual Behavior)
直接調用 `self.borrow()` 會在 GC 運行時 panic，如果剛好有 mutable borrow 存在。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/cell.rs:591`:
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let value = self.borrow();  // <-- 會 panic
    value.capture_gc_ptrs_into(ptrs);
}
```

對比 `RwLock` 的正確實作 (`cell.rs:604-607`):
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // Use blocking read() to reliably capture all GC pointers, same as GcRwLock (bug34).
    if let Ok(guard) = self.read() {
        guard.capture_gc_ptrs_into(ptrs);
    }
}
```

`RwLock` 使用 `self.read()` (相當於 try_read) 並檢查 `Ok(guard)`，而 `RefCell` 直接使用會 panic 的 `borrow()`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, collect_full};
use std::cell::RefCell;

fn main() {
    let cell = Gc::new(RefCell::new(vec![Gc::new(42)]));
    
    // 建立 mutable borrow
    let mut_borrow = cell.borrow_mut();
    
    // 嘗試觸發 GC - 這會導致 panic
    collect_full();
    
    println!("Should not reach here");
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `cell.rs:591` 的 `RefCell` 實作，使用 `try_borrow()`:

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Ok(value) = self.try_borrow() {
        value.capture_gc_ptrs_into(ptrs);
    }
    // 如果 borrow 失敗，什麼都不做，跳過 capturing
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 角度，這是一個優雅降級的問題。當無法安全地捕獲指標時，應該跳過而非崩潰。這與 SATB 的設計目標一致：儘量減少 GC 對用戶代碼的干擾。

**Rustacean (Soundness 觀點):**
這不是嚴格意義上的 soundness bug（因為 panic 是 Rust 的合法行為），但從可用性角度，這是一個設計缺陷。`RwLock` 已經展示了正確的模式，應該保持一致。

**Geohot (Exploit 觀點):**
雖然不是安全漏洞，但攻擊者可能利用這個行為進行 DoS。通過刻意持有 mutable borrow 並觸發 GC，可以使服務崩潰。

---

## Resolution (2026-02-26)

**Outcome:** Already fixed. Same fix as bug85.

The `RefCell` GcCapture implementation in `cell.rs` lines 632-636 uses `try_borrow()` and skips capturing when borrow fails. No code changes required.
