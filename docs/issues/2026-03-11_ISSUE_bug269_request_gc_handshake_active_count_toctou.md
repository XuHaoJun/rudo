# [Bug]: request_gc_handshake active_count load 有 TOCTOU race condition

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | 需要在 request_gc_handshake 載入 active_count 的瞬間有新執行緒註冊 |
| **Severity (嚴重程度)** | Medium | 可能導致 GC 行為不正確或效能問題 |
| **Reproducibility (復現難度)** | Very High | Race condition 極難穩定重現，需要精確時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** GC handshake - `request_gc_handshake()` in heap.rs
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`request_gc_handshake()` 函數在載入 `active_count` 時存在 TOCTOU (Time-Of-Check-Time-Of-Use) race condition。

### 預期行為
當呼叫 `request_gc_handshake()` 時，應該準確判斷是否為單執行緒環境，以決定執行單執行或多執行緒 GC 收集。

### 實際行為
`active_count` 在 registry lock 之外載入，導致可能載入過時的值。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/heap.rs:756-772` 的 `request_gc_handshake()` 函數中：

```rust
pub fn request_gc_handshake() -> bool {
    let registry = thread_registry().lock().unwrap();

    // Set GC_REQUESTED flag first
    GC_REQUESTED.store(true, Ordering::Release);

    // Set per-thread gc_requested flag for all threads
    for tcb in &registry.threads {
        tcb.gc_requested.store(true, Ordering::Release);
    }

    let active = registry.active_count.load(Ordering::Acquire);  // BUG: lock released!
    drop(registry);

    active == 1
}
```

問題在於 `active_count.load()` (line 768) 發生在 `drop(registry)` (line 769) 之前，但載入動作本身是在 lock 之外執行的。

**Race Condition 場景：**
1. Thread A 呼叫 `request_gc_handshake()`
2. Thread A 取得 registry lock
3. Thread A 設定 GC_REQUESTED = true
4. Thread A 對現有執行緒設定 gc_requested = true
5. Thread A 載入 active_count = 1 (line 768)
6. **在 line 768 和 769 之間**，Thread B 開始 spawn
7. Thread B 檢查 GC_REQUESTED (為 true)，設定自己的 gc_requested = true
8. Thread B 註冊到 registry (active_count 變成 2)
9. Thread A 釋放 lock
10. Thread A 檢查 active == 1，返回 true (錯誤！)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

由於 race condition 的時序極難控制，穩定重現困難。理論上：

```rust
// 理論上的觸發場景
let barrier = Arc::new(Barrier::new(2));

// Thread A: 觸發 GC
let t1 = thread::spawn(|| {
    barrier.wait();
    // 在這裡觸發 GC
    collect_full();
});

// Thread B: 同時 spawn 新執行緒
let t2 = thread::spawn(|| {
    barrier.wait();
    // 嘗試觸發新執行緒 spawn 時機
    let _gc = Gc::new(Data { value: 42 });
});

t1.join().unwrap();
t2.join().unwrap();
```

**注意：** 此 bug 需要極精確的時序才能穩定重現。實際影響可能有限，因為：
- Thread B 仍會看到 GC_REQUESTED = true 並設定自己的 gc_requested
- Thread B 最終會進入 safe point 參與 GC
- `get_all_thread_control_blocks()` 會包含 Thread B

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：將 active_count.load() 移到 lock 內
```rust
let active = {
    let registry = thread_registry().lock().unwrap();
    registry.active_count.load(Ordering::Acquire)
};
```

選項 2：忽略此 race condition，因為影響有限
- Thread B 仍會透過 GC_REQUESTED 參與 GC
- `get_all_thread_control_blocks()` 會包含所有執行緒

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這是一個經典的 TOCTOU race condition
- 雖然發生機率低，但可能導致 GC 行為不一致
- 影響相對有限，因為新執行緒仍會看到 GC_REQUESTED 並參與 GC

**Rustacean (Soundness 觀點):**
- 不是記憶體安全問題，不會導致 UAF
- 是邏輯錯誤，可能導致效能問題
- 不會造成 UB (undefined behavior)

**Geohot (Exploit 觀點):**
- 很難利用此 race condition
- 需要精確時序控制
- 實際安全影響極低
