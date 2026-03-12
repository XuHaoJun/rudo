# [Bug]: GcThreadSafeCell::capture_gc_ptrs_into 使用 try_lock 與 GcRwLock/GcMutex 行為不一致

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在多執行緒環境下，當 GcThreadSafeCell 被某執行緒持有鎖時，另一執行緒嘗試追蹤 GC 指針 |
| **Severity (嚴重程度)** | Medium | 可能導致 SATB barrier 遺漏 GC 指針，進而可能導致物件被錯誤回收 |
| **Reproducibility (重現難度)** | Medium | 需要並發場景才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::capture_gc_ptrs_into` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

所有類型在實現 `GcCapture::capture_gc_ptrs_into` 時應該有一致的行為：
- `GcRwLock` 使用 blocking `read()` 確保捕獲所有 GC 指針
- `GcMutex` 使用 blocking `lock()` 確保捕獲所有 GC 指針
- `GcThreadSafeCell` 應該使用 blocking 鎖或提供明確的文档说明

### 實際行為 (Actual Behavior)

`GcThreadSafeCell::capture_gc_ptrs_into` 使用 `try_lock()`：
- 當鎖被其他執行緒持有時，靜默跳過捕獲
- 雖然文檔說「writer 會在 borrow_mut() 中記錄 SATB」，但這假設了始終有 writer 存在
- 當多個 reader 持有鎖時，會遺漏 GC 指針

### 程式碼位置

`cell.rs:1320-1324`：
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Some(guard) = self.inner.try_lock() {  // <-- BUG: 使用 try_lock
        guard.capture_gc_ptrs_into(ptrs);
    }
}
```

對比 `GcRwLock` (`sync.rs:727-732`)：
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let guard = self.inner.read();  // <-- 使用 blocking read
    guard.capture_gc_ptrs_into(ptrs);
}
```

對比 `GcMutex` (`sync.rs:755-761`)：
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let guard = self.inner.lock();  // <-- 使用 blocking lock
    guard.capture_gc_ptrs_into(ptrs);
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `GcThreadSafeCell` 的設計目標是跨執行緒訪問，因此使用 blocking 鎖可能會導致死結。然而：

1. `try_lock()` 失敗時會靜默跳過 GC 指針捕獲
2. 當 cell 被多個 reader 同時持有時（雖然不常見），會遺漏 GC 指針
3. 這與 `GcRwLock` 和 `GcMutex` 的實現不一致，後兩者使用 blocking 鎖

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 建立 `GcThreadSafeCell<Gc<SomeType>>`
2. 一個執行緒獲取讀取鎖（通过 borrow()）
3. 同時另一個執行緒嘗試追蹤 GC root
4. 由於 try_lock() 失敗，GC 指針可能被遺漏

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：改用 blocking 鎖（可能導致死結）
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let guard = self.inner.lock();  // 使用 blocking lock
    guard.capture_gc_ptrs_into(ptrs);
}
```

選項 2：添加明確的文檔說明此行為是故意的
```rust
/// Uses `try_lock()` to avoid deadlocks in cross-thread scenarios.
/// Note: If the lock is held by another thread, GC pointers will be silently missed.
/// The writer is expected to record SATB in `borrow_mut()` when it acquires the lock.
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier 需要捕獲所有舊值以確保正確性。使用 try_lock() 遺漏 GC 指針可能導致增量標記不正確，進而可能導致物件被錯誤回收。

**Rustacean (Soundness 觀點):**
雖然不是嚴格的 UB，但可能導致記憶體安全問題。如果物件被錯誤回收且有指標指向它，可能導致 Use-After-Free。

**Geohot (Exploit 攻擊觀點):**
在並發環境中，攻擊者可能通過控制鎖的獲取時序來觸發此問題，導致 GC 指針被遺漏，進而可能導致記憶體錯誤。

---

## 驗證記錄

- [ ] Bug 存在於目前程式碼中
- [ ] 與 GcRwLock/GcMutex 實現不一致
- [ ] 確認此為設計選擇還是需要修復
