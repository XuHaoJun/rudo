# [Bug]: IncrementalMarkState::enabled 使用 Relaxed ordering 導致 write_barrier_needed 可能返回過時值

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒並發，且需要精確時序才能觸發 |
| **Severity (嚴重程度)** | High | 可能導致 write barrier 失效，進而導致年輕物件被錯誤回收 |
| **Reproducibility (復現難度)** | High | 需要 Miri 或 ThreadSanitizer 才能穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `IncrementalMarkState`, `write_barrier_needed`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`IncrementalMarkState::enabled` 欄位使用 `Ordering::Relaxed` 進行載入，可能導致 `write_barrier_needed()` 函數讀取到過時的值。這會導致：

1. 當 incremental marking 剛啟用時，某些執行緒可能仍看到 `enabled = false`
2. 當 incremental marking 被禁用時，某些執行緒可能仍看到 `enabled = true`

這與 bug54（GC_REQUESTED 使用 Relaxed ordering）問題類似，但是是針對不同的 flag。

### 預期行為 (Expected Behavior)

`write_barrier_needed()` 應該總是返回正確的 barrier 狀態，無論其他執行緒何時修改 `enabled` 欄位。

### 實際行為 (Actual Behavior)

由於使用 `Ordering::Relaxed`，執行緒可能讀取到過時的 `enabled` 值，導致：
- 應該觸發 barrier 時卻沒有觸發
- 不應該觸發 barrier 時卻觸發了

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題程式碼位於 `crates/rudo-gc/src/gc/incremental.rs`:

```rust
// Line 433-435
pub fn is_enabled(&self) -> bool {
    self.enabled.load(Ordering::Relaxed)  // <-- BUG: 應該使用更強的 ordering
}

// Line 491-496
pub fn write_barrier_needed() -> bool {
    let state = IncrementalMarkState::global();
    state.enabled.load(Ordering::Relaxed)  // <-- BUG: Relaxed 讀取可能返回過時值
        && !state.fallback_requested()
        && is_write_barrier_active()
}
```

`Ordering::Relaxed` 只保證原子性，不保證跨執行緒的可見性。這意味著：
- 當執行緒 A 寫入 `enabled = true`
- 執行緒 B 可能仍看到 `enabled = false`（過時值）

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要使用 ThreadSanitizer 或 Miri 來檢測這個 data race：

```rust
// 需要 TSan 才能穩定復現
fn test_incremental_enabled_relaxed_race() {
    use std::thread;
    use std::sync::atomic::{AtomicBool, Ordering};
    
    // 啟用 incremental marking
    set_incremental_config(IncrementalConfig::default().with_enabled(true));
    
    let barrier = AtomicBool::new(false);
    
    // 多個執行緒同時修改和讀取 enabled 狀態
    let handles: Vec<_> = (0..4).map(|i| {
        thread::spawn(move || {
            if i % 2 == 0 {
                // 啟用 barrier
                set_incremental_config(IncrementalConfig::default().with_enabled(true));
            } else {
                // 禁用 barrier  
                set_incremental_config(IncrementalConfig::default().with_enabled(false));
            }
            barrier.store(true, Ordering::SeqCst);
            
            // 旋轉直到所有執行緒就緒
            while !barrier.load(Ordering::SeqCst) {}
            
            // 讀取 enabled 狀態
            let is_enabled = IncrementalMarkState::global().is_enabled();
            let barrier_needed = write_barrier_needed();
            
            // 驗證一致性（Relaxed 可能導致不一致）
            println!("is_enabled={}, barrier_needed={}", is_enabled, barrier_needed);
        })
    }).collect();
    
    for h in handles {
        h.join().unwrap();
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `enabled` 欄位的載入ordering 從 `Relaxed` 改為 `Acquire`：

```rust
// Line 433-435
pub fn is_enabled(&self) -> bool {
    self.enabled.load(Ordering::Acquire)  // 使用 Acquire 確保讀取最新值
}

// Line 439
self.enabled.store(config.enabled, Ordering::Release);  // 使用 Release 確保寫入被立即看到
```

類似的修改應該應用於 `write_barrier_needed()`:

```rust
pub fn write_barrier_needed() -> bool {
    let state = IncrementalMarkState::global();
    state.enabled.load(Ordering::Acquire)  // 使用 Acquire
        && !state.fallback_requested()
        && is_write_barrier_active()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個問題會影響 incremental marking 的正確性。如果 write barrier 在不該觸發時觸發浪費效能；但如果應該觸發時沒觸發，會導致年輕物件被錯誤回收（use-after-free）。Relaxed ordering 在高並發場景下特別危險。

**Rustacean (Soundness 觀點):**
這不是 UB（因為 `AtomicBool` 的Relaxed 讀取仍是安全的），但會導致邏輯錯誤。與 bug54（GC_REQUESTED）類似，使用 Relaxed ordering 來做控制流決策是不安全的。

**Geohot (Exploit 觀點):**
在即時通訊或高效能運算等高並發場景，這個 race 可能被利用來觸發物件過早回收，進而導致 use-after-free。雖然需要精確時序，但理論上是可利用的。

---

## Resolution (2026-03-01)

**Outcome:** Fixed.

Three changes applied to `crates/rudo-gc/src/gc/incremental.rs`:

1. `is_enabled()` (line 434): `Relaxed` → `Acquire` — ensures threads observe the latest `enabled` value before gating on it.
2. `set_config()` (line 439): `Relaxed` → `Release` — ensures the store is visible to all threads that subsequently load with `Acquire`.
3. `write_barrier_needed()` (line 493): replaced the direct `state.enabled.load(Ordering::Relaxed)` with `state.is_enabled()`, which now uses `Acquire`.

The `Acquire`/`Release` pair establishes a happens-before edge between the config setter and any thread reading `enabled` to decide whether to fire a write barrier. All tests pass.
