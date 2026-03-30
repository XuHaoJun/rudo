# [Bug]: GcHandle::clone 和 GcHandle::downgrade 在 orphan migration 視窗期間不正確地 panic

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序：clone/downgrade 在 migration 期間被調用 |
| **Severity (嚴重程度)** | High | 導致 panic（拒絕服務），但不會造成記憶體安全問題 |
| **Reproducibility (復現難度)** | Medium | 需要精確控制執行緒終止和 clone/downgrade 的時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::clone()` (cross_thread.rs:615-689), `GcHandle::downgrade()` (cross_thread.rs:458-549)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (012-cross-thread-gchandle feature)

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::clone()` 和 `GcHandle::downgrade()` 應該在 orphan migration 視窗期間正確處理有效的 handle，不應 panic。當 TCB roots 被遷移到 orphan table 的過程中，handle 應該可以在 orphan table 中找到。

### 實際行為 (Actual Behavior)

在 `migrate_roots_to_orphan()` 執行期間，存在一個 race window：
1. TCB roots lock 被釋放（entries 已排出到本地 vector）
2. Orphan lock 尚未取得（entries 尚未插入 orphan table）

如果在此視窗期間調用 `clone()` 或 `downgrade()`：
- `clone_orphan_root_with_inc_ref()` 返回 `(INVALID, false)`（handle 尚未在 orphan table）
- `origin_tcb.upgrade()` 返回 `Some`（TCB 仍然 alive）
- 但 `roots.strong` 為空（已被遷移）
- 結果：**panic at line 637/468**（「cannot clone/downgrade an unregistered GcHandle」）

### 對比 `resolve()` 的行為

`resolve()` 在 bug401 中已對此 race window 進行修復，採用 retry logic：
1. 如果 TCB alive 但 entry 不在 TCB roots，检查 orphan
2. 如果不在 orphan，重新檢查 TCB roots（migration 可能已在 orphan lock 獲取期間完成）
3. 如果仍找不到且 TCB 仍然 alive，再次檢查 orphan

但 `clone()` 和 `downgrade()` **沒有**這個 retry logic，會直接 panic。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### Migration Lock Ordering (heap.rs)

`migrate_roots_to_orphan()` 的鎖順序：
1. 取得 TCB roots lock
2. 將 entries 排出到本地 vector
3. **釋放 TCB roots lock** ← Handle 現在不在任何位置！
4. 取得 orphan lock
5. 將 entries 插入 orphan table
6. 釋放 orphan lock

### clone() 問題代碼 (cross_thread.rs:615-689)

```rust
let (new_id, origin_tcb) = if let (new_id, true) = heap::clone_orphan_root_with_inc_ref(...) {
    // Orphan path - 如果成功則使用
} else if let Some(tcb) = self.origin_tcb.upgrade() {
    // TCB path
    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    if !roots.strong.contains_key(&self.handle_id) {
        panic!("cannot clone an unregistered GcHandle");  // BUG: 沒有先檢查 orphan！
    }
    // ...
} else {
    panic!("cannot clone an unregistered GcHandle");
};
```

**問題**：如果 `clone_orphan_root_with_inc_ref()` 返回 `(INVALID, false)`（因為 migration 尚未完成），我們會進入 TCB path。如果 TCB 仍然 alive 但 `roots.strong` 為空（migration 已排出），我們會 panic 而不檢查 orphan table。

### downgrade() 問題代碼 (cross_thread.rs:458-468)

```rust
if let Some(tcb) = self.origin_tcb.upgrade() {
    let roots = tcb.cross_thread_roots.lock().unwrap();
    if !roots.strong.contains_key(&self.handle_id) {
        panic!("GcHandle::downgrade: handle has been unregistered");  // BUG: 沒有先檢查 orphan！
    }
    // ...
} else {
    // Orphan path - 這是正確的
    let orphan = heap::lock_orphan_roots();
    if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        panic!("GcHandle::downgrade: handle has been unregistered");
    }
    // ...
}
```

**問題**：當 TCB alive 但 `roots.strong` 為空時，會 panic 而不檢查 orphan table。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
//! PoC for GcHandle::clone/downgrade panic during orphan migration
//! 需要精確控制時序

