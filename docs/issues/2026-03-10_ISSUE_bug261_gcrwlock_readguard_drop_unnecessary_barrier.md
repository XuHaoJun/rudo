# [Bug]: GcRwLockReadGuard::drop() 錯誤地觸發 Write Barrier - Read 操作不應需要 Barrier

**Status:** Open
**Tags:** Unverified

---

## 📊 Threat Model Assessment

| Aspect | Assessment |
|--------|------------|
| Likelihood | Low |
| Severity | Medium |
| Reproducibility | Low |

---

## 🧩 Affected Component & Environment

- **Component:** `GcRwLockReadGuard::drop()` in `sync.rs:386-396`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`GcRwLockReadGuard::drop()` 不應該觸發任何 write barrier，因為讀取操作不會修改資料。SATB barrier 的目的是記錄被覆蓋之前的舊值，但讀取操作不會覆蓋任何東西。

### Actual Behavior

`GcRwLockReadGuard::drop()` 目前錯誤地調用 `mark_object_black`：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        for gc_ptr in ptrs {
            let _ =
                unsafe { crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8) };
        }
    }
}
```

這與 bug237 描述的問題不同：
- bug237 說的是 `GcRwLockReadGuard::drop()` 缺少 barrier（那時程式碼是空的）
- 現在的程式碼有了 barrier，但這是錯誤的 - 讀取操作不應該觸發 barrier

---

## 🔬 Root Cause Analysis

1. **SATB Barrier 的用途**：Snapshot-At-The-Beginning barrier 是為了記錄「被覆蓋之前的舊值」，確保在 incremental marking 期間，即使物件被修改，原本可達的物件仍然能被追蹤。

2. **讀取操作不需要 Barrier**：當你只是讀取資料時，沒有任何東西被「覆蓋」，所以沒有舊值需要記錄。

3. **錯誤的類比**：這就好比在一個只讀的文件上執行「儲存前備份」一樣 - 沒有任何改動，當然不需要備份。

4. **與 Write Guard 的對比**：
   - `GcRwLockWriteGuard::drop()` - 需要 barrier，因為寫入會覆蓋舊值
   - `GcMutexGuard::drop()` - 需要 barrier，因為寫入會覆蓋舊值
   - `GcRwLockReadGuard::drop()` - **不需要** barrier，讀取不會覆蓋任何東西

---

## 💣 Potential Issues

雖然這不會導致記憶體不安全（`mark_object_black` 是 idempotent 的），但它會導致：

1. **不必要的效能開銷**：每次讀取 guard drop 時都會執行不必要的 barrier 操作
2. **语义不一致**：與預期行為不符，讀取操作不應該觸發 barrier
3. **未預期的行為**：可能會導致追蹤邏輯混亂

---

## 🛠️ Suggested Fix / Remediation

移除 `GcRwLockReadGuard::drop()` 中的 barrier 捕獲邏輯：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // Read guard doesn't modify data - no barrier needed.
        // The parking_lot guard will release the read lock automatically.
    }
}
```

或者，如果出於任何原因需要保留某些追蹤，應該明確標注這是什麼目的：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        // NOTE: No barrier needed for read operations.
        // Only write operations (WriteGuard, MutexGuard) need to record old values for SATB.
    }
}
```

---

## 🗣️ Internal Discussion Record

### R. Kent Dybvig
SATB barrier 的設計是為了解決「在標記期間修改指標」這個問題。對於讀取操作，沒有修改發生，就沒有必要記錄任何東西。這是基本的 GC 理論。

### Rustacean
這是一個语义错误。虽然不会导致内存安全问题（mark_object_black 是安全的），但它违反了「只有写入才需要 barrier」的设计原则。

### Geohot
这会导致不必要的性能开销。在高性能场景中，每个读操作都会被不必要地延迟。

---

## 📋 Related Bugs

- bug237: GcRwLockReadGuard::drop() 缺少 SATB 捕獲（已修复，但修复方向错误）
- bug107: GcRwLockWriteGuard/GcMutexGuard Drop 缺少 generational barrier（已修复）
- bug161: GcRwLock/GcMutex Drop TOCTOU（已修复）

---

## ✅ Resolution

- [ ] Fixed
- [x] Not Fixed
