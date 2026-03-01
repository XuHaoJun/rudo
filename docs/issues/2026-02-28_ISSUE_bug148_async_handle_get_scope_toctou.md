# [Bug]: AsyncHandle::get() TOCTOU - is_scope_active 檢查與 slot 存取非原子操作導致 Use-After-Free

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：scope drop 時另一執行緒正在調用 get() |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (Reproducibility)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()`, `handles/async.rs:570-594`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`AsyncHandle::get()` 存在 TOCTOU (Time-Of-Check-Time-Of-Use) race condition。函數先檢查 scope 是否仍然有效，然後非原子地存取 slot，在檢查和存取之間存在 race window。

### 預期行為 (Expected Behavior)
當 scope 被 drop 時，`get()` 應該檢測到 scope 已失效並 panic，不應存取已釋放的記憶體。

### 實際行為 (Actual Behavior)
即使 `is_scope_active()` 檢查在 `slot` 存取之前，在這兩者之間仍然存在一個 race window：
1. Thread A 呼叫 `get()`，執行 `is_scope_active()` 檢查 → scope 仍活躍，返回 `true`
2. Thread B 開始 drop scope：呼叫 `unregister_async_scope()`，從 `active_scope_ids` 移除 scope ID
3. Thread A 執行 `let slot = unsafe { &*self.slot }` → 存取已失效的 slot指標！

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/async.rs:570-594`：

```rust
pub fn get(&self) -> &T {
    let tcb = crate::heap::current_thread_control_block()
        .expect("AsyncHandle::get() must be called within a GC thread");

    // 檢查 scope 是否活躍 - line 574
    // 問題：此處獲取並釋放 lock，但後續存取 slot 時 lock 已釋放
    if !tcb.is_scope_active(self.scope_id) {
        panic!(...);
    }

    // Race window: 在這裡另一個執行緒可以 drop scope！

    // 非原子操作！scope 可能在檢查後失效
    let slot = unsafe { &*self.slot };  // line 582
    // ...
}
```

**問題：**
1. `is_scope_active()` 內部獲取並釋放 `active_scope_ids` 的 lock（見 heap.rs:405-407）
2. 檢查和 `slot` 存取是分離的，非原子操作
3. 在多執行緒環境下，另一執行緒可以在檢查和存取之間 drop scope
4. Bug115 的修復添加了 scope 檢查，但**未解決**檢查和存取之間的 TOCTOU

**對比 `GcHandle::resolve()` 的正確做法：**
```rust
// cross_thread.rs:178-200 - lock 在整個 check+use 期間持有
let roots = tcb.cross_thread_roots.lock().unwrap();
if !roots.strong.contains_key(&self.handle_id) {
    panic!("...");
}
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    // 檢查和 inc_ref 都在 lock 保護下
    gc_box.inc_ref();
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data { value: i32 }

fn toctou_race() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    let ready = AtomicBool::new(false);
    let started = AtomicBool::new(false);
    let handle_ptr = &handle as *const _;
    
    let thread_handle = thread::spawn(move || {
        while !ready.load(Ordering::Relaxed) {}
        
        started.store(true, Ordering::Relaxed);
        drop(scope);  // Drop scope while handle is being accessed
    });
    
    ready.store(true, Ordering::Relaxed);
    
    while !started.load(Ordering::Relaxed) {}
    
    // 可能 UAF! - scope 可能在 is_scope_active 通過後、slot 存取前被 drop
    unsafe {
        let _ = (*handle_ptr).get();
    }
    
    thread_handle.join().unwrap();
}
```

**注意：** 這是一個不確定的 race 條件，可能需要多次運行或使用 stress testing 工具來穩定重現。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 scope 檢查和 slot 存取放在同一個 lock 保護下：

```rust
pub fn get(&self) -> &T {
    let tcb = crate::heap::current_thread_control_block()
        .expect("AsyncHandle::get() must be called within a GC thread");

    // 方案 1: 獲取 active_scope_ids lock 並保持到 slot 存取完成
    let _guard = tcb.active_scope_ids.lock().unwrap();
    
    if !tcb.active_scope_ids.lock().unwrap().contains(&self.scope_id) {
        panic!(
            "AsyncHandle used after scope was dropped. \
             The AsyncHandleScope that created this handle has been dropped. \
             Ensure the scope stays alive as long as any handles are in use."
        );
    }

    // Now safe to access slot - lock is held
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe {
        let gc_box = &*gc_box_ptr;
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "AsyncHandle::get: cannot access a dead, dropping, or under construction Gc"
        );
        gc_box.value()
    }
}
```

或者參考 cross_thread.rs 的做法，在整個 check+use 期間持有 lock。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 問題與 Bug115 相關。Bug115 修復了 release 構建中缺少 scope 檢查的問題，但引入了新的 TOCTOU race。在 GC 系統中，檢查和操作必須原子地進行，特別是在並發環境下。建議使用與 `GcHandle::resolve()` 相同的模式：在 lock 保護下進行 check+use。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。Scope 檢查和 slot 存取之間的 race window 允許 use-after-free。雖然發生的概率較低（需要精確的時序），但一旦發生就會導致未定義行為。

**Geohot (Exploit 觀點):**
攻擊者可以通過精確時序控制來利用此漏洞。如果 scope 被 drop 但記憶體未被立即重用，攻擊者可能讀取到舊資料。如果記憶體被新物件重用，可能造成指標混淆，進一步實現任意記憶體讀寫。

---

## Resolution (2026-03-02)

**Outcome:** Fixed and verified.

**Root cause confirmed:** `AsyncHandle::get()` and `AsyncHandle::to_gc()` both called
`tcb.is_scope_active()` (which acquires and immediately releases `active_scope_ids` lock),
then accessed `self.slot` without the lock — leaving a race window where the scope could
be dropped and `AsyncScopeData` freed between the check and the dereference.

**Fix applied** in `crates/rudo-gc/src/handles/async.rs` and `src/heap.rs`:

1. Added `ThreadControlBlock::with_scope_lock_if_active()` — runs a closure while holding
   the `active_scope_ids` lock, returning `None` if the scope is no longer active.  Because
   `unregister_async_scope` must acquire this same lock before it can remove the scope Arc
   (and thus free `AsyncScopeData`), holding it for the duration of the closure guarantees
   `self.slot` remains valid throughout the dereference.

2. Replaced the open-coded `is_scope_active()` + slot-access pattern in both `get()` and
   `to_gc()` with `with_scope_lock_if_active(…)`, so the TOCTOU window is eliminated.

**Tests:** Full test suite (`bash test.sh`) passes. Clippy clean.
