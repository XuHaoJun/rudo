# [Bug]: Origin 執行緒終止後 GcHandle 持有無效的 Arc<ThreadControlBlock> 導致記憶體洩露

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要跨執行緒使用 GcHandle，且 origin 執行緒終止 |
| **Severity (嚴重程度)** | Medium | 導致記憶體洩露，而非立即崩潰 |
| **Reproducibility (復現難度)** | Low | 需要特定的使用模式（執行緒終止 + GcHandle） |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle`, `WeakCrossThreadHandle`, `ThreadControlBlock`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當使用 `Gc::cross_thread_handle()` 創建跨執行緒 handle 後，如果 origin 執行緒終止，handle 仍會保持對 `Arc<ThreadControlBlock>` 的引用。這導致：

1. `ThreadControlBlock` 無法被釋放（因為 Arc 引用計數不歸零）
2. 與該 TCB 關聯的所有資源都無法釋放
3. 最終導致記憶體洩露

### 預期行為
- 當 origin 執行緒終止後，對應的 GcHandle 應該自動失效或被清理
- `ThreadControlBlock` 應該在沒有任何引用時被釋放

### 實際行為
1. 執行緒 A 創建 `GcHandle`，持有 `Arc<ThreadControlBlock>`
2. 執行緒 A 終止
3. `GcHandle` 仍然有效（在其他執行緒上可訪問）
4. `Arc<ThreadControlBlock>` 引用計數不歸零（因為 GcHandle 持有 Arc）
5. TCB 無法被釋放 → 記憶體洩露

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs` 中：

```rust
pub struct GcHandle<T: Trace + 'static> {
    pub(crate) ptr: NonNull<GcBox<T>>,
    pub(crate) origin_tcb: Arc<ThreadControlBlock>,  // 問題在這裡
    pub(crate) origin_thread: ThreadId,
    pub(crate) handle_id: HandleId,
}
```

問題：
1. `GcHandle` 持有 `Arc<ThreadControlBlock>`
2. 當 origin 執行緒終止時，沒有機制通知 GcHandle
3. `ThreadControlBlock` 沒有 `Drop` 實作來清理關聯的 handles
4. GcHandle 可以存在於 origin 執行緒的生命週期之外

`ThreadControlBlock` 的定義（heap.rs:151-191）顯示它沒有實作 `Drop`，因此無法自動清理。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let tcb_ref_count = Arc::new(AtomicUsize::new(0));
    
    let handle = thread::spawn(|| {
        let gc = Gc::new(Data { value: 42 });
        let handle = gc.cross_thread_handle();
        
        // 追蹤 TCB 引用計數
        let tcb = handle.origin_tcb.clone();
        // Arc<ThreadControlBlock> 引用計數應該為 2（原始 + 複製）
        
        (handle, tcb)
    });
    
    let (handle, _tcb) = handle.join().unwrap();
    
    // Origin 執行緒已終止，但 handle 仍持有 origin_tcb
    // 這導致 ThreadControlBlock 無法被釋放
    
    // 可以繼續使用 handle
    let gc = handle.resolve().unwrap();
    println!("{}", gc.value);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：為 ThreadControlBlock 實作 Drop（推薦）
在 `ThreadControlBlock` 中實作 `Drop`，清理所有關聯的 handles：

```rust
impl Drop for ThreadControlBlock {
    fn drop(&mut self) {
        // 清理 cross_thread_roots
        let mut roots = self.cross_thread_roots.lock().unwrap();
        roots.strong.clear();
        // 清理其他資源
    }
}
```

### 方案 2：在 GcHandle 中添加生命週期追蹤
追蹤 origin 執行緒的狀態，當執行緒終止時自動使 handle 失效。

### 方案 3：使用 WeakArc 替代 Arc
使用 `Weak<ThreadControlBlock>`，這樣當沒有強引用時，TCB 可以被釋放：

```rust
pub struct GcHandle<T: Trace + 'static> {
    // ...
    pub(crate) origin_tcb: Weak<ThreadControlBlock>,
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題反映了執行緒本地資源與跨執行緒生命週期管理的複雜性。在傳統 GC 中，所有執行緒共享 heap，但 rudo-gc 的執行緒本地分配模型需要更謹慎地處理執行緒終止時的資源釋放。

**Rustacean (Soundness 觀點):**
這是記憶體管理問題而非安全性問題（不會導致 UB），但記憶體洩露仍然是嚴重的問題。建議使用 RAII 模式確保資源正確釋放。

**Geohot (Exploit 觀點):**
雖然不會直接導致漏洞，但長期執行的程式可能因為記憶體洩露而被耗盡資源。攻擊者可以通過構造大量 GcHandle 來加速記憶體洩露，導致 DoS。

---

## Resolution (2026-02-21)

**Fixed** via 方案 3 (Weak + root migration): `GcHandle` and `WeakCrossThreadHandle` now hold `Weak<ThreadControlBlock>`. When the origin thread terminates, roots are migrated to a global orphan table in `ThreadLocalHeap::drop`; the TCB is dropped when ref count reaches 0. Handles falling back to the orphan table for unregister/clone/drop. GC marking extended to scan orphan roots.
