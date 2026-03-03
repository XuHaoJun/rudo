# [Bug]: GcHandle::downgrade TOCTOU - Missing Lock Protection Between State Check and inc_weak

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 check 和 inc_weak之間精確時序 |
| **Severity (嚴重程度)** | Medium | 可能導致 Weak 指向無效物件 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade()` in `handles/cross_thread.rs:290-310`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::downgrade()` 應該在持有鎖的情況下檢查物件狀態並執行 `inc_weak()`，確保 atomic check-and-use 避免 TOCTOU。

### 實際行為 (Actual Behavior)

`GcHandle::downgrade()` 實作中，檢查物件狀態和呼叫 `inc_weak()` 之间没有锁保护：

1. Thread A: 執行 `downgrade()`，通過 assertions 檢查 (lines 295-302)
2. Thread B: 開始 drop 物件，設定 dropping_state = 1
3. Thread A: 呼叫 `inc_weak()` (line 303) - 成功增加 weak_count
4. 結果: `WeakCrossThreadHandle` 指向一個正在被 drop 的物件

### 程式碼位置

`handles/cross_thread.rs` 第 290-310 行 (`GcHandle::downgrade` 實作)：

```rust
#[must_use]
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::downgrade: cannot downgrade an unregistered GcHandle"
    );
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
        );
        gc_box.inc_weak();  // <-- TOCTOU: 沒有鎖保護，狀態可能已改變
    }
    // ...
}
```

對比 `GcHandle::clone()` (lines 344-361) - 它在鎖內執行 check + inc_ref：
```rust
let mut roots = tcb.cross_thread_roots.lock().unwrap();
// ... check ...
gc_box.inc_ref();  // 在鎖內
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**根本原因：**
1. `GcHandle::downgrade()` 沒有獲取 `cross_thread_roots` 鎖
2. 檢查物件狀態和呼叫 `inc_weak()` 之间沒有 atomic 保護
3. 另一個執行緒可以在 check 和 use 之間改變物件狀態

**Race 條件分析：**
- 兩個執行緒並發執行時，物件狀態可以在 assertion 和 inc_weak() 之間改變
- 導致 `WeakCrossThreadHandle` 指向一個無效/正在 drop 的物件
- 雖然後續的 `WeakCrossThreadHandle::upgrade()` 有防護，但 downgrade() 本身應該確保 atomic check-and-use

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 理論 PoC - 需要精確時序控制
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::time::Duration;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // 使用 Arc<Handle> 延遲 drop
    let handle_arc = Arc::new(handle);
    
    // 嘗試並發 downgrade 和 drop
    let handle_clone = handle_arc.clone();
    let t1 = thread::spawn(move || {
        // 持續嘗試 downgrade
        for _ in 0..10000 {
            let _ = handle_clone.downgrade();
        }
    });
    
    let handle_clone2 = Arc::clone(&handle_arc);
    let t2 = thread::spawn(move || {
        // 嘗試在另一個執行緒 drop
        thread::sleep(Duration::from_nanos(1));
        drop(handle_clone2);
    });
    
    t1.join();
    t2.join();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用鎖保護 check-and-use：

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::downgrade: cannot downgrade an unregistered GcHandle"
    );
    
    if let Some(tcb) = self.origin_tcb.upgrade() {
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        if !roots.strong.contains_key(&self.handle_id) {
            panic!("GcHandle::downgrade: handle has been unregistered");
        }
        unsafe {
            let gc_box = &*self.ptr.as_ptr();
            assert!(
                !gc_box.has_dead_flag()
                    && gc_box.dropping_state() == 0
                    && !gc_box.is_under_construction(),
                "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
            );
            gc_box.inc_weak();
        }
    } else {
        // 處理 orphan 情況
        let orphan = heap::lock_orphan_roots();
        if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
            panic!("GcHandle::downgrade: handle has been unregistered");
        }
        unsafe {
            let gc_box = &*self.ptr.as_ptr();
            assert!(
                !gc_box.has_dead_flag()
                    && gc_box.dropping_state() == 0
                    && !gc_box.is_under_construction(),
                "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
            );
            gc_box.inc_weak();
        }
    }
    
    WeakCrossThreadHandle {
        weak: GcBoxWeakRef::new(self.ptr),
        origin_tcb: Weak::clone(&self.origin_tcb),
        origin_thread: self.origin_thread,
    }
}
```

關鍵修改：
1. 在檢查和 inc_weak() 之間持有 cross_thread_roots 鎖
2. 確保 atomic check-and-use
3. 參考 GcHandle::clone() 的模式

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
downgrade() 應該遵循與 clone() 相同的鎖定模式，確保在修改引用計數之前驗證物件狀態。這確保了 weak reference 的正確性。

**Rustacean (Soundness 觀點):**
這是一個 TOCTOU 漏洞，可能導致建立指向無效物件的 weak handle。雖然後續的 upgrade() 會檢查，但 downgrade() 本身應該確保 atomic check-and-use。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠控制時序，他們可能利用這個 TOCTOU 建立一個指向正在 drop 的物件的 weak handle，進一步利用記憶體佈局。

---

## Resolution Note (2026-03-03)

**Fixed.** `GcHandle::downgrade()` now holds `cross_thread_roots` (when origin TCB is alive) or `lock_orphan_roots()` (when orphaned) during the state check and `inc_weak()` call, matching the pattern used by `GcHandle::clone()`. This prevents TOCTOU with concurrent unregister/drop. All cross_thread_handle and cross_thread_weak_clone tests pass.

