# [Bug]: GcRootSet::snapshot Race Condition - Dirty Flag Cleared After Lock Release

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 snapshot() 釋放鎖和清除 dirty flag 之間觸發並發修改 |
| **Severity (嚴重程度)** | High | 可能導致 GC 錯過 root，進而導致物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制才能穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::snapshot` (tokio/root.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`GcRootSet::snapshot()` 函數在釋放 mutex 鎖之後才清除 `dirty` flag，這導致在鎖釋放和 flag 清除之間的並發修改會丟失其 dirty 狀態。

### 預期行為
`dirty` flag 應該在釋放鎖之前或以原子方式清除，確保在 snapshot 期間的所有修改都能被正確追蹤。

### 實際行為
在 `snapshot()` 中：
1. 第 123 行：獲取 mutex 鎖
2. 第 124-132 行：過濾並複製有效的 root
3. 第 133 行：釋放 mutex 鎖
4. 第 134 行：清除 dirty flag

這導致以下 race condition：
- 執行緒 A：調用 `snapshot()`，在第 133 行釋放鎖
- 執行緒 B：調用 `register()` 或 `unregister()`，在第 63 或 87 行設置 dirty = true
- 執行緒 A：在第 134 行清除 dirty = false

結果：執行緒 B 的修改會丟失 dirty 狀態！

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題程式碼位於 `crates/rudo-gc/src/tokio/root.rs:122-136`：

```rust
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();  // Line 123: 獲取鎖
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(|&&ptr| {
            unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    drop(roots);  // Line 133: 釋放鎖
    self.dirty.store(false, Ordering::Release);  // Line 134: 清除 dirty flag - BUG!
    valid_roots
}
```

問題：在第 133 行和第 134 行之間，執行緒可以調用 `register()` 或 `unregister()`，這些函數會設置 `dirty = true`（第 63 和 87 行），但隨後會被第 134 行清除，導致這些修改被遺忘。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試來觸發此 race condition：

```rust
#[test]
fn test_gcrootset_snapshot_race() {
    use std::thread;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    
    let set = GcRootSet::global();
    let ready = AtomicBool::new(false);
    let barrier = AtomicBool::new(false);
    
    // 註冊一個 root
    let gc = Gc::new(42i32);
    let ptr = gc.raw_ptr() as usize;
    set.register(ptr);
    
    // 執行緒 A: 調用 snapshot
    let handle_a = thread::spawn(move || {
        while !barrier.load(Ordering::SeqCst) {}
        // 短延遲讓執行緒 B 獲得鎖
        thread::sleep(Duration::from_nanos(1));
        let snapshot = set.snapshot(&crate::heap::current_local_heap().unwrap());
        // 驗證 dirty flag 狀態
        let is_dirty_after = set.is_dirty();
        (snapshot, is_dirty_after)
    });
    
    // 執行緒 B: 並發調用 register/unregister
    let handle_b = thread::spawn(move || {
        barrier.store(true, Ordering::SeqCst);
        while !ready.load(Ordering::SeqCst) {}
        // 嘗試在 snapshot 釋放鎖和清除 dirty flag 之間觸發
        set.unregister(ptr);
        set.register(ptr);
    });
    
    ready.store(true, Ordering::SeqCst);
    let (snapshot, is_dirty_after) = handle_a.join().unwrap();
    handle_b.join().unwrap();
    
    // 如果有 race，dirty flag 可能為 false 但實際上有並發修改
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：在釋放鎖之前清除 dirty flag（可能丟失 snapshot 期間的修改）：

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
    self.dirty.store(false, Ordering::Release);  // 在 drop(roots) 之前清除
    drop(roots);
    valid_roots
}
```

選項 2：使用RwLock並在讀取時清除dirty flag（推薦）：

```rust
// 使用 RwLock 允許並發讀取
roots: RwLock<Vec<usize>>,

pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    // 讀取快照並清除 dirty flag 原子操作
    let (valid_roots, was_dirty) = self.roots
        .read()
        .map(|roots| {
            let valid: Vec<usize> = roots
                .iter()
                .filter(|&&ptr| {
                    unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
                })
                .copied()
                .collect();
            (valid, self.dirty.load(Ordering::Acquire))
        })
        .unwrap_or((Vec::new(), false));
    
    // 如果曾經是 dirty 的，保持 dirty 直到下次修改
    if was_dirty {
        // 檢查是否有新的修改
    }
    
    valid_roots
}
```

選項 3：使用 compare-and-swap 清除 dirty flag：

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
    
    // 使用 CAS 確保只有當前為 dirty 時才清除
    // 如果同時有並發修改，CAS 會失敗但這是預期行為
    let _ = self.dirty.compare_exchange(true, false, Ordering::Release, Ordering::Acquire);
    
    valid_roots
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 bug 會影響 GC 的正確性。如果 root 在 snapshot 期間被添加或移除，但 dirty flag 被錯誤地清除，GC 可能會：
- 錯過某些 root，導致 live 物件被錯誤回收
- 或者錯誤地保留已死亡的物件（機率較低）

**Rustacean (Soundness 觀點):**
這是經典的 TOCTOU (Time-of-check to time-of-use) race condition。雖然不會導致記憶體不安全（不正確的 GC 回收），但會導致記憶體洩露或 use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可能利用此 race condition 來：
- 導致物件被過早回收，進而觸發 use-after-free
- 或者阻止物件被回收，導致記憶體洩露

---

## Resolution (2026-03-02)

**Fixed.** The code in `crates/rudo-gc/src/tokio/root.rs` already applies the correct fix: `dirty` is cleared **while holding the lock** (line 134) before `drop(roots)` (line 135). The comment explicitly documents this: "Clear dirty while still holding the lock so concurrent register/unregister operations cannot have their updates overwritten." Matches suggested fix Option 1. Tokio integration and multi-runtime tests pass.
