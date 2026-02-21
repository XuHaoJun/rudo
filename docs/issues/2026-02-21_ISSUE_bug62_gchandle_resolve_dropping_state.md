# [Bug]: GcHandle::resolve() 與 GcHandle::try_resolve() 缺少 dropping_state 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在物件正在被 drop 時呼叫 resolve |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制觸發 race condition |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當呼叫 `GcHandle::resolve()` 或 `GcHandle::try_resolve()` 時，如果物件正在被 drop（`dropping_state != 0`），應該：
- `resolve()`: panic 或返回安全的錯誤
- `try_resolve()`: 返回 `None`

這與 `Weak::upgrade()`、`GcBoxWeakRef::upgrade()` 的行為一致。

### 實際行為 (Actual Behavior)

目前 `resolve()` 和 `try_resolve()` 只檢查：
- `is_under_construction()` ✓
- `has_dead_flag()` ✓

但**缺少** `dropping_state()` 檢查：

```rust
// cross_thread.rs:158-169 (resolve)
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(
        !gc_box.is_under_construction(),
        "GcHandle::resolve: object is under construction"
    );
    assert!(
        !gc_box.has_dead_flag(),
        "GcHandle::resolve: object has been dropped (dead flag set)"
    );
    // BUG: 沒有檢查 dropping_state()!
    gc_box.inc_ref();
    Gc::from_raw(self.ptr.as_ptr() as *const u8)
}
```

```rust
// cross_thread.rs:203-210 (try_resolve)
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    if gc_box.is_under_construction() || gc_box.has_dead_flag() {
        return None;
    }
    // BUG: 沒有檢查 dropping_state()!
    gc_box.inc_ref();
    Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/cross_thread.rs:158-169` 和 `handles/cross_thread.rs:203-210`

在 bug39 中，已經為 `resolve()` 和 `try_resolve()` 添加了 `is_under_construction()` 和 `has_dead_flag()` 檢查，但**遺漏了** `dropping_state()` 檢查。

這與以下已修復的 bug 是相同的模式問題：
- bug41: `GcBoxWeakRef::upgrade()` 缺少 dropping_state 檢查 (Fixed)
- bug42: `Weak::try_upgrade()` 缺少 dropping_state 檢查 (Fixed)
- bug51: `GcHandle::downgrade()` 缺少 dead/dropping 檢查 (Fixed)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcHandle, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // 在一個執行緒中開始 drop 物件
    let handle_clone = handle.clone();
    let dropping = Arc::new(AtomicBool::new(false));
    let dropping_clone = dropping.clone();
    
    thread::spawn(move || {
        dropping_clone.store(true, Ordering::SeqCst);
        drop(handle_clone);  // 這會調用 GcHandle::drop -> dec_ref
    });
    
    // 等待另一執行緒開始 drop
    while !dropping.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    
    // 嘗試 resolve - dropping_state 可能 != 0
    // 預期: panic 或 None
    // 實際: 可能成功返回 Gc，導致 UAF
    let result = handle.resolve();
    
    // 使用 result 訪問資料 - 可能訪問已釋放的記憶體
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `resolve()` 中添加 `dropping_state()` 檢查：

```rust
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(...); // 執行緒檢查
    
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.is_under_construction(),
            "GcHandle::resolve: object is under construction"
        );
        assert!(
            !gc_box.has_dead_flag(),
            "GcHandle::resolve: object has been dropped (dead flag set)"
        );
        // 新增: 檢查 dropping_state
        assert!(
            gc_box.dropping_state() == 0,
            "GcHandle::resolve: object is being dropped"
        );
        gc_box.inc_ref();
        Gc::from_raw(self.ptr.as_ptr() as *const u8)
    }
}
```

在 `try_resolve()` 中添加 `dropping_state()` 檢查：

```rust
pub fn try_resolve(&self) -> Option<Gc<T>> {
    if std::thread::current().id() != self.origin_thread {
        return None;
    }
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        if gc_box.is_under_construction() 
            || gc_box.has_dead_flag() 
            || gc_box.dropping_state() != 0  // 新增
        {
            return None;
        }
        gc_box.inc_ref();
        Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 reference counting GC 中，當物件正在被 drop 時（dropping_state != 0），即使有其他強引用存在（ref_count > 0），也不應該允許建立新的強引用。這是因為 drop 過程可能會釋放相關的記憶體或執行特定的清理邏輯。新建立的強引用可能會在物件已經開始釋放後嘗試訪問，導致不一致的狀態。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。允許在物件正在被 drop 時建立新的 Gc<T> 可能導致 Use-After-Free（UAF）。攻擊者可以通過控制時序來利用這個漏洞，訪問已釋放或正在釋放的記憶體。

**Geohot (Exploit 攻擊觀點):**
這個漏洞可以被利用來實現 use-after-free：
1. 取得 GcHandle 指向物件 A
2. 在另一執行緒開始 drop 物件 A（設置 dropping_state = 1）
3. 在物件 A 完全釋放前，呼叫 resolve() 取得新的 Gc<T>
4. 利用新取得的 Gc<T> 訪問已釋放的記憶體

這與傳統的 double-free 或 use-after-free 漏洞類似，可以導致記憶體 corruption 甚至 code execution。

---

## Resolution

`GcHandle::resolve()` 與 `GcHandle::try_resolve()` 已新增 `dropping_state()` 檢查：resolve 在 dropping_state != 0 時 panic，try_resolve 在 dropping_state != 0 時回傳 None。
