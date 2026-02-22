# [Bug]: GcHandle clone()/unregister() Race 導致物件在 Root 移除後仍被視為 Root

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發調用 clone() 和 unregister() |
| **Severity (嚴重程度)** | High | 導致記憶體洩漏或潛在的 Use-After-Free |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle`, `cross_thread.rs`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`GcHandle` 的 `clone()` 和 `unregister()` 方法存在 TOCTOU (Time-of-Check to Time-of-Use) 競態條件，導致：

1. **當 clone() 在 unregister() 之前執行**：cloned handle 的 root entry 會被孤立 (orphaned)，導致物件無法被回收
2. **當 clone() 檢查 handle_id 和實際插入 之間 unregister() 被調用**：物件在該時間窗口內沒有 root 保護

### 預期行為
- `unregister()` 應該移除所有相關的 root entries
- `clone()` 應該在持有鎖的情況下驗證 handle 有效性

### 實際行為
1. **Orphaned Root 導致記憶體洩漏**:
   - `clone()` 創建新的 root entry (new handle_id)
   - `unregister()` 只移除原始 handle_id 的 entry
   - Cloned handle 的 entry 永遠存在 → 物件無法回收

2. **TOCTOU 導致潛在 Use-After-Free**:
   - `clone()` 檢查 `handle_id != HandleId::INVALID` (無鎖)
   - 檢查通過後、插入前，`unregister()` 移除 root
   - 如果 GC 運行，物件可能被回收

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs:206-225` 的 `Clone` 實現中：

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        assert_ne!(
            self.handle_id,
            HandleId::INVALID,
            "cannot clone an unregistered GcHandle"
        );

        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        let new_id = roots.allocate_id();
        roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
        drop(roots);

        Self {
            ptr: self.ptr,
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
            handle_id: new_id,
        }
    }
}
```

問題：
1. **無鎖檢查**：`handle_id != HandleId::INVALID` 在獲取鎖之前檢查
2. **獨立的 handle_id**：clone() 使用新的 handle_id，與原始 handle 無關
3. **unregister() 只移除原始 ID**：在 `handles/cross_thread.rs:104-109`

```rust
pub fn unregister(&mut self) {
    let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
    roots.strong.remove(&self.handle_id);  // 只移除自己的 handle_id
    drop(roots);
    self.handle_id = HandleId::INVALID;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct LargeData {
    _data: [u8; 1024],
}

#[test]
fn test_gchandle_clone_unregister_race() {
    // 1. Create GcHandle
    let gc = Gc::new(LargeData { _data: [0u8; 1024] });
    let handle = gc.cross_thread_handle();
    
    // 2. Clone the handle (creates new root entry with new handle_id)
    let cloned = handle.clone();
    
    // 3. Unregister original handle (only removes original handle_id)
    handle.unregister();
    
    // 4. Drop original handle
    drop(handle);
    
    // 5. Force GC
    collect_full();
    
    // Expected: Object should be collectable since unregister was called
    // Actual: Object remains alive due to orphaned root entry from clone
    
    // The cloned handle still has its entry in cross_thread_roots,
    // keeping the object alive even though we explicitly called unregister()
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：使用相同的 Handle ID 追蹤所有 Clones

修改 `Clone` 使用共享的 handle_id 追蹤：

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        // Share the same handle_id to track all clones together
        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        // Increment refcount instead of creating new entry
        roots.strong.get(&self.handle_id); // Ensure exists
        drop(roots);

        Self {
            ptr: self.ptr,
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
            handle_id: self.handle_id, // Share handle_id!
        }
    }
}

impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        // Only remove if this is the last clone
        // Need reference counting for handle_ids
        roots.strong.remove(&self.handle_id);
    }
}
```

### 方案 2：Clone 時驗證並修復 Root

在 `clone()` 中驗證原始 root 仍然存在：

```rust
fn clone(&self) -> Self {
    let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
    
    // Verify original root still exists
    if !roots.strong.contains_key(&self.handle_id) {
        // Root was removed - re-insert to maintain invariant
        roots.strong.insert(self.handle_id, self.ptr.cast());
    }
    
    let new_id = roots.allocate_id();
    roots.strong.insert(new_id, self.ptr.cast());
    drop(roots);
    
    Self { /* ... */ }
}
```

### 方案 3：全域 Handle ID 追蹤

使用獨立的結構追蹤所有相關的 handle IDs：

```rust
struct HandleGroup {
    primary_id: HandleId,
    clone_ids: Vec<HandleId>,
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在傳統 GC 中，cross-thread references 通常透過共享的 root set 追蹤，而不是每個 handle 獨立的 root。rudo-gc 的設計讓每個 `GcHandle` 都有獨立的 root entry，這導致了 clone/unregister 语义不一致。建議重新設計為共享 root 追蹤機制。

**Rustacean (Soundness 觀點):**
這是明確的記憶體安全問題：
1. TOCTOU 檢查 `handle_id != INVALID` 在無鎖狀態進行
2. 物件可能在 root 移除後但 GC 回收前被存取
3. `clone()` 使用新的 handle_id 導致 `unregister()` 無法追蹤所有相關 entries

**Geohot (Exploit 攻擊觀點):**
攻擊者可以：
1. 對 handle 調用 `clone()` 
2. 對原始 handle 調用 `unregister()` 期望釋放記憶體
3. 利用 timing 繼續持有clone 並保持物件 alive
4. 如果物件包含敏感資料，這會導致延長的生命週期

或者更危險：
1. 在 `clone()` 檢查和插入之間調用 `unregister()`
2. 觸發 GC
3. 物件被回收，但 clone 仍持有指標
4. 後續使用該指標 → Use-After-Free

---

## Resolution

**2026-02-21** — Fixed TOCTOU by moving validity check under lock (resolve-patterns TOCTOU):

- **Clone (TCB path):** Before allocating/inserting, acquire `cross_thread_roots` lock and verify `roots.strong.contains_key(&self.handle_id)`. If not present (removed by concurrent unregister), panic.
- **Clone (orphan path):** Added `heap::clone_orphan_root()` which atomically checks `(thread_id, handle_id)` exists, allocates new ID, inserts. Returns `(id, false)` if source was removed.
- Early `handle_id == INVALID` check remains for fast-fail; the critical check is under lock to prevent TOCTOU where another thread unregisters a *different* handle (clone) sharing the same object.
- Added tests: `test_clone_unregistered_handle_panics`, `test_clone_then_unregister_cloned_keeps_alive`.
