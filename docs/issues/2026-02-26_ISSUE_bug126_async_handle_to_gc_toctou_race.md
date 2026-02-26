# [Bug]: AsyncHandle::to_gc TOCTOU - state check 與 inc_ref 非原子操作導致 Use-After-Free

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：to_gc 執行時物件正在被另一執行緒 drop |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::to_gc()`, `handles/async.rs:671-684`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`AsyncHandle::to_gc()` 存在 TOCTOU (Time-Of-Check-Time-Of-Use) race condition。函數先檢查物件狀態 flags，然後非原子地呼叫 `inc_ref()`，在檢查和遞增之間存在 race window。

### 預期行為 (Expected Behavior)
當物件正在被 drop（`dropping_state != 0`）或已死亡（`has_dead_flag` 設置）時，`to_gc()` 應返回有效的 Gc 或 panic，不應返回對已死亡物件的引用。

### 實際行為 (Actual Behavior)
即使 `has_dead_flag()`、`dropping_state()` 和 `is_under_construction()` 檢查在 `inc_ref()` 之前，在這兩者之間仍然存在一個 race window：
1. Thread A 檢查 flags → 全部通過（物件狀態正常）
2. Thread B 開始 drop 物件：設置 `dropping_state = 1`，設置 `DEAD_FLAG`，遞減 `ref_count` 到 `0`
3. Thread A 執行 `inc_ref()`：盲目遞增 ref_count，不檢查物件狀態
4. Thread A 返回 `Gc { ... }` → 可能指向已死亡物件！

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/async.rs:671-684`：

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let gc_box_ptr = (*self.slot).as_ptr() as *const GcBox<T>;
        let gc_box = &*gc_box_ptr;
        
        // 檢查 flags - lines 675-679
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "..."
        );
        
        // Race window: 在這裡另一個執行緒可以設置 dropping_state!
        
        // 非原子操作！沒有再次檢查物件狀態
        gc_box.inc_ref();  // line 681
        
        Gc::from_raw(gc_box_ptr as *const u8)
    }
}
```

**問題：**
1. 檢查和 `inc_ref()` 是分離的，非原子操作
2. `inc_ref()` 不檢查物件是否正在被 drop
3. 在多執行緒環境下，另一執行緒可以在檢查和遞增之間開始 drop 物件

**對比 GcBoxWeakRef::upgrade() 的正確做法：**
```rust
// ptr.rs:496 - 使用 try_inc_ref_if_nonzero 原子操作
if !gc_box.try_inc_ref_if_nonzero() {
    return None;
}
```

`try_inc_ref_if_nonzero()` 原子地檢查 `ref_count > 0` 並遞增，避免了 TOCTOU。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use rudo_gc::handles::AsyncHandleScope;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data { value: i32 }

fn toctou_race() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    
    // 創建物件並獲取 handle
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    // 在另一執行緒中 drop 物件
    let gc_clone = gc.clone();
    let ready = AtomicBool::new(false);
    let started = AtomicBool::new(false);
    
    let thread_handle = thread::spawn(move || {
        // 等待主執行緒準備好
        while !ready.load(Ordering::Relaxed) {}
        
        // Drop Gc - 這會觸發 drop 邏輯
        started.store(true, Ordering::Relaxed);
        drop(gc_clone);
        
        // 嘗試觸發 GC
        collect_full();
    });
    
    // 在 to_gc 前設置 ready，讓另一執行緒開始 drop
    ready.store(true, Ordering::Relaxed);
    
    // 立即調用 to_gc，爭取在另一執行緒 drop 時通過檢查
    // 這是一個不確定的 race，可能需要多次嘗試
    while !started.load(Ordering::Relaxed) {}
    
    let _escaped = handle.to_gc(); // 可能 UAF!
    
    thread_handle.join().unwrap();
}
```

**注意：** 這是一個不確定的 race 條件，可能需要多次運行或使用 stress testing 工具來穩定重現。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `AsyncHandle::to_gc()` 中的 `inc_ref()` 替換為 `try_inc_ref_if_nonzero()`：

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let gc_box_ptr = (*self.slot).as_ptr() as *const GcBox<T>;
        let gc_box = &*gc_box_ptr;
        
        // 保留原有的 assert 檢查（用於 panic 情況）
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "AsyncHandle::to_gc: cannot convert a dead, dropping, or under construction Gc"
        );
        
        // 使用原子操作替代非原子的檢查+遞增
        if !gc_box.try_inc_ref_if_nonzero() {
            // ref_count 為 0，物件正在被 drop
            panic!(
                "AsyncHandle::to_gc: object is being dropped by another thread"
            );
        }
        
        Gc::from_raw(gc_box_ptr as *const u8)
    }
}
```

**修復原理：**
- `try_inc_ref_if_nonzero()` 內部使用 `fetch_update` 原子地檢查 `ref_count > 0` 並遞增
- 如果 `ref_count == 0`（物件正在被 drop），返回 `false`
- 這消除了 TOCTOU race window

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 問題與 Bug119（GcBoxWeakRef::upgrade TOCTOU）屬於同一類問題。在 GC 系統中，引用計數的遞增必須與狀態檢查原子地進行，以防止在檢查和操作之間的 race window。`try_inc_ref_if_nonzero()` 提供了這種原子性保証。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。非原子的檢查+遞增模式允許返回指向已死亡物件的 Gc，導致 Use-After-Free。`inc_ref()` 應該被視為不安全操作，需要使用原子版本。

**Geohot (Exploit 觀點):**
攻擊者可以通過精確時序控制來利用此漏洞。如果物件被 drop 後記憶體未被立即重用，攻擊者可能讀取到舊資料。如果記憶體被新物件重用，可能造成指標混淆，進一步實現任意記憶體讀寫。

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Replaced `inc_ref()` with `try_inc_ref_if_nonzero()` in both `AsyncHandle::to_gc()` and `Handle::to_gc()` to eliminate the TOCTOU race window. The atomic `fetch_update` in `try_inc_ref_if_nonzero()` ensures ref_count > 0 is checked and incremented in a single operation. If the object is being dropped (ref_count == 0), the function panics with "object is being dropped by another thread".

Existing tests (including `repro_bug132_handle_to_gc_after_gc_dropped`, `test_async_handle_to_gc`) pass. Race conditions require Miri/TSan for reliable verification; single-threaded tests confirm the fix does not regress normal behavior.
