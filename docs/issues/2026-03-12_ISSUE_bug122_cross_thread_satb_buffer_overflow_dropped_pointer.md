# [Bug]: Cross-Thread SATB Buffer Overflow 丟失指標導致潛在 Premature Collection

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要大量跨執行緒 mutation 且 buffer 溢出才會觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致物件被過早回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要並發 PoC，單執行緒無法穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::push_cross_thread_satb()` in `heap.rs:1963-1971`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當跨執行緒 SATB buffer 溢出時，應該透過某種機制保留該指標（例如記錄到溢出緩衝區或觸發 fallback 前先保存），避免在 fallback 完成前物件被錯誤回收。

### 實際行為 (Actual Behavior)
`push_cross_thread_satb()` 在 buffer 達到上限時，僅請求 fallback 但直接 **丟棄指標**：
```rust
if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
    crate::gc::incremental::IncrementalMarkState::global()
        .request_fallback(FallbackReason::SatbBufferOverflow);
    return; // <-- 指標在此丟失！
}
```

這創建了一個 race window：請求 fallback 到 fallback 完成之間，指標未受保護，可能被過早回收。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/heap.rs:1963-1971`：

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        return;  // <-- BUG: 指標被丟棄，未記錄！
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

bug20 修復了 unbounded growth 問題（新增 MAX_CROSS_THREAD_SATB_SIZE），但修復時只加入了大小檢查和 fallback 請求，**忽略了 fallback 完成前的 race window**。

當 buffer 溢出時：
1. 指標未被加入任何 buffer
2. 請求 fallback（最終會觸發 full STW collection）
3. **Race window**: 在 fallback 完成前，若此指標是物件的唯一引用，物件可能被過早回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要並發環境才能可靠重現（單執行緒無法觸發 race condition）：

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct Data {
    value: usize,
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn main() {
    // 啟用增量標記
    rudo_gc::gc::set_incremental_config(
        rudo_gc::gc::IncrementalConfig {
            enabled: true,
            dirty_pages_threshold: 10,
            mark_slice_duration_ms: 1,
        }
    );

    // 建立 OLD->YOUNG 引用所需的場景
    // 需要多執行緒並發才能可靠觸發 race
    
    let handles: Vec<_> = (0..100)
        .map(|_| {
            thread::spawn(move || {
                for _ in 0..10000 {
                    // 創建跨執行緒 SATB 記錄
                    // 大量寫入會觸發 buffer 溢出
                    let cell = Gc::new(GcThreadSafeCell::new(Data { value: 0 }));
                    for _ in 0..1000 {
                        *cell.borrow_mut() = Data { value: COUNTER.fetch_add(1, Ordering::Relaxed) };
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}
```

**注意**: 根據 Pattern 2，需要 ThreadSanitizer 或 Miri 才能可靠檢測此 race condition。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

方案 1：在 fallback 完成前保留指標到溢出緩衝區
```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        // 記錄到溢出緩衝區，而非丟棄
        OVERFLOW_BUFFER.lock().push(gc_ptr.as_ptr() as usize);
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        return;
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

方案 2：確保 fallback 迅速完成（降低 race window）
- 這需要審視 fallback 機制的延遲時間

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug20 修復了 unbounded growth，但修復時未考慮 fallback 完成前的 race condition。SATB 不變性要求所有 OLD->YOUNG 引用的舊值都被記錄。當 buffer 溢出且指標被丟棄時，物件可能在標記前被回收，違反 SATB 不變性。

**Rustacean (Soundness 觀點):**
這是潛在的 use-after-free 問題。若物件被過早回收且後續存取，將觸發未定義行為。雖然 fallback 最終會保護大多數物件（透過 full trace），但在 race window 內存取的物件可能受影響。

**Geohot (Exploit 攻擊觀點):**
此 race condition 可被利用：攻擊者可通過觸發大量跨執行緒 mutation 填充 buffer，製造 race window，然後嘗試存取可能被過早回收的物件，實現記憶體佈局控制或資訊洩漏。

---

## Resolution (2026-03-13)

**Fix applied:** Added `CROSS_THREAD_SATB_OVERFLOW_BUFFER` in `heap.rs`. When the main cross-thread SATB buffer is full, pointers are now pushed to the overflow buffer instead of being dropped. `flush_cross_thread_satb_buffer()` drains both main and overflow buffers during `execute_final_mark`, ensuring all recorded pointers are marked before sweep. Mirrors the per-thread `satb_overflow_buffer` pattern.

---