use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;

#[derive(Clone, Trace)]
struct Data { value: i32 }

#[test]
fn test_clone_during_migration_race() {
    let handle = Arc::new(Gc::new(Data { value: 42 }).cross_thread_handle());
    
    // 在另一個執行緒中不斷嘗試 clone
    let handle_clone = handle.clone();
    let clone_count = Arc::new(AtomicUsize::new(0));
    let start_clone = Arc::new(AtomicBool::new(false));
    
    let clone_thread = thread::spawn(move || {
        while !start_clone.load(std::sync::atomic::Ordering::Relaxed) {
            thread::yield_now();
        }
        loop {
            // 這可能會在 migration 視窗期間 panic
            let _ = handle_clone.clone();
            clone_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            thread::yield_now();
        }
    });
    
    // 啟動 clone執行緒
    thread::sleep(Duration::from_micros(100));
    start_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    
    // 終止 origin執行緒（觸發 migration）
    drop(handle);  // handle 超出範圍，執行緒即將終止
    
    // 等待一段時間
    thread::sleep(Duration::from_millis(10));
    
    // Clone執行緒可能已 panic
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

借鑒 `resolve()` 的 bug401 修復，為 `clone()` 和 `downgrade()` 添加 retry logic 或 orphan fallback：

### 對於 clone()：

```rust
let (new_id, origin_tcb) = if let (new_id, true) = heap::clone_orphan_root_with_inc_ref(...) {
    // Orphan path - 成功
} else if let Some(tcb) = self.origin_tcb.upgrade() {
    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    if !roots.strong.contains_key(&self.handle_id) {
        drop(roots);
        // BUG401 fix: Fall back to orphan table
        let orphan = heap::lock_orphan_roots();
        if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
            // Retry orphan path
            if let (new_id, true) = heap::clone_orphan_root_with_inc_ref(...) {
                // ...
            } else {
                panic!("cannot clone an unregistered GcHandle");
            }
        } else {
            panic!("cannot clone an unregistered GcHandle");
        }
    }
    // ...
} else {
    // Orphan path - 如果 TCB 已死但 orphan 有這個 handle
    let orphan = heap::lock_orphan_roots();
    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        if let (new_id, true) = heap::clone_orphan_root_with_inc_ref(...) {
            // ...
        } else {
            panic!("cannot clone an unregistered GcHandle");
        }
    } else {
        panic!("cannot clone an unregistered GcHandle");
    }
};
```

### 或者更簡潔的方式：

仿效 `resolve()` 的模式，在 TCB path 中添加 orphan fallback：

```rust
} else if let Some(tcb) = self.origin_tcb.upgrade() {
    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    if !roots.strong.contains_key(&self.handle_id) {
        drop(roots);
        // Check orphan before panicking - migration may be in progress
        let orphan = heap::lock_orphan_roots();
        if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
            panic!("cannot clone an unregistered GcHandle");
        }
        // Retry from orphan...
    }
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
 orphan migration 是執行緒終止時的必要操作。GC 必須維護 outlive 建立執行緒的 handles。但鎖順序創建了一個視窗，期間 handle 對 clone/downgrade 不可見。這與 bug401 中 resolve() 的情況相同，但 clone/downgrade 尚未修復。

**Rustacean (Soundness 觀點):**
 這不是傳統的記憶體安全問題（沒有 UAF 或 use-after-free），但它是正確性問題導致 panic。clone() 或 downgrade() 在不應該失敗的時候失敗了。

**Geohot (Exploit 觀點):**
 這是潛在的拒絕服務（panic）漏洞。如果攻擊者可以控制執行緒終止的時序，他們可能會在 migration 視窗期間對 handle 調用 clone/downgrade 導致 panic。

---

## 相關 Bug

- **bug401**: GcHandle::resolve panic during cross-thread handle migration (已修復，使用 retry logic)
- **bug313**: GcHandle::is_valid() TOCTOU - Orphan Lock Release 到 TCB Check 之間的 Race Condition (已修復)
- **bug325**: Orphan Root Migration Race (已修復)
