# [Bug]: GcMutex::capture_gc_ptrs_into() 使用 try_lock() 而非 lock()，與 GcRwLock 不一致

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在並發場景下，當 GC 進行 SATB 掃描時剛好有一個線程持有 GcMutex |
| **Severity (嚴重程度)** | Medium | 可能導致 SATB 不完整，但影響範圍有限（持有鎖的線程仍會觸發 barrier） |
| **Reproducibility (復現難度)** | High | 需要特定時序：GC 掃描 + mutex 被佔用 + 有 GC 指針需要保護 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcMutex`, `GcRwLock`, `GcCapture`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcMutex::capture_gc_ptrs_into()` 應該與 `GcRwLock::capture_gc_ptrs_into()` 使用相同的阻塞策略，以確保 SATB (Snapshot-At-The-Beginning) 的正確性。

### 實際行為 (Actual Behavior)
`GcMutex::capture_gc_ptrs_into()` 使用 `try_lock()` 而非 `lock()`，當 mutex 被其他線程持有時會靜默失敗，無法捕獲 GC 指針。這與 `GcRwLock` 的實現不一致。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/sync.rs` 中：

- **GcRwLock** (lines 648-652) 使用阻塞的 `read()`：
  ```rust
  fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
      // Use blocking read() to reliably capture all GC pointers. try_read() would
      // silently miss pointers when a writer holds the lock, breaking SATB.
      let guard = self.inner.read();
      guard.capture_gc_ptrs_into(ptrs);
  }
  ```

- **GcMutex** (lines 676-680) 使用非阻塞的 `try_lock()`：
  ```rust
  fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
      if let Some(guard) = self.inner.try_lock() {
          guard.capture_gc_ptrs_into(ptrs);
      }
  }
  ```

`GcRwLock` 的註釋明確警告了這個問題，但 `GcMutex` 實現時遺漏了相同的邏輯。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上可通過以下步驟觸發：
1. 創建一個包含 GC 指針的 `GcMutex<T>`
2. 在一個線程中長期持有該 mutex（使用 `lock()` 阻塞）
3. 在另一個線程中觸發 GC 的 SATB 掃描
4. 觀察 `capture_gc_ptrs_into()` 是否能正確捕獲 GC 指針

實際上，由於持有鎖的線程在操作 GC 指針時會觸發 write barrier，問題影響較小，但仍是一個一致性問題。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `GcMutex::capture_gc_ptrs_into()` 改為使用阻塞的 `lock()`：

```rust
#[inline]
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // Use blocking lock() to reliably capture all GC pointers, consistent with
    // GcRwLock::capture_gc_ptrs_into(). try_lock() would silently miss pointers
    // when a writer holds the lock, potentially breaking SATB.
    let guard = self.inner.lock();
    guard.capture_gc_ptrs_into(ptrs);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 角度來看，SATB 需要完整且一致的引用快照。`GcMutex` 使用 `try_lock()` 可能在以下場景造成問題：
1. 增量標記期間進行 SATB 掃描
2. GC 線程需要捕獲所有根引用
3. 此時某個應用線程持有 GcMutex

雖然持有鎖的線程在離開臨界區時會觸發 barrier，但這依賴於「所有線程都會解鎖」這一假設。如果鎖被長期持有（如用於長時間計算），可能導致增量標記不完整。

**Rustacean (Soundness 觀點):**
這不是嚴格的 soundness 問題，因為：
- 持有鎖的線程仍會通過 unlock 觸發 barrier
- 內存安全性未被直接破壞

但這是一個 API 不一致性問題，可能導致未預期的行為。`GcRwLock` 有明確註釋說明為何使用 blocking 操作，`GcMutex` 應保持一致。

**Geohot (Exploit 觀點):**
在極端情況下，可能利用此不一致性：
- 攻擊者可以嘗試長期持有 GcMutex 來干擾 GC 的 SATB 掃描
- 這可能導致某些應為 live 的對象被錯誤回收（理論上）
- 實際影響取決於具體使用模式和時序
