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
- **Component:** `GcHandle<T>::drop()` and `GcHandle::unregister()`
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

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/handles/cross_thread.rs:380-394` (Drop impl)

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

**相同問題也存在于：** `unregister()` 方法 (lines 122-135)

**根本原因：**
1. `handle_id` 是 plain `u64` (非 `AtomicU64`)
2. 檢查 `handle_id == HandleId::INVALID` 與設定 `handle_id = HandleId::INVALID` 不是原子操作
3. 缺少 compare-and-swap 機制來確保只有一個執行緒執行 cleanup

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 理論 PoC - 需要精確時序控制
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn poc_double_dec_ref() {
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // Clone handle 到多個執行緒
    let handles: Vec<_> = (0..4)
        .map(|_| handle.clone())
        .collect();
    
    // 同時 drop 這些 handles
    let threads: Vec<_> = handles
        .into_iter()
        .map(|h| {
            thread::spawn(move || {
                drop(h);  // 多個執行緒同時 drop
            })
        })
        .collect();
    
    for t in threads {
        t.join().unwrap();
    }
    // 可能發生: use-after-free, memory corruption, 或 double free
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 使用 AtomicU64 + compare_exchange

將 `handle_id` 改為 `AtomicU64`，使用 `compare_exchange` 確保只有一個執行緒執行 cleanup：

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        // 嘗試 atomic 將 handle_id 從有效值改為 INVALID
        let old_id = self.handle_id.load(Ordering::AcqRel);
        if old_id == HandleId::INVALID {
            return;
        }
        
        let expected = old_id;
        let new_id = HandleId::INVALID;
        
        // 只有一個執行緒能成功將 handle_id 設為 INVALID
        if self.handle_id.compare_exchange(expected, new_id, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return; // 其他執行緒已經處理了
        }
        
        // 現在安全地執行 cleanup
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.remove(&old_id);
            drop(roots);
        } else {
            let _ = heap::remove_orphan_root(self.origin_thread, old_id);
        }
        
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
    }
}
```

### 方案 2: 使用 Mutex 保護 (簡單但有效能影響)

在 drop 期間獲取鎖，防止並發 drop：

```rust
// 需要修改 GcHandle 結構添加額外的 mutex
static DROP_LOCK: Mutex<HashMap<ThreadId, HandleId>> = Mutex::new(HashMap::new());
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 double dec_ref 問題在 GC 實現中是嚴重的。在 reference counting GC 中，重複遞減引用計數會導致：
1. 物件被過早釋放
2. 記憶體佈局被破壞
3. 其他執行緒可能訪問已釋放的記憶體

正確的做法是使用 compare-and-swap 確保只有一個執行緒執行 cleanup邏輯。

**Rustacean (Soundness 觀點):**
這是明確的 memory safety 問題：
- double dec_ref 可能導致 ref_count 變成負數或 0
- 當 ref_count 為 0 時，物件被釋放，後續的 dec_ref 訪問已釋放的記憶體
- 這是典型的 use-after-free

**Geohot (Exploit 攻擊觀點):**
雖然精確時序很難控制，但攻擊者可以：
1. 使用 thread priority 影響執行緒調度
2. 使用 busy loop 增加競爭視窗
3. 在即時系統或有嚴格時序控制的環境中更容易觸發

---

## 相關 Issue

- bug103: GcHandle inc_ref TOCTOU race (類似模式但影響不同操作)
- bug29: GcHandle clone/unregister race (已被修復)
- bug72: GcHandle resolve unregistered handle UB (提及 Drop early return)

---

## Resolution (2026-03-03)

**Outcome:** Invalid — misidentified.

The issue assumes that when multiple `GcHandle` clones drop concurrently, they share the same `handle_id` and would race, causing double `dec_ref`. This is incorrect.

**Design:** Each `GcHandle` clone receives a **unique** `handle_id` via `roots.allocate_id()` (TCB path) or `heap::allocate_orphan_handle_id()` (orphan path). Each clone holds one ref and has its own root entry. When N clones drop, each removes its own `handle_id` from the root table and calls `dec_ref` once — correctly, N times for N refs.

**Verification:** Added `test_bug185_concurrent_drop_of_clones` in `tests/bug4_tcb_leak.rs`: 8 clones dropped concurrently from 8 threads via `Barrier`. Test passes; no double dec_ref, no crash.
