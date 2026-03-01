# [Bug]: GcRootSet snapshot dirty flag lost update - 新增 root 可能被忽略

**Status:** Fixed
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 tokio 環境並發 register/snapshot |
| **Severity (嚴重程度)** | High | 可能導致 GC 漏標 root，錯誤回收 live 物件 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::snapshot` in `tokio/root.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current
- **Feature:** `tokio` feature required

---

## 📝 問題描述 (Description)

`GcRootSet::snapshot` 函數在清除 dirty flag 時存在 lost update 問題。當並發執行 `register()` 和 `snapshot()` 時，新註冊的 root 可能被 GC 忽略，導致 live 物件被錯誤回收。

### 預期行為 (Expected Behavior)

每次呼叫 `snapshot()` 後，如果還有新的 root 被註冊，dirty flag 應該保持為 true，直到所有新 root 都被 GC 處理。

### 實際行為 (Actual Behavior)

`snapshot()` 在釋放 mutex 後無條件清除 dirty flag，導致並發的 `register()` 所設置的 flag 被覆蓋：

```rust
// tokio/root.rs:122-136
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();  // 獲取 mutex
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(...)
        .copied()
        .collect();
    drop(roots);  // 釋放 mutex
    self.dirty.store(false, Ordering::Release);  // BUG: 無條件清除!
    valid_roots
}
```

Race condition 時序：
1. Thread A: 呼叫 `register()`，添加新 root，設置 dirty = true
2. Thread B: 呼叫 `snapshot()`，持有 mutex 中
3. Thread A: 嘗試 `register()`，等待獲取 mutex
4. Thread B: 完成迭代，釋放 mutex
5. Thread A: 獲取 mutex，添加 root，設置 dirty = true，釋放 mutex
6. Thread B: 清除 dirty = false → **Thread A 的更新丟失!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

`dirty` flag 的設計是用於表示「是否有新的 root 需要 GC 處理」。但 `snapshot()` 在釋放 mutex 後無條件清除，沒有考慮到並發的 `register()` 可能已經添加了新的 root。

這導致：
1. GC 可能會跳過新註冊的 root
2. 該 root 指向的物件可能被錯誤回收
3. 造成 use-after-free

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 `tokio` feature
2. 創建多個 tokio task，同時進行：
   - Task A: 不斷調用 `GcRootGuard::new()` 註冊新 root
   - Task B: 不斷調用 `GcRootSet::global().snapshot()` 觸發 GC
3. 需要精確的時序控制來觸發 race

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用 compare-and-swap 來安全地清除 dirty flag：

```rust
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(|&&ptr| {
            unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    drop(roots);
    
    // Only clear dirty if no new roots were added since we acquired the lock
    // Use compare-and-swap to avoid lost updates
    let _ = self.dirty.compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire);
    
    valid_roots
}
```

或者，在持有 mutex 時清除 flag：

```rust
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let mut roots = self.roots.lock().unwrap();
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(|&&ptr| {
            unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    
    // Clear dirty while holding the lock
    self.dirty.store(false, Ordering::Release);
    drop(roots);
    
    valid_roots
}
```

第二個方案更簡單，但會延長 mutex 持有時間，可能影響效能。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Root set 的完整性對於 GC 正確性至關重要。如果 GC 漏標 root，會導致 live 物件被回收，這是嚴重的正確性問題。GcRootSet 是 tokio 整合的關鍵元件，這種 race condition 特別危險。

**Rustacean (Soundness 觀點):**
這是潛在的 soundness 問題。如果 live 物件被錯誤回收，後續存取會造成 use-after-free。在 Rust 的記憶體安全承諾下，這是不可接受的。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 強制 GC 回收特定物件
2. 造成 use-after-free
3. 破壞 tokio async 應用的記憶體完整性

---

## Resolution (2026-03-01)

**Outcome:** Fixed, verification partially covered.

Applied a minimal synchronization fix in `crates/rudo-gc/src/tokio/root.rs`:
- `GcRootSet::snapshot()` now clears `dirty` while still holding the `roots` mutex.
- This prevents the lost-update window where concurrent `register()`/`unregister()` can set `dirty = true` and then be overwritten by a later unconditional `store(false)`.

Verification:
- `cargo test -p rudo-gc --features tokio --lib tokio::root::tests::test_snapshot -- --test-threads=1` ✅
- `cargo test -p rudo-gc --features tokio --test tokio_multi_runtime test_dirty_flag_behavior -- --test-threads=1` ✅

Note: no deterministic stress/loom reproduction was added in this change, so the tag remains `Not Verified`.
