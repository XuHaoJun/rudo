# [Bug]: Weak::try_upgrade 缺少 CAS 後的第二次檢查導致 TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需在 upgrade 過程中有 concurrent drop 發生 |
| **Severity (嚴重程度)** | `Critical` | 可能導致 UAF |
| **Reproducibility (復現難度)** | `Medium` | 需特定時序才能觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>::try_upgrade` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為
`Weak::try_upgrade` 應該與 `Weak::upgrade` 具有相同的安全檢查。在 CAS 成功後，需要第二次檢查 `dropping_state` 和 `has_dead_flag` 以防止 TOCTOU race。

### 實際行為
`Weak::upgrade` (ptr.rs:1755-1765) 正確地包含 CAS 後的第二次檢查，但 `Weak::try_upgrade` (ptr.rs:1834-1849) 缺少此檢查。當 ref_count 不為零時嘗試升級，可能會在檢查和 CAS 之間 object 被 drop，導致 UAF。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`Weak::upgrade` (lines 1745-1771) 有正確的實作：
```rust
if gc_box.ref_count.compare_exchange_weak(...).is_ok() {
    // Post-CAS safety check: re-verify after owning a count
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        // Undo the increment and return None
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    return Some(...);
}
```

但 `Weak::try_upgrade` (lines 1834-1849) 使用簡單的 compare_exchange，沒有第二次檢查：
```rust
if gc_box.ref_count.compare_exchange_weak(...).is_ok() {
    // 直接 return，沒有第二次檢查!
    crate::gc::notify_created_gc();
    return Some(Gc { ... });
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    flag: Arc<AtomicBool>,
}

static START_DROP: AtomicBool = AtomicBool::new(false);

fn main() {
    let flag = Arc::new(AtomicBool::new(false));
    let gc = Gc::new(Data { flag: flag.clone() });
    let weak = Gc::downgrade(&gc);
    
    // 在另一個 thread 啟動 drop
    let handle = thread::spawn(move || {
        while !START_DROP.load(Ordering::Relaxed) {}
        drop(gc);
    });
    
    // 開始 drop
    START_DROP.store(true, Ordering::Relaxed);
    
    // 同時嘗試 try_upgrade - 可能會在 drop 過程中讀取
    // 由於缺少第二次檢查，可能會 UAF
    let _ = weak.try_upgrade();
    
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::try_upgrade` 中，CAS 成功後添加第二次檢查，類似 `Weak::upgrade` 的實作：

```rust
if gc_box.ref_count.compare_exchange_weak(...).is_ok() {
    // 添加第二次檢查
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        let _ = gc_box;
        crate::ptr::GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    crate::gc::notify_created_gc();
    return Some(Gc { ... });
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`Weak::upgrade` 已有正確的 double-check 模式（防止 resurrection TOCTOU）。`Weak::try_upgrade` 應該共用相同的安全檢查邏輯。兩個函數使用相同的 compare_exchange 模式，應該有一致的錯誤處理。

**Rustacean (Soundness 觀點):**
缺少第二次檢查可能導致 UAF：物件在檢查通過後、CAS 前被 drop，但 CAS 仍然成功（因為 ref_count 還沒變），導致返回一個指向已釋放記憶體的 Gc。這與 bug167 報告的問題相同，但 bug167 錯誤地聲稱 `upgrade` 也缺少此檢查（實際上 `upgrade` 已經修復）。

**Geohot (Exploit 觀點):**
這是一個經典的 TOCTOU 漏洞。攻擊者可以透過精確時序控制，在 try_upgrade 過程中觸發 concurrent drop，導致 use-after-free。與 `upgrade` 函數相比，`try_upgrade` 額外檢查了 `usize::MAX` 溢位，但卻遺漏了更重要的第二次狀態檢查。

---

## Resolution (2026-03-02)

**Outcome:** Fixed.

Added the post-CAS safety check to `Weak::try_upgrade` in `ptr.rs`, matching the pattern already present in `Weak::upgrade`:

```rust
if gc_box.ref_count.compare_exchange_weak(...).is_ok() {
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    // ...
}
```

All Weak-related tests pass. TOCTOU race conditions require Miri/ThreadSanitizer for reliable verification; single-threaded tests pass.
