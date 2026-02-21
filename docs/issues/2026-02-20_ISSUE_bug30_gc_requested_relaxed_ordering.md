# [Bug]: GC_REQUESTED Relaxed Ordering Causes Missed GC Handshake

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 多執行緒場景下可能發生，但需要精確的 timing |
| **Severity (嚴重程度)** | High | 可能導致 GC 完全失效，記憶體持續累積無法回收 |
| **Reproducibility (復現難度)** | Very High | 需要精確的執行時序控制，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap.rs` - GC request mechanism (`request_gc` function)
- **OS / Architecture:** All (Linux x86_64, etc.)
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

在 `request_gc` 函數中，`GC_REQUESTED` 和 per-thread `gc_requested` 使用 `Ordering::Relaxed` 儲存，但其他執行緒使用 `Ordering::Acquire` 載入。這導致記憶體順序錯誤，可能造成執行緒錯過 GC 請求。

### 預期行為 (Expected Behavior)
當 `request_gc` 設定 `gc_requested = true` 後，所有執行緒應該在下次載入此 flag 時看到 `true` 值，並參與 GC handshake 協議。

### 實際行為 (Actual Behavior)
由於使用 `Relaxed` ordering，執行緒可能看到 `gc_requested = false`（CPU pipeline 或編譯器優化可能重排），導致：
1. 執行緒錯過 GC 請求
2. GC 無法暫停所有執行緒
3. 記憶體持續累積無法回收

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/heap.rs`

```rust
// Line 593 - 錯誤：使用 Relaxed ordering
GC_REQUESTED.store(true, Ordering::Relaxed);

// Line 597 - 錯誤：使用 Relaxed ordering  
for tcb in &registry.threads {
    tcb.gc_requested.store(true, Ordering::Relaxed);
}
```

**對比正確用法：**
- Line 2952: `tcb.gc_requested.store(true, Ordering::Release);` ✓
- Line 653: `tcb.gc_requested.store(false, Ordering::Release);` ✓
- Line 689: `GC_REQUESTED.store(false, Ordering::SeqCst);` ✓

**為何這是 bug：**
1. `Relaxed` ordering 只保證原子性，不提供同步
2. 當執行緒在 line 509, 541 使用 `Acquire` 載入時，可能無法看到先前 Relaxed store 的結果
3. 根據 ARM/POWER 架構，Relaxed store 可能不會對其他執行緒可見

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC 概念：需要極端的執行時序
// 1. 執行緒 A 呼叫 request_gc()，使用 Relaxed store
// 2. 執行緒 B 正在載入 gc_requested (Acquire)
// 3. 由於 Relaxed ordering，執行緒 B 可能看不到 store

// 實際觸發條件需要：
// - 多執行緒並發
// - 執行緒 B 在 GC_REQUESTED store 之前就讀取
// - CPU 執行緒遷移導致 cache 未同步
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `Ordering::Relaxed` 改為 `Ordering::Release`：

```rust
// Line 593
GC_REQUESTED.store(true, Ordering::Release);

// Line 597
for tcb in &registry.threads {
    tcb.gc_requested.store(true, Ordering::Release);
}
```

這樣當其他執行緒使用 `Acquire` 載入時，能夠看到 store 之前的所有記憶體操作。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 系統中，stop-the-world 機制必須確保所有執行緒都能看到 GC 請求。使用 Relaxed ordering 違反了這個基本原則。在多執行緒環境下，這可能導致：
- 部分執行緒繼續執行並分配記憶體
- 標記階段看不到所有根
- 記憶體回收不完全

**Rustacean (Soundness 觀點):**
這不是傳統意義的 UB（因為 atomic 操作仍然正確），但屬於邏輯錯誤。Relaxed ordering 在此上下文提供了錯誤的保證。代碼的其他部分已經正確使用 Release ordering (line 2952, 653)，這表明是一致性問題。

**Geohot (Exploit 觀點):**
雖然這不是安全性漏洞（因為是 GC 內部機制），但如果攻擊者能控制執行時序，可能：
- 阻止 GC 執行
- 導致記憶體無限增長（DoS）
- 在極端情況下可能與其他 bug 組合造成記憶體腐敗

---

## Resolution

**2026-02-21** — Changed `Ordering::Relaxed` to `Ordering::Release` for GC handshake stores:

- **heap.rs** `request_gc_handshake()`: `GC_REQUESTED.store(true, Release)`, `tcb.gc_requested.store(true, Release)`
- **heap.rs** `resume_all_threads()`: `tcb.gc_requested.store(false, Release)`, `GC_REQUESTED.store(false, Release)`
- **heap.rs** `clear_gc_request()`: `tcb.gc_requested.store(false, Release)`, `GC_REQUESTED.store(false, Release)`
- **gc.rs** non-collector path: `GC_REQUESTED.store(false, Release)`

Mutator threads load these flags with `Acquire`; Release stores create the required synchronizes-with edges so all threads observe the GC request and clear.
