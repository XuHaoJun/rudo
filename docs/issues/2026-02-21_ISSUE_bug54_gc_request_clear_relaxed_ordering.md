# [Bug]: GC Request Clear 使用 Relaxed Ordering 導致執行緒可能錯過 GC 完成信號

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 多執行緒場景下可能發生 |
| **Severity (嚴重程度)** | High | 執行緒可能無限期等待已完成的 GC |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap.rs` - GC request clear mechanism (`resume_all_threads`, `clear_gc_request`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 GC 完成時，`resume_all_threads()` 和 `clear_gc_request()` 清除 `gc_requested` flag 後，等待中的執行緒應該能夠看到 `false` 值並繼續執行。

### 實際行為 (Actual Behavior)
`resume_all_threads()` (line 564, 579) 和 `clear_gc_request()` (line 662, 665) 使用 `Ordering::Relaxed` 儲存 `false` 到 GC request flags，但等待中的執行緒使用 `Ordering::Acquire` 載入此 flag。

使用 `Relaxed` ordering 清除 flag 可能導致等待執行緒無法看到 GC 完成的信號，造成執行緒無限期等待或錯過繼續執行的時機。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼
**`heap.rs:559-580` - `resume_all_threads` 函數：**
```rust
pub fn resume_all_threads() {
    let registry = thread_registry().lock().unwrap();
    let mut woken_count = 0;
    for tcb in &registry.threads {
        if tcb.state.load(Ordering::Acquire) == THREAD_STATE_SAFEPOINT {
            tcb.gc_requested.store(false, Ordering::Relaxed);  // <-- BUG
            tcb.park_cond.notify_all();
            // ...
        }
    }
    // ...
    GC_REQUESTED.store(false, Ordering::Relaxed);  // <-- BUG
}
```

**`heap.rs:659-666` - `clear_gc_request` 函數：**
```rust
pub fn clear_gc_request() {
    let registry = thread_registry().lock().unwrap();
    for tcb in &registry.threads {
        tcb.gc_requested.store(false, Ordering::Relaxed);  // <-- BUG
    }
    drop(registry);
    GC_REQUESTED.store(false, Ordering::Relaxed);  // <-- BUG
}
```

### 等待執行緒使用 Acquire ordering
**`heap.rs:541`：**
```rust
while tcb.gc_requested.load(Ordering::Acquire) {  // <-- Uses Acquire
    guard = tcb.park_cond.wait(guard).unwrap();
}
```

### 邏輯缺陷

1. 當 GC 完成時，執行緒使用 `Relaxed` ordering 清除 `gc_requested = false`
2. 等待中的執行緒使用 `Acquire` ordering 載入這個 flag
3. 由於 `Relaxed` store 可能對 `Acquire` load 不可見，導致：
   - 執行緒可能繼續等待已完成的 GC
   - 可能導致死鎖或效能問題
   - CPU 快取不一致可能導致執行緒錯過通知

### 與 bug30 的關係

bug30 報告了 `request_gc` 函數中儲存 `true` 時使用 `Relaxed` ordering 的問題。本 bug 是互補的：儲存 `true` 的問題在 bug30 中，而儲存 `false`（清除）的問題在本文檔中。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC 概念：需要多執行緒並發
// 1. 執行緒 A 呼叫 request_gc()，所有執行緒進入 safepoint 等待
// 2. GC 完成，呼叫 resume_all_threads() 使用 Relaxed store 清除 flag
// 3. 執行緒 B 正在載入 gc_requested (Acquire)
// 4. 由於 Relaxed ordering，執行緒 B 可能看不到 store (false)
// 5. 執行緒 B 繼續等待，可能導致死鎖

// 實際觸發條件需要：
// - 多執行緒並發
// - CPU 執行緒遷移導致 cache 未同步
// - 在 store 和 load 之間的 timing
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `Ordering::Relaxed` 改為 `Ordering::Release`：

```rust
// heap.rs:564
tcb.gc_requested.store(false, Ordering::Release);

// heap.rs:579
GC_REQUESTED.store(false, Ordering::Release);

// heap.rs:662
tcb.gc_requested.store(false, Ordering::Release);

// heap.rs:665
GC_REQUESTED.store(false, Ordering::Release);
```

使用 `Release` ordering 可以確保：
1. 清除 flag 前的所有記憶體操作對看到 flag 為 false 的執行緒可見
2. 與等待執行緒的 `Acquire` load 正確同步
3. 符合 memory model 的 producer-consumer 模式

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 的 stop-the-world 機制中，恢復執行緒執行必須確保所有執行緒都能看到 GC 完成的信號。使用 `Relaxed` ordering 清除 flag 違反了這個基本原則。這可能導致執行緒無限期等待，類似於死鎖。與 bug30（請求 GC 時的 ordering 問題）互補，兩者都需要修復才能確保 GC handshake 協議的正確性。

**Rustacean (Soundness 觀點):**
這不是傳統意義的 UB，但屬於並發邏輯錯誤。`Relaxed` ordering 在此上下文提供了錯誤的保證。根據 Rust atomic 模型的 producer-consumer 模式，生產者（清除 flag）應該使用 `Release`，消費者（等待 flag）應該使用 `Acquire`。

**Geohot (Exploit 攻擊觀點):**
雖然這是 GC 內部機制，但攻擊者可能利用此漏洞：
- 導致 GC 執行緒無法恢復執行
- 造成程式無回應（類似 DoS）
- 在極端情況下可能與其他 bug 組合導致記憶體腐敗

---

## Resolution

heap.rs 中 `resume_all_threads()` 與 `clear_gc_request()` 已改為使用 `Ordering::Release` 清除 `gc_requested` 與 `GC_REQUESTED` 標誌，與 mutator 執行緒的 `Acquire` load 正確配對。
