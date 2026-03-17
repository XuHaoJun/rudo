# [Bug]: Orphan Root Migration Race - handle appears unregistered during migration window

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序：migrate_roots_to_orphan 釋放 TCB lock 後、取得 orphan lock 前 |
| **Severity (嚴重程度)** | High | 導致 panic ("handle has been unregistered")，阻斷程式執行 |
| **Reproducibility (復現難度)** | Medium | 需要並發場景：origin thread 終止時另一執行緒嘗試 resolve |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `migrate_roots_to_orphan()`, `GcHandle::resolve()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

在 `migrate_roots_to_orphan()` 函數中，從 TCB 遷移 roots 到 orphan table 之間存在一個 race window。當 origin thread 終止時，另一執行緒可能在此時嘗試呼叫 `resolve()`，導致 handle 被錯誤地視為已註銷。

### 預期行為 (Expected Behavior)

當 origin thread 終止時，該 thread 的所有 cross-thread handles 應該正確地從 TCB roots 遷移到 orphan table。在遷移完成前或完成後，`resolve()` 都應該能夠正確找到 handle（如果 handle 有效的話）。

### 實際行為 (Actual Behavior)

在 `migrate_roots_to_orphan()` 的以下時序發生 race：
1. TCB roots lock 釋放 (line 185: `drop(roots)`)
2. orphan table lock 取得 (line 187: `let mut orphan = ...`)

在此時間窗口內，如果另一執行緒呼叫 `resolve()`：
1. 檢查 TCB roots - 不存在（已排乾）
2. 檢查 orphan table - 不存在（尚未插入）
3. panic: "GcHandle::resolve: handle has been unregistered"

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs:172-191` 的 `migrate_roots_to_orphan()` 函數：

```rust
pub fn migrate_roots_to_orphan(tcb: &ThreadControlBlock, thread_id: ThreadId) {
    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    // ... drain roots ...
    drop(roots);  // <-- Line 185: 釋放 TCB lock

    // RACE WINDOW: 在此處另一執行緒可以呼叫 resolve()
    
    let mut orphan = orphaned_cross_thread_roots().lock();  // <-- Line 187: 取得 orphan lock
    // ... insert to orphan ...
}
}

而在 `cross_thread.rs:167-202` 的 `resolve()` 函數：
```rust
self.origin_tcb.upgrade().map_or_else(
    || {
        let orphan = heap::lock_orphan_roots();
        if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
            panic!("GcHandle::resolve: handle has been unregistered");  // <-- 在 race window 內會觸發
        }
        // ...
    },
    |tcb| {
        let roots = tcb.cross_thread_roots.lock().unwrap();
        if !roots.strong.contains_key(&self.handle_id) {
            panic!("GcHandle::resolve: handle has been unregistered");
        }
        // ...
    },
)
```

**問題核心**：在釋放 TCB lock 和取得 orphan lock 之間，沒有任何同步機制。`resolve()` 可能在此時檢查到「既不在 TCB，也不在 orphan」的狀態。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcHandle, static_collect};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone)]
struct Data {
    value: Arc<AtomicUsize>,
}
static_collect!(Data);

fn main() {
    let data = Data {
        value: Arc::new(AtomicUsize::new(42)),
    };
    
    let handle: GcHandle<Data> = Gc::new(data).into_handle();
    let handle_clone = handle.clone();
    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = ready.clone();
    
    // Spawn thread that will try to resolve during migration
    let resolver = thread::spawn(move || {
        // Wait until migration is in progress
        while !ready_clone.load(Ordering::Relaxed) {
            thread::yield_now();
        }
        // Try to resolve - may panic if race triggers
        let _ = handle_clone.try_resolve();
    });
    
    // Spawn thread that will terminate and trigger migration
    let terminator = thread::spawn(move || {
        let _handle = handle; // Keep handle alive in this thread
        // Thread terminates here, triggering migrate_roots_to_orphan
    });
    
    // Signal migration to start
    ready.store(true, Ordering::Relaxed);
    
    // Wait for both threads
    resolver.join().unwrap();
    terminator.join().unwrap();
    
    println!("Test completed");
}
```

**注意**：此 PoC 可能需要多次執行才能觸發 race condition。可使用 stress testing 或注入延遲來提高重現機率。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**選項 1：持有雙重鎖**
在整個遷移過程中同時持有 TCB lock 和 orphan lock：
```rust
pub fn migrate_roots_to_orphan(tcb: &ThreadControlBlock, thread_id: ThreadId) {
    let roots = tcb.cross_thread_roots.lock().unwrap();
    if roots.strong.is_empty() {
        return;
    }
    let drained: Vec<_> = roots.strong.drain().collect();
    
    // 同時取得 orphan lock（在 release TCB lock 之前）
    let mut orphan = orphaned_cross_thread_roots().lock();
    for (handle_id, ptr) in drained {
        orphan.insert((thread_id, handle_id), ptr.as_ptr() as usize);
    }
}
```

**選項 2：使用 atomic flag**
在 TCB 中添加一個 flag 表示「正在遷移中」，`resolve()` 在檢查時需要：
1. 檢查 TCB 是否正在遷移
2. 如果是，則等待遷移完成後再檢查

**選項 3（推薦）：確保 migrate 完成後才允許 resolve**
修改 `resolve()` 的邏輯：
- 當 TCB upgrade 失敗時，不直接檢查 orphan
- 而是先確認遷移已經完成（可以透過檢查 thread 是否還在 thread registry 中）

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題涉及 GC 系統中執行緒生命週期管理的時序問題。當執行緒終止時，roots 的遷移必須與其他執行緒對 handle 的訪問正確同步。當前的實現在釋放 TCB lock 和取得 orphan lock 之間存在一個關鍵的同步缺口，可能導致有效的 handle 被錯誤地視為已註銷。

**Rustacean (Soundness 觀點):**
此問題不會導致記憶體不安全（不會 UAF），但會導致 panic，降低程式可用性。從 API 角度來看，`resolve()` 在有效 handle 的情況下應該成功，而不應該因為內部實現的 race condition 而失敗。

**Geohot (Exploit 觀點):**
雖然這不是傳統的安全漏洞，但攻擊者可能透過精心設計的執行緒時序來觸發此 panic，導致服務阻斷（DoS）。在多執行緒 GC 系統中，此類時序問題是經典的攻擊面。
