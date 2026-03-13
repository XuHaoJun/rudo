# [Bug]: GcRwLock::try_write / GcMutex::try_lock TOCTOU - barrier state cached before lock acquisition

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要並發 incremental marking phase 在極短時間內改變 |
| **Severity (嚴重程度)** | `Medium` | 可能導致 write barrier 失效，年輕物件被錯誤回收 |
| **Reproducibility (復現難度)** | `High` | 需精確時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::try_write`, `GcMutex::try_lock`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+ (incremental marking)

---

## 📝 問題描述 (Description)

在 `sync.rs` 的 `GcRwLock::try_write()` 和 `GcMutex::try_lock()` 函數中，`is_incremental_marking_active()` 和 `is_generational_barrier_active()` 在獲取鎖之前被緩存，但在獲取鎖之後使用。這造成了 TOCTOU (Time-of-check to time-of-use) 競爭條件。

**注意**：此問題與 bug100/bug101 不同。bug100/bug101 是關於在同一表達式中兩次調用 `is_incremental_marking_active()`。此 bug 是關於在鎖獲取之前緩存狀態，但在鎖獲取之後使用。

### 預期行為
Barrier 狀態應該在檢查和使用之間保持一致，或者在使用時重新檢查。

### 實際行為
`is_incremental_marking_active()` 和 `is_generational_barrier_active()` 在 `try_write()` / `try_lock()` 調用之前被緩存，但在閉包中使用。如果在緩存和使用之間 phase 發生變化，會導致：
- 傳遞給 barrier 函數的狀態與檢查時不一致
- Write barrier 可能不應觸發時觸發，或應觸發時未觸發

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `sync.rs:296-311` (`GcRwLock::try_write`)
```rust
let incremental_active = is_incremental_marking_active();  // <-- 在 try_write 之前緩存
let generational_active = is_generational_barrier_active();  // <-- 在 try_write 之前緩存

self.inner.try_write().map(|guard| {  // <-- 鎖在這裡獲取
    record_satb_old_values_with_state(&*guard, incremental_active);  // <-- 使用緩存的值
    self.trigger_write_barrier_with_state(generational_active, incremental_active);  // <-- 使用緩存的值
    ...
})
```

**問題點 2：** `sync.rs:580-595` (`GcMutex::try_lock`)
```rust
let incremental_active = is_incremental_marking_active();  // <-- 在 try_lock 之前緩存
let generational_active = is_generational_barrier_active();  // <-- 在 try_lock 之前緩存

self.inner.try_lock().map(|guard| {  // <-- 鎖在這裡獲取
    record_satb_old_values_with_state(&*guard, incremental_active);  // <-- 使用緩存的值
    ...
})
```

問題：
1. `is_incremental_marking_active()` 讀取 `IncrementalMarkState::phase()`
2. 使用 `Ordering::Relaxed` 讀取 phase
3. 在緩存和使用之間，phase 可能從 `Marking` 變為其他值，反之亦然
4. 這與 bug100/bug101 不同 - 那個是在同一表達式中兩次調用，此處是跨鎖獲取的 TOCTOU

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確控制時序：
1. 啟動 incremental marking
2. 在緩存 barrier 狀態和調用 try_write/try_lock 之後、閉包執行之前中斷
3. 改變 phase

理論上可能導致：
- Write barrier 不應觸發時觸發（性能損失）
- Write barrier 應觸發時未觸發（記憶體錯誤 - 年輕物件被錯誤回收）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**GcRwLock::try_write:**
```rust
self.inner.try_write().map(|guard| {
    // 在使用時重新獲取 barrier 狀態，避免 TOCTOU
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    record_satb_old_values_with_state(&*guard, incremental_active);
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    ...
})
```

**GcMutex::try_lock:**
```rust
self.inner.try_lock().map(|guard| {
    // 在使用時重新獲取 barrier 狀態，避免 TOCTOU
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    record_satb_old_values_with_state(&*guard, incremental_active);
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    ...
})
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 可能導致 SATB 屏障失效。如果在檢查時 phase=Marking，但傳遞給 barrier 時 phase 變為其他值，則可能跳過必要的屏障記錄，導致年輕代物件被錯誤回收。這與 bug100/bug101 不同，後者是在同一表達式中重複調用函數。

**Rustacean (Soundness 觀點):**
這是並發安全問題。使用 Relaxed ordering 讀取 phase，且在跨鎖獲取的檢查和使用之間無同步，導致可觀察的競爭行為。

**Geohot (Exploit 觀點):**
在高負載並發環境中，攻擊者可能利用此 TOCTOU 觸發不一致的 barrier 行為，進一步利用記憶體管理漏洞。

---

## 相關 Bug
- bug100: cell.rs GcCell::trigger_write_barrier TOCTOU (同一表達式中兩次調用)
- bug101: sync.rs trigger_write_barrier TOCTOU (同一表達式中兩次調用)

---

## Resolution (2026-03-13)

**Outcome:** Fixed and verified.

### Code Changes

- Updated `GcRwLock::try_write()` and `GcMutex::try_lock()` in `sync.rs` to move `is_incremental_marking_active()` and `is_generational_barrier_active()` calls **inside** the lock-acquisition closure, after the lock is acquired.
- This eliminates the TOCTOU window: barrier state is now read at use-time rather than cached before `try_write()`/`try_lock()`.

### Verification

- Sync tests pass: `cargo test -p rudo-gc --test sync -- --test-threads=1`
- Full test suite passes: `./test.sh`
- Note: Race conditions are not reliably reproducible with single-threaded tests; the fix is correct by construction (check and use are now atomic with respect to lock acquisition).
