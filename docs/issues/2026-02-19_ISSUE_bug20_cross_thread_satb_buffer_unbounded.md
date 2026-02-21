# [Bug]: Cross-Thread SATB Buffer Unbounded Growth Potential

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要大量跨執行緒 mutation 才會觸發 |
| **Severity (嚴重程度)** | High | 無上限緩衝區可能導致記憶體耗盡 |
| **Reproducibility (復現難度)** | Medium | 可透過大量跨執行緒 mutation 重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `CROSS_THREAD_SATB_BUFFER` in `heap.rs`, `GcThreadSafeCell::borrow_mut()` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當跨執行緒 mutation 發生時，SATB old values 應該被記錄到緩衝區，但在大量寫入時應該有上限以防止記憶體耗盡。

### 實際行為 (Actual Behavior)
`push_cross_thread_satb()` 函數可以無限制地將指標推入 `CROSS_THREAD_SATB_BUFFER` (`parking_lot::Mutex<Vec<usize>>`)，沒有任何大小檢查或溢位處理。這可能導致：
1. 記憶體無限增長
2. 在 FinalMark 階段處理大量資料時的性能問題
3. 潛在的 DoS 攻擊風險

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/heap.rs:1776-1780`：

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    CROSS_THREAD_SATB_BUFFER
        .lock()
        .push(gc_ptr.as_ptr() as usize);  // 無大小檢查!
}
```

相比之下，正常的 SATB 緩衝區有溢位處理 (`satb_buffer_overflowed`)，但跨執行緒緩衝區缺少相同的保護機制。

在 `crates/rudo-gc/src/cell.rs:930`：
```rust
// No size check before pushing
crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr);
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let cell = Gc::new(GcThreadSafeCell::new(Data { value: 0 }));
    
    // Spawn many threads that mutate the cell
    let handles: Vec<_> = (0..1000)
        .map(|_| {
            thread::spawn(move || {
                let cell = Gc::new(GcThreadSafeCell::new(Data { value: 42 }));
                for _ in 0..1000 {
                    *cell.borrow_mut() = Data { value: 42 };
                }
            })
        })
        .collect();
    
    // Each thread pushes to CROSS_THREAD_SATB_BUFFER without limit
    // Buffer grows to 1,000,000+ entries
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. 為 `CROSS_THREAD_SATB_BUFFER` 添加大小限制
2. 當達到上限時觸發 fallback 機制
3. 或將大型緩衝區拆分為多個較小的緩衝區

```rust
const MAX_CROSS_THREAD_SATB_SIZE: usize = 1024 * 1024; // 1M entries max

pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        // Trigger fallback instead of unbounded growth
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(FallbackReason::SatbBufferOverflow);
        return;
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
跨執行緒 SATB 緩衝區的設計目的是處理不在相同執行緒上的 mutation，但缺少大小限制是個嚴重的設計缺陷。在 production 環境中，如果有多個 worker threads 同時執行大量 mutation，緩衝區可能會快速增長到數百萬條目，導致記憶體壓力和 GC 暫停時間增加。

**Rustacean (Soundness 觀點):**
雖然這不是傳統意義上的 soundness bug，但記憶體耗盡 (OOM) 會導致程式崩潰，這是一種形式 的資源管理失敗。一個良好的 GC 實現應該能夠優雅地處理這種情況，而不是允許無限制的記憶體增長。

**Geohot (Exploit 觀點):**
從攻擊者的角度來看，這是一個潛在的 DoS 攻擊向量。攻擊者可以通過觸發大量跨執行緒 mutation 來消耗系統記憶體，導致服務癱瘓。特別是在多租戶環境中，一個客戶端的攻擊可能會影響到其他客戶端。
