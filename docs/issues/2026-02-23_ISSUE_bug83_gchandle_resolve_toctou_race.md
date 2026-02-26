# [Bug]: GcHandle resolve/clone 存在 TOCTOU Race Condition 導致 Use-After-Free

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要多執行緒並髮操作才能觸發 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，讀取已釋放記憶體 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`, `GcHandle::clone()`, `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`resolve()`、`try_resolve()` 和 `clone()` 在檢查 `handle_id` 有效性後，後續對 `GcBox` 的操作（如 `inc_ref()`）應該是安全的：
- 要麼 handle 有效且物件受 root table 保護
- 要麼 handle 失效且函數提前返回

### 實際行為 (Actual Behavior)

存在 **TOCTOU (Time-of-Check-Time-of-Use)** 競爭視窗：

1. Thread A: `resolve()` 檢查 `handle_id` (valid)
2. Thread B: `unregister()` 獲取鎖、從 roots 移除、設 `handle_id = INVALID`、呼叫 `dec_ref()` (可能 drop 物件！)
3. Thread A: `inc_ref()` 在已釋放的物件上執行 -> **Use-After-Free**

### 相關 Issue

- bug72: GcHandle::resolve() / try_resolve() 未檢查 handle_id 是否已失效（已部分修復：添加了檢查）
- 本 issue 是 bug72 的**延伸問題**：即使添加了檢查，仍存在 TOCTOU 競爭

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/cross_thread.rs`

在 `resolve()` 中（第 147-178 行）：
```rust
pub fn resolve(&self) -> Gc<T> {
    assert!(self.handle_id != HandleId::INVALID, ...);  // CHECK (line 148-151)
    // ... thread check ...
    unsafe {
        let gc_box = &*self.ptr.as_ptr();  // READ without lock (line 163)
        // ... assertions ...
        gc_box.inc_ref();  // USE - 可能在已釋放物件上執行！(line 176)
        Gc::from_raw(...)
    }
}
```

在 `unregister()` 中（第 104-117 行）：
```rust
pub fn unregister(&mut self) {
    if self.handle_id == HandleId::INVALID {
        return;
    }
    if let Some(tcb) = self.origin_tcb.upgrade() {
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);  // 從 root 移除
        drop(roots);
    } else {
        let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
    }
    self.handle_id = HandleId::INVALID;  // 設為無效
    crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());  // 可能 drop 物件！
}
```

**競爭視窗：** 在 `handle_id` 被設為 `INVALID` 之後、但在 `dec_ref()` 執行期間（或執行後），另一個執行緒的 `resolve()` 可能已經通過了 `handle_id` 檢查並正在執行 unsafe 區塊。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要使用 ThreadSanitizer 或精心設計的時序來觸發。單執行緒無法可靠復現此問題。

概念驗證（需要多執行緒）：
```rust
use rudo_gc::{Gc, Trace};
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
    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = ready.clone();
    
    // Thread A: 不斷呼叫 resolve()
    let handle_a = handle.clone();
    let t1 = thread::spawn(move || {
        while !ready_clone.load(Ordering::Relaxed) {
            thread::yield();
        }
        for _ in 0..10000 {
            let _ = handle_a.try_resolve(); // 重複呼叫
        }
    });
    
    // Thread B: 不斷呼叫 unregister() 然後重建
    let mut handle_b = handle.clone();
    let t2 = thread::spawn(move || {
        ready.store(true, Ordering::Relaxed);
        for _ in 0..10000 {
            handle_b.unregister(); // 移除 root
            // 快速重建以產生競爭
            if let Ok(new_gc) = gc.try_resolve() {
                handle_b = new_gc.cross_thread_handle();
            }
        }
    });
    
    t1.join().unwrap();
    t2.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

需要使 handle 有效性檢查和引用計數操作具有原子性：

1. **方案一**：在檢查有效性時持有 `cross_thread_roots` 鎖：
   ```rust
   pub fn resolve(&self) -> Gc<T> {
       assert!(self.handle_id != HandleId::INVALID, ...);
       assert_eq!(std::thread::current().id(), self.origin_thread, ...);
       
       // 需要獲取鎖來保護 check + use 的原子性
       if let Some(tcb) = self.origin_tcb.upgrade() {
           let roots = tcb.cross_thread_roots.lock().unwrap();
           if !roots.strong.contains_key(&self.handle_id) {
               panic!("GcHandle::resolve: handle has been unregistered");
           }
           unsafe {
               let gc_box = &*self.ptr.as_ptr();
               gc_box.inc_ref();
               return Gc::from_raw(self.ptr.as_ptr() as *const u8);
           }
       }
       // ... orphan handling ...
   }
   ```

2. **方案二**：使用世代計數器（generation counter）來追蹤 handle 的有效性：
   - 在 `GcHandle` 中新增 `generation: AtomicU64`
   - 在 `resolve()` 時檢查並遞增世代
   - 這樣可以在不持有鎖的情況下檢測競爭

3. **方案三**：改用 RAII 風格的 API，讓 handle 持有鎖直到操作完成：
   - 這會是較大的 API 變動

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 競爭條件。在 GC 系統中，root table 保護物件不被回收是核心不變量。當 handle 被從 root table 移除但尚未完成 `dec_ref()` 時，物件處於「過渡狀態」——理論上可以被回收。此時另一個執行緒若呼叫 `resolve()` 並通過檢查，會在不受保護的物件上執行 `inc_ref()`，導致 use-after-free。

**Rustacean (Soundness 觀點):**
這是明確的 undefined behavior。即使有 handle_id 檢查，檢查和使用之間缺乏同步，導致檢查變得無意義。根據 Rust 的記憶體模型，這種「檢查後使用」但沒有同步的模式被視為 data race。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以透過精確時序控制：
1. 建立 GcHandle
2. 將 handle 傳送給另一個執行緒
3. 使用時序技巧讓 `resolve()` 和 `unregister()` 競爭
4. 透過懸指標實現任意讀寫

修復此問題需要確保 check 和 use 的原子性，或使用世代計數器來偵測無效化。

---

## Resolution (2026-02-26)

**Outcome:** Fixed (Option 1).

Held `cross_thread_roots` (or orphan roots) lock during the entire resolve/try_resolve flow: check `contains_key(handle_id)` and `inc_ref` are now atomic. For `clone()`, moved `inc_ref` inside the lock in the TCB path; added `clone_orphan_root_with_inc_ref` for the orphan path. Added `heap::lock_orphan_roots()` for orphan table access.

