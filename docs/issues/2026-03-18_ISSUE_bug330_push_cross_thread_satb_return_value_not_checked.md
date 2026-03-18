# [Bug]: GcThreadSafeCell::borrow_mut 與 record_satb_old_values_with_state 未檢查 push_cross_thread_satb 返回值

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在沒有 GC heap 的執行緒中使用 GcThreadSafeCell |
| **Severity (嚴重程度)** | Medium | 可能導致 SATB 不變性破壞，但 incremental marking 有 fallback 機制 |
| **Reproducibility (重現難度)** | Low | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut()` in `cell.rs:1087`, `record_satb_old_values_with_state()` in `sync.rs:100`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

當前執行緒沒有 GC heap 時，`GcThreadSafeCell::borrow_mut()` 與 `record_satb_old_values_with_state()` 會使用跨執行緒 SATB buffer (`push_cross_thread_satb`) 來記錄舊指標。然而，這兩個函數都沒有檢查 `push_cross_thread_satb` 的返回值。

### 預期行為

當 `push_cross_thread_satb` 返回 `false` 時（表示 buffer 滿了，需要 fallback），應該呼叫 `request_fallback`。

### 實際行為

在 `cell.rs:1087` 和 `sync.rs:100`，程式碼忽略 `push_cross_thread_satb` 的返回值，導致即使 buffer 滿了也不會請求 fallback。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs:1087`:
```rust
} else {
    // No GC heap on this thread, use cross-thread buffer
    for gc_ptr in gc_ptrs {
        crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr); // BUG: return value not checked!
    }
}
```

在 `sync.rs:100`:
```rust
} else {
    for gc_ptr in gc_ptrs {
        crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr); // BUG: return value not checked!
    }
}
```

`push_cross_thread_satb` 返回 `bool`：`true` = 成功存儲，`false` = 請求 fallback。當返回 `false` 時應該呼叫 `IncrementalMarkState::global().request_fallback(FallbackReason::SatbBufferOverflow)`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要滿足以下條件：
1. 在沒有 GC heap 的執行緒中呼叫 `GcThreadSafeCell::borrow_mut()` 或使用 `GcRwLock::write()`
2. 跨執行緒 SATB buffer 要嘛滿，要嘛 overflow buffer 也要滿
3. 驗證 fallback 是否被請求

理論上的 PoC:
```rust
// 需要多執行緒環境
// Thread A: 不斷調用 GcThreadSafeCell::borrow_mut() 填充 cross-thread SATB buffer
// Thread B: 嘗試在無 heap 環境下調用 borrow_mut
// 驗證 fallback 是否正確請求
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cell.rs:1087` 和 `sync.rs:100` 檢查返回值：

```rust
} else {
    // No GC heap on this thread, use cross-thread buffer
    for gc_ptr in gc_ptrs {
        if !crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr) {
            crate::gc::incremental::IncrementalMarkState::global()
                .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這會導致 SATB 不變性在跨執行緒情境下被破壞。當 cross-thread buffer 滿時，應該觸發 fallback 機制切換到 STW 完整標記，但目前的實作會忽略這個信號。

**Rustacean (Soundness 觀點):**
這不是傳統的 UB，但可能導致記憶體管理邏輯錯誤。如果 SATB 舊值沒有正確記錄，在增量標記期間可能會遺漏需要保留的物件。

**Geohot (Exploit 觀點):**
在极端情况下，SATB 不變性破坏可能导致可达对象被错误回收。但这需要非常精确的时序控制，实际利用难度较高。

---

## ✅ 修復記錄 (Fix Record)

- **Date:** 
- **Fix:**

---

## 🔍 驗證記錄 (Verification)

已確認 bug 存在於兩處：
1. `cell.rs:1087` - GcThreadSafeCell::borrow_mut
2. `sync.rs:100` - record_satb_old_values_with_state

兩處都沒有檢查 `push_cross_thread_satb` 的返回值。
