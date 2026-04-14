# [Bug]: GcHandle::downgrade() 在 orphan migration 視窗期間不正確地 panic

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序：downgrade 在 migration 期間被調用 |
| **Severity (嚴重程度)** | High | 導致 panic（拒絕服務），但不會造成記憶體安全問題 |
| **Reproducibility (復現難度)** | Medium | 需要精確控制執行緒終止和 downgrade 的時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade()` (cross_thread.rs:497-659)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (012-cross-thread-gchandle feature)

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::downgrade()` 應該在 orphan migration 視窗期間正確處理有效的 handle，不應 panic。當 TCB roots 被遷移到 orphan table 的過程中，handle 應該可以在 orphan table 中找到。

### 實際行為 (Actual Behavior)

在 `migrate_roots_to_orphan()` 執行期間，存在一個 race window：
1. TCB roots lock 被釋放（entries 已排出到本地 vector）
2. Orphan lock 尚未取得（entries 尚未插入 orphan table）

如果在此視窗期間調用 `downgrade()`：
- `origin_tcb.upgrade()` 返回 `Some`（TCB 仍然 alive）
- 但 `roots.strong` 為空（已被遷移）
- `downgrade()` 在 line 506-510 檢查 `roots.strong` 後直接檢查 orphan
- 由於 handle 尚未在 orphan table（migration 視窗），panic 發生

### 對比 `clone()` 的行為

`clone()` 在 bug470 中已對此 race window 進行修復，採用 retry logic：
1. 如果 TCB alive 但 entry 不在 TCB roots，檢查 orphan
2. 如果不在 orphan，重新檢查 TCB roots（migration 可能已在 orphan lock 獲取期間完成）
3. 如果仍找不到且 TCB 仍然 alive，再次檢查 orphan

但 `downgrade()` **沒有**這個 retry logic，會直接 panic。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### Migration Lock Ordering (heap.rs)

`migrate_roots_to_orphan()` 的鎖順序：
1. 取得 TCB roots lock
2. 將 entries 排出到本地 vector
3. **釋放 TCB roots lock** ← Handle 現在不在任何位置！
4. 取得 orphan lock
5. 將 entries 插入 orphan table
6. 釋放 orphan lock

### downgrade() 問題代碼 (cross_thread.rs:506-510)

```rust
if !roots.strong.contains_key(&self.handle_id) {
    drop(roots);
    let orphan = heap::lock_orphan_roots();
    if !orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        panic!("GcHandle::downgrade: handle has been unregistered");  // BUG: 沒有 retry logic！
    }
    // ...
}
```

對比 clone() 的修復 (lines 737-775)：
```rust
if !roots.strong.contains_key(&self.handle_id) {
    // BUG470 fix: Check orphan before panicking - migration may be in progress.
    // This matches the retry pattern in GcHandle::resolve() (bug401).
    let orphan = heap::lock_orphan_roots();
    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        drop(orphan);
    } else {
        drop(orphan);
        drop(roots);
        // Migration in progress: retry TCB lookup
        roots = tcb.cross_thread_roots.lock().unwrap();
        if !roots.strong.contains_key(&self.handle_id) {
            // ... retry orphan ...
        }
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

為 `downgrade()` 添加與 `clone()` 相同的 retry logic：

1. 當 TCB alive 但 entry 不在 TCB roots 時
2. 先檢查 orphan table
3. 如果 orphan 也沒有，等待後重試 TCB roots（migration 可能已完成）
4. 如果仍找不到，則 panic

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
orphan migration 是執行緒終止時的必要操作。GC 必須維護 outlive 建立執行緒的 handles。但鎖順序創建了一個視窗，期間 handle 對 downgrade 不可見。這與 bug470 中 clone() 的情況相同，但 downgrade 尚未修復。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題（不會導致 UAF 或 memory corruption），而是一個 API 可用性問題。`downgrade()` panic 可能導致用戶程式崩潰，特別是在執行緒終止和 handle 操作並發進行的場景中。

**Geohot (Exploit 觀點):**
這個 panic 可以被利用來進行拒絕服務攻擊。攻擊者可以讓目標執行緒終止，同時嘗試在 migration 視窗期間調用 `downgrade()`，導致程式崩潰。