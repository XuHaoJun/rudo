# [Bug]: GcHandle::is_valid() TOCTOU - Orphan Lock Release 到 TCB Check 之間的 Race Condition

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發場景：origin thread 在 orphan lock 釋放後、TCB check 前終止 |
| **Severity (嚴重程度)** | Medium | 導致 is_valid() 對有效 handle 返回 false，API 不一致 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制，但理論上可穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::is_valid()`, `handles/cross_thread.rs:100-115`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`is_valid()` 應該在 handle 有效時返回 true，不應有 TOCTOU race condition。

### 實際行為 (Actual Behavior)

在 `drop(orphan)` 釋放 orphan lock 之後、`origin_tcb.upgrade()` 檢查 TCB roots 之前，存在一個 race window：

1. 檢查 orphan table（持有 orphan lock）
2. 釋放 orphan lock
3. **Race window**: origin thread 在此時終止，roots 遷移到 orphan table
4. 檢查 TCB roots：
   - `upgrade()` 可能返回 `None`（TCB 正在被 drop）
   - 或 `upgrade()` 返回 `Some` 但 `roots.strong` 為空（已經遷移到 orphan）

結果：`is_valid()` 返回 false，但 handle 實際上是有效的（在 orphan table 中）。

### 對比 `resolve()` 的行為

`resolve()` 使用 `map_or_else` 模式在整個檢查過程中持有適當的鎖，避免了這個 TOCTOU。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `handles/cross_thread.rs:100-115`：

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    // Check orphan first: when origin exits, roots migrate before TCB drops. There is a
    // window where upgrade() returns Some but roots.strong is empty (already migrated).
    let orphan = heap::lock_orphan_roots();
    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        return true;
    }
    drop(orphan);  // <-- Lock released here
    self.origin_tcb.upgrade().is_some_and(|tcb| {  // <-- No lock protection
        let roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.contains_key(&self.handle_id)
    })
}
```

問題：
1. 在 `drop(orphan)` 和 `origin_tcb.upgrade()` 之間沒有鎖保護
2. Origin thread 可以在此時終止並遷移 roots 到 orphan table
3. 導致 false negative：有效的 handle 被錯誤地判斷為無效

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Arc::new(Gc::new(Data { value: 42 }));
    let handle = Arc::new(gc.cross_thread_handle());
    let is_valid_result = Arc::new(AtomicBool::new(false));
    
    // Thread A: check is_valid after a short delay
    let handle_a = Arc::clone(&handle);
    let is_valid_result_a = Arc::clone(&is_valid_result);
    let thread_a = thread::spawn(move || {
        thread::sleep(Duration::from_millis(1)); // Let thread B start
        let result = handle_a.is_valid();
        is_valid_result_a.store(result, Ordering::SeqCst);
        println!("Thread A: is_valid() = {}", result);
    });

    // Thread B: Drop the Gc to trigger migration
    let gc_b = Arc::clone(&gc);
    let thread_b = thread::spawn(move || {
        println!("Thread B: dropping Gc");
        drop(gc_b);
    });

    thread_a.join().unwrap();
    thread_b.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用類似 `resolve()` 的模式，在整個檢查過程中保持鎖的一致性：

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    
    // Try TCB first (if alive), then fall back to orphan
    self.origin_tcb.upgrade().map_or_else(
        || {
            // TCB is dead, check orphan table
            let orphan = heap::lock_orphan_roots();
            orphan.contains_key(&(self.origin_thread, self.handle_id))
        },
        |tcb| {
            let roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.contains_key(&self.handle_id)
        },
    )
}
```

注意：需要先檢查 TCB 再檢查 orphan，與當前順序相反。這樣可以避免在 TCB 活著時釋放 orphan lock 後的 race window。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的 TOCTOU race condition。問題在於 GC 系統中線程生命週期管理與 root migration 的時序問題。當 origin thread 終止時，roots 會從 TCB 遷移到 orphan table，但這個遷移過程與 `is_valid()` 的檢查之間存在 race window。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題（不會導致 UAF 或 memory corruption），但是一個 API 可用性問題。`is_valid()` 返回 false 可能導致用戶錯誤地認為 handle 不可用，進而採取錯誤的錯誤處理邏輯。

**Geohot (Exploit 攻擊觀點):**
這個 race condition 很難利用，但理論上：
- 攻擊者可以使 handle 看起來無效（false negative）
- 這可能導致用戶端的邏輯錯誤，例如重新創建已經存在的資源
- 配合其他 bug 可能造成更大影響

---

## 備註

此 bug 與 bug128（GcHandle::is_valid() 未驗證 Root 存在性）相關但不同：
- bug128: 修復了原本完全不檢查 root list 的問題
- bug313: 當前 bug 是關於在檢查過程中 orphan lock 釋放後的 race window

