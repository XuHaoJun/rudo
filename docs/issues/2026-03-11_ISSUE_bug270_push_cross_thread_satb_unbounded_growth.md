# [Bug]: push_cross_thread_satb 緩衝區溢位時仍然 push 導致無上限 growth

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 任何大量跨執行緒 mutation 都會觸發 |
| **Severity (嚴重程度)** | High | 無上限緩衝區導致記憶體耗盡 |
| **Reproducibility (重現難度)** | Very Low | 每次都會觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::push_cross_thread_satb()` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為
當 `CROSS_THREAD_SATB_BUFFER` 超過 `MAX_CROSS_THREAD_SATB_SIZE` (1M 條目) 時：
1. 請求 fallback
2. **返回** (不繼續 push) 以防止無上限 growth

### 實際行為
請求 fallback 後仍然 push 條目，導致緩衝區無上限增長。

**與 bug20/bug268 的關係：**
- bug20 原本報告無上限 growth 問題，標記為 "Fixed"
- bug20 的 Resolution 說明應該添加 `return;` 來防止 growth
- bug268 錯誤地描述為「有 return 導致丟失舊值」
- **實際程式碼**：沒有 return，所以 bug20 的 fix 從未被正確 apply

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs:1963-1970`：

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        // BUG: 缺少 return; 導致無上限 growth!
    }
    buffer.push(gc_ptr.as_ptr() as usize);  // 永遠會執行
}
```

bug20 的 Resolution 說明建議的修復：
```rust
if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
    crate::gc::incremental::IncrementalMarkState::global()
        .request_fallback(FallbackReason::SatbBufferOverflow);
    return;  // <-- 這個 return 從未被添加到程式碼!
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // 建立跨執行緒 GC 指針
    let cell = Gc::new(GcThreadSafeCell::new(Data { value: 0 }));
    
    // 大量跨執行緒 mutation
    let handles: Vec<_> = (0..100)
        .map(|_| {
            thread::spawn(move || {
                for _ in 0..10000 {
                    *cell.borrow_mut() = Data { value: 42 };
                }
            })
        })
        .collect();
    
    // 緩衝區會持續增長超過 1M 條目
    for h in handles { h.join().unwrap(); }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        return;  // 添加 return 防止無上限 growth
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug20 的回歸 bug。原始 bug 報告了無上限 growth 問題，fix 被記錄但從未被正確 apply 到程式碼。增量標記需要緩衝區有上限來確保 GC 暫停時間可預測。

**Rustacean (Soundness 觀點):**
這是資源管理問題。虽然不是传统意义上的 memory safety bug，但無限制的記憶體增長會導致 OOM，程式崩潰。

**Geohot (Exploit 觀點):**
這可以被利用來進行 DoS 攻擊 - 通過觸發大量跨執行緒 mutation 來消耗系統記憶體。

---

## ✅ 驗證記錄 (Verification Record)

### 2026-03-11 驗證
- **驗證結果**: Bug 存在於目前程式碼中
- **程式碼位置**: `crates/rudo-gc/src/heap.rs:1963-1970`
- **確認事項**:
  - 當緩衝區滿時，請求 fallback (line 1966-1967)
  - **但沒有 return**，所以 line 1969 永遠會執行
  - 這與 bug20 的 fix 說明不符
- **影響**: 緩衝區無上限生長，導致記憶體耗盡
