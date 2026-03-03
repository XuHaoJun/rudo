# [Bug]: GcHandle::drop Double dec_ref Race Condition - 跨執行緒同時 Drop 導致引用計數錯誤

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒並發 drop 同一 handle 的 clone |
| **Severity (嚴重程度)** | Critical | 導致 double dec_ref，造成 use-after-free 或 memory corruption |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle<T>::drop()` in `handles/cross_thread.rs:380-394`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當多個 `GcHandle<T>` clone 同時從不同執行緒 drop 時，應該只有一個執行緒執行 `dec_ref`，避免重複釋放記憶體。

### 實際行為 (Actual Behavior)

`GcHandle::drop()` 實作中，`handle_id` 是非原子的 `u64` 欄位。當多個 clone 同時 drop 時：
1. Thread A: 檢查 `handle_id != HandleId::INVALID` → true，繼續執行
2. Thread B: 檢查 `handle_id != HandleId::INVALID` → true，繼續執行
3. Thread A: 從 root table 移除，設定 `handle_id = HandleId::INVALID`，呼叫 `dec_ref`
4. Thread B: 從 root table 移除（無效 key，靜默失敗），設定 `handle_id = HandleId::INVALID`，呼叫 `dec_ref`
5. **結果**：兩個執行緒都呼叫了 `dec_ref` → **double dec_ref** → use-after-free 或 memory corruption

### 程式碼位置

`handles/cross_thread.rs` 第 380-394 行 (`GcHandle::drop` 實作)：

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        if self.handle_id == HandleId::INVALID {  // <-- Race: 兩個執行緒可能都看到 != INVALID
            return;
        }
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.remove(&self.handle_id);  // 第一個執行緒成功移除，第二個執行緒靜默失敗
            drop(roots);
        } else {
            let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
        }
        self.handle_id = HandleId::INVALID;  // 兩個執行緒都會執行
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());  // 兩個執行緒都會呼叫!
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**根本原因：**
1. `handle_id` 是 plain `u64` (非 `AtomicU64`)
2. 檢查 `handle_id == HandleId::INVALID` 與設定 `handle_id = HandleId::INVALID` 不是原子操作
3. `roots.strong.remove()` 返回 `Option<V>`，但代碼忽略了返回值，導致第二個執行緒靜默失敗
4. 缺少 compare-and-swap 機制來確保只有一個執行緒執行 cleanup 和 dec_ref

**Race 條件分析：**
- 兩個 GcHandle clone 共享相同的 handle_id
- 當從不同執行緒同時 drop 時，兩個執行緒都可以通過 line 381 的檢查
- 第一個執行緒成功從 HashMap 移除 entry，第二個執行緒的 remove 靜默失敗（因為 key 已不存在）
- 兩個執行緒都會執行 line 393 的 dec_ref，導致 double dec_ref

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
    
    // 建立多個 clone
    let handle1 = handle.clone();
    let handle2 = handle.clone();
    let handle3 = handle.clone();
    
    // 儲存 clone 的 Arc 以防止 early drop
    let handles = vec![handle1, handle2, handle3];
    
    // 嘗試從不同執行緒同時 drop
    let handles_clone = Arc::new(handles);
    
    let mut threads = Vec::new();
    for _ in 0..3 {
        let hc = handles_clone.clone();
        threads.push(thread::spawn(move || {
            // 同時 drop 多個 handles
            for _ in 0..100 {
                // 快速創建和 drop
            }
            drop(hc);
        }));
    }
    
    for t in threads {
        t.join().unwrap();
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用 compare-and-swap (CAS) 來確保只有一個執行緒執行 cleanup：

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        // 使用 atomic swap 來確保只有一個執行緒執行 cleanup
        let prev_id = self.handle_id.swap(HandleId::INVALID, Ordering::AcqRel);
        if prev_id == HandleId::INVALID {
            return; // 已經被 drop 過
        }
        
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            // 只有成功移除的執行緒才執行 dec_ref
            if roots.strong.remove(&prev_id).is_some() {
                drop(roots);
                crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
            }
        } else {
            if heap::remove_orphan_root(self.origin_thread, prev_id).is_some() {
                crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
            }
        }
    }
}
```

關鍵修改：
1. 使用 `handle_id.swap()` 原子地交換值，確保只有一個執行緒獲得有效的 handle_id
2. 根據 `remove()` 的返回值決定是否執行 dec_ref
3. 使用 `AcqRel` ordering 確保記憶體順序正確

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的 reference counting race condition。在多執行緒環境下，多個 clone 同時 drop 必須確保只有一個執行緒執行 dec_ref。double dec_ref 會導致物件被錯誤地釋放，而另一個仍然有效的 Gc 指標會變成 use-after-free。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。double dec_ref 可能導致：
1. 如果物件記憶體已被重新分配：dereferencing 會讀取錯誤的資料
2. 如果物件記憶體尚未重新分配：可能讀取到已 drop 的資料

**Geohot (Exploit 攻擊觀點):**
這個 race condition 可以被利用。如果攻擊者能夠控制記憶體重新分配的時序，他們可能可以：
1. 構造 double free 場景
2. 控制物件記憶體內容
3. 通過 use-after-free 讀取敏感資料

---

## Resolution (2026-03-03)

**Outcome:** Invalid — duplicate of bug185, misidentified.

This issue is identical to [bug185](2026-03-03_ISSUE_bug185_gchandle_drop_double_dec_ref.md). The analysis assumes that when multiple `GcHandle` clones drop concurrently, they share the same `handle_id` and would race, causing double `dec_ref`. This is incorrect.

**Design:** Each `GcHandle` clone receives a **unique** `handle_id` via `roots.allocate_id()` (TCB path) or `heap::clone_orphan_root_with_inc_ref` (orphan path). Each clone holds one ref and has its own root entry. When N clones drop, each removes its own `handle_id` from the root table and calls `dec_ref` once — correctly, N times for N refs.

**Verification:** `test_bug185_concurrent_drop_of_clones` in `tests/bug4_tcb_leak.rs` exercises 8 clones dropped concurrently from 8 threads via `Barrier`. Test passes; no double dec_ref, no crash.
