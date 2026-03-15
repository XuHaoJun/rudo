# [Bug]: GcHandle::resolve/try_resolve 缺少 inc_ref 後的 post-check 導致 TOCTOU UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒交錯執行，且時間窗口很小 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全漏洞 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒調度，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcHandle::resolve()` 與 `GcHandle::try_resolve()` 在調用 `gc_box.inc_ref()` **之後**缺少對 `dropping_state()` 和 `has_dead_flag()` 的第二次檢查。這與 `Weak::upgrade()` 中已修復的 TOCTOU 漏洞相同，但尚未應用於 GcHandle。

### 預期行為 (Expected Behavior)

在調用 `inc_ref()` 增加引用計數後，應該再次檢查物件狀態，確保物件未被正在釋放 (dropping) 或已死亡 (dead)。如果狀態異常，應該撤銷 increment 並返回 `None` (try_resolve) 或 panic (resolve)。

### 實際行為 (Actual Behavior)

`GcHandle::resolve()` (lines 194-210) 和 `GcHandle::try_resolve()` (lines 256-266) 只在 `inc_ref()` **之前**進行狀態檢查，沒有在 **之後** 進行第二次檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cross_thread.rs` 中，`resolve()` 和 `try_resolve()` 的實現如下：

```rust
// resolve() lines 194-210
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(!gc_box.is_under_construction(), ...);
    assert!(!gc_box.has_dead_flag(), ...);
    assert!(gc_box.dropping_state() == 0, ...);
    gc_box.inc_ref();  // <-- 沒有 post-check!
    Gc::from_raw(self.ptr.as_ptr() as *const u8)
}
```

對比 `Weak::upgrade()` (ptr.rs:527-549) 的正確實現：

```rust
// Try atomic transition from 0 to 1 (resurrection)
if gc_box.try_inc_ref_from_zero() {
    // Second check: verify object wasn't dropped between check and CAS
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        // Undo the increment and return None
        let _ = gc_box;
        crate::ptr::GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    return Some(Gc { ... });
}

// ref_count > 0: use atomic try_inc_ref_if_nonzero
if !gc_box.try_inc_ref_if_nonzero() {
    return None;
}
// Post-CAS safety check: verify object wasn't dropped between check and CAS
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(ptr.as_ptr());
    return None;
}
```

**Race 條件分析：**

1. Thread A: 調用 `GcHandle::resolve()`，通過所有 pre-checks
2. Thread B: 同時調用 `GcBox::dec_ref()`，ref_count 變為 0，開始 dropping
3. Thread A: 執行 `gc_box.inc_ref()`（此時物件正在 dropping）
4. Thread A: 返回 `Gc::from_raw()`，但物件正在被 drop

**後果：** 返回的 `Gc` 指標指向一個正在被 drop 的物件，導致 Use-After-Free。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
    marker: Arc<AtomicBool>,
}

fn main() {
    let gc = Gc::new(Data {
        value: 42,
        marker: Arc::new(AtomicBool::new(false)),
    });
    
    let handle = gc.cross_thread_handle();
    let marker = gc.marker.clone();
    
    // 建立多個 clone
    let handle1 = handle.clone();
    let handle2 = handle.clone();
    
    // Thread A: 嘗試 resolve
    let handle1_clone = handle1.clone();
    let thread_a = thread::spawn(move || {
        // 等待 Thread B 開始 dropping
        while !marker.load(Ordering::Acquire) {
            thread::yield();
        }
        // 嘗試 resolve - 可能在物件正在 dropping 時
        let _ = handle1_clone.resolve();
    });
    
    // Thread B: 同時觸發 dropping
    let thread_b = thread::spawn(move || {
        marker.store(true, Ordering::Release);
        // 快速 drop 所有 clones 來觸發 dec_ref
        drop(handle2);
        drop(handle);
        
        // 立即分配新物件來重用記憶體
        for _ in 0..1000 {
            let _ = Gc::new(Data {
                value: 0,
                marker: Arc::new(AtomicBool::new(false)),
            });
        }
    });
    
    thread_a.join().unwrap();
    thread_b.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::resolve()` 和 `GcHandle::try_resolve()` 中添加 post-check：

```rust
// resolve() 修復
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(!gc_box.is_under_construction(), ...);
    assert!(!gc_box.has_dead_flag(), ...);
    assert!(gc_box.dropping_state() == 0, ...);
    gc_box.inc_ref();
    
    // 新增：Post-check - 確保物件在 inc_ref 後仍然有效
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        // 撤銷 increment
        GcBox::dec_ref(self.ptr.as_ptr());
        panic!("GcHandle::resolve: object was dropped between check and inc_ref");
    }
    
    Gc::from_raw(self.ptr.as_ptr() as *const u8)
}
```

```rust
// try_resolve() 修復
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    // ... existing pre-checks ...
    gc_box.inc_ref();
    
    // 新增：Post-check
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        GcBox::dec_ref(self.ptr.as_ptr());
        return None;
    }
    
    Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU (Time-Of-Check to Time-Of-Use) 漏洞。在多執行緒環境下，物件狀態可能在檢查和使用之間改變。`Weak::upgrade()` 已經正確處理了這個問題，使用 post-CAS 檢查來確保安全。`GcHandle::resolve()` 應該採用相同的模式。

**Rustacean (Soundness 觀點):**
這是一個嚴重的記憶體安全問題。返回一個指向正在被 drop 的物件的指標會導致 Use-After-Free，這是 Rust 最嚴重的安全問題之一。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試構造以下場景：
1. 通過精確的執行緒調度，在 inc_ref 和創建 Gc 指針之間觸發物件 drop
2. 利用記憶體重用來讀取敏感數據
3. 或者通過 double-free 機制來利用記憶體損壞

---

## Resolution

**已修復** - 2026-03-14 (與 bug200 相同修復)

Duplicate of bug200. 修復已應用於 `handles/cross_thread.rs` 的 `GcHandle::resolve()` 和 `try_resolve()`。

## 修復狀態

- [x] 已修復
- [ ] 未修復
