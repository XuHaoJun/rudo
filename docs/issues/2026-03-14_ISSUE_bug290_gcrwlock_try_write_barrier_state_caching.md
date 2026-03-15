# [Bug]: GcRwLock::try_write caches barrier states AFTER lock acquisition, inconsistent with write()

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確的 GC phase 改變時序 |
| **Severity (嚴重程度)** | Medium | 可能導致 barrier 行為不一致 |
| **Reproducibility (重現難度)** | Medium | 需要多執行緒並發場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::try_write()` in `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`GcRwLock::try_write()` 應該與 `GcRwLock::write()` 一致，在獲取鎖之前緩存 barrier states，避免 TOCTOU 競爭條件。

### 實際行為
`GcRwLock::try_write()` 在 `.map()` closure 內部緩存 barrier states，這發生在鎖成功獲取之後。而 `GcRwLock::write()` 在獲取鎖之前緩存 barrier states。

這導致了不一致的行為：
- `write()`: 緩存狀態 → 獲取鎖 → 使用緩存的狀態
- `try_write()`: 嘗試獲取鎖 → 如果成功，緩存狀態 → 使用緩存的狀態

在 `try_write()` 中，GC phase 在鎖獲取和狀態緩存之間可能會改變，這會導致與 `write()` 不同的行為。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs` 中：

**write() (lines 281-288):**
```rust
let incremental_active = is_incremental_marking_active();  // 獲取鎖之前
let generational_active = is_generational_barrier_active(); // 獲取鎖之前

let guard = self.inner.write();  // 然後獲取鎖
record_satb_old_values_with_state(&*guard, incremental_active);
self.trigger_write_barrier_with_state(generational_active, incremental_active);
```

**try_write() (lines 323-334):**
```rust
self.inner.try_write().map(|guard| {
    let incremental_active = is_incremental_marking_active();  // 獲取鎖之後（在 map 內部）
    let generational_active = is_generational_barrier_active(); // 獲取鎖之後
    record_satb_old_values_with_state(&*guard, incremental_active);
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    ...
})
```

問題在於時序差異：
1. 在 `write()` 中，狀態在鎖獲取之前緩存，確保在整個操作過程中狀態一致
2. 在 `try_write()` 中，狀態在鎖獲取之後緩存，如果 GC phase 在這段時間內改變，會導致不一致的 barrier 行為

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制：
1. Thread A: 調用 `try_write()`，開始嘗試獲取鎖
2. Thread B: GC 發生，phase 改變
3. Thread A: 獲取鎖成功，讀取新的 barrier states（與 write() 不同的狀態）
4. 觀察到與 `write()` 不同的 barrier 行為

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `try_write()` 改為與 `write()` 一致的模式，在嘗試獲取鎖之前緩存 states：

```rust
pub fn try_write(&self) -> Option<GcRwLockWriteGuard<'_, T>>
where
    T: GcCapture,
{
    // 在嘗試獲取鎖之前緩存 barrier states
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    
    self.inner.try_write().map(|guard| {
        record_satb_old_values_with_state(&*guard, incremental_active);
        self.trigger_write_barrier_with_state(generational_active, incremental_active);
        let barrier_active = generational_active || incremental_active;
        mark_gc_ptrs_immediate(&*guard, barrier_active);
        GcRwLockWriteGuard {
            guard,
            _marker: PhantomData,
        }
    })
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Barrier states 緩存的目的是確保在整個 write 操作過程中狀態一致。在 `try_write()` 中，由於緩存發生在鎖獲取之後，如果 GC phase 在此期間改變，會導致這次 write 使用不同的 barrier 配置，與 `write()` 產生不一致的行為。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB，但可能導致不正確的 GC 行為。如果 barrier 沒有正確觸發，可能會導致物件被錯誤地回收。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠控制 GC timing，他們可能利用這個不一致性來繞過 barrier，導致記憶體回收問題。

---

## 🔗 相關 Issue

- bug110: Cache barrier states once to avoid TOCTOU (fixed for write(), but try_write() still inconsistent)

---

## 驗證記錄

**驗證日期:** 2026-03-14
**驗證人員:** opencode

### 驗證結果

確認 bug 存在：
- `write()` (lines 281-282): 緩存 barrier states 在獲取鎖之前
- `try_write()` (lines 324-325, 修復前): 緩存 barrier states 在獲取鎖之後（在 `.map()` closure 內部）

這導致 TOCTOU 競爭條件：GC phase 可能在鎖獲取和狀態緩存之間改變。

### 修復內容

在 `crates/rudo-gc/src/sync.rs` 的 `try_write()` 函數中：
- 將 barrier state 緩存移至 `.try_write()` 調用之前
- 現在與 `write()` 行為一致

修復位置：sync.rs lines 323-335

## 修復狀態

- [x] 已修復
- [ ] 未修復
