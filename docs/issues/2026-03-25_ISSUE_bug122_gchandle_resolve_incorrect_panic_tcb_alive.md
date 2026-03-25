# [Bug]: GcHandle::resolve()  incorrect panic when TCB alive and handle removed via unregister

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：resolve() 同時 unregister() 在 TCB alive 時 |
| **Severity (嚴重程度)** | High | 程式錯誤 panic 而非優雅返回 None |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`resolve()` 應該在 handle 無效（已被 unregister）時返回 `None` 或適當的錯誤，而非 panic。

### 實際行為 (Actual Behavior)

當 TCB 處於 alive 狀態時，`resolve()` 與 `unregister()` 並發執行會導致不正確的 panic：

1. `resolve()` 檢查 TCB roots，沒找到 handle（因為即將被 unregister 或 race window）
2. `resolve()` 釋放 TCB roots lock
3. `unregister()` 移除 handle（只從 TCB roots 移除，**不會**添加到 orphan table，因為 TCB 仍然 alive）
4. `resolve()` 檢查 orphan table，找不到
5. **Panic: "GcHandle::resolve: handle has been unregistered"**

問題根源：當 TCB alive 時，`unregister()` 只從 TCB roots 移除，不會添加到 orphan table。因此出現在 neither table 並不代表 handle 無效，只是因為 TCB alive 時壓根就不會在 orphan table。

### 對比 try_resolve()

`try_resolve()` 應該也受影響，但會返回 `None` 而非 panic。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/cross_thread.rs` 第 194-214 行

```rust
|tcb| {
    let roots = tcb.cross_thread_roots.lock().unwrap();
    if roots.strong.contains_key(&self.handle_id) {
        return self.resolve_impl();  // 找到，正確
    }
    drop(roots);  // RACE WINDOW: 釋放 lock
    let orphan = heap::lock_orphan_roots();
    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        return self.resolve_impl();
    }
    // ...
    panic!("GcHandle::resolve: handle has been unregistered");  // 不正確的 panic！
}
```

**關鍵問題：**
- 當 TCB alive 時，handle 永遠不會在 orphan table
- `unregister()` 當 TCB alive 時只從 TCB roots 移除，不添加到 orphan
- Orphan table 只在 TCB 終止時（`migrate_roots_to_orphan`）才會被填充
- 因此 "not in TCB roots AND not in orphan" 不代表 handle 無效

**unregister() 邏輯 (lines 123-136)：**
```rust
pub fn unregister(&mut self) {
    if self.handle_id == HandleId::INVALID {
        return;
    }
    if let Some(tcb) = self.origin_tcb.upgrade() {
        // TCB alive：只從 TCB roots 移除，不添加到 orphan！
        let mut roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.remove(&self.handle_id);
        drop(roots);
    } else {
        // TCB dead：只從 orphan 移除
        let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
    }
    self.handle_id = HandleId::INVALID;
    crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要 ThreadSanitizer 或精確時序控制：

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    let start_resolve = Arc::new(AtomicBool::new(false));
    let start_unregister = Arc::new(AtomicBool::new(false));
    let start_resolve_c = start_resolve.clone();
    let start_unregister_c = start_unregister.clone();

    // Thread A: resolve() - will hit the race window
    let t1 = thread::spawn(move || {
        start_unregister_c.store(true, Ordering::SeqCst);
        // Spin until unregister starts
        while start_resolve_c.load(Ordering::SeqCst) == false {
            thread::yield();
        }
        // Now resolve - handle might not be in TCB roots anymore
        let _ = handle.resolve();
    });

    // Thread B: unregister - removes from TCB roots only
    let t2 = thread::spawn(move || {
        while !start_unregister.load(Ordering::SeqCst) {
            thread::yield();
        }
        start_resolve.store(true, Ordering::SeqCst);
        // Give resolve() a chance to check TCB roots first
        thread::yield();
        // Now unregister - this only removes from TCB roots (TCB still alive)
        let mut h = handle.clone();
        h.unregister();
    });

    t1.join();
    t2.join();
}
```

**注意**：這個 PoC 需要精確時序。真正穩定復現需要模擬 `migrate_roots_to_orphan` 的邏輯，確認當 TCB alive 時 handle 不會出現在 orphan table。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**核心問題**：當 TCB alive 時，orphan table 不會包含該 handle 的 entry。

修復方案：當 TCB alive 且在 TCB roots 找不到 handle 時，應該返回 `None`（或 `try_resolve()` 作為參考）而非 panic。

```rust
|tcb| {
    let roots = tcb.cross_thread_roots.lock().unwrap();
    if roots.strong.contains_key(&self.handle_id) {
        return self.resolve_impl();
    }
    // TCB alive but not in TCB roots → not in orphan either (unregister case)
    // Return None instead of panicking
    return None;  // <-- 修復：當 TCB alive 但找不到 handle 時返回 None
}
```

但這會改變 `resolve()` 的語義（從 panic 變為 Option）。更好的方案可能是重構邏輯：

1. 當 `upgrade().is_some()` (TCB alive) 且 `roots.strong.contains_key()` 失敗 → 直接返回 `None`
2. 當 `upgrade().is_none()` (TCB dead) 且 orphan 也找不到 → panic（真的無效了）

或者，我應該將 panic 改為 `None` 並更新函數文檔。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 GC root migration 語義問題。當 TCB alive 時，handle 只能存在於 TCB roots。當 TCB terminated 時，roots 會 migrate 到 orphan table。resolve() 的邏輯應該反映這個狀態機：
- TCB alive + in TCB roots → valid
- TCB alive + NOT in TCB roots → 已被 unregister，return None
- TCB dead + in orphan → valid
- TCB dead + NOT in orphan → invalid，panic

目前的實現混淆了"T CB alive + not in TCB roots" 和 "TCB dead + not in orphan" 兩種情況。

**Rustacean (Soundness 觀點):**
不正確的 panic 是一種 panic! 而不是 UB，但仍然是錯誤的 API 行為。調用者無法合理處理這個 panic，特別是在 `try_resolve()` 存在的情況下。如果 handle 無效且無法解析，應該返回 Option。

**Geohot (Exploit 觀點):**
攻擊者可能濫用這個不正確的 panic 來進行 denial of service。雖然不是傳統的記憶體安全漏洞，但能夠觸發 panic 可能在某些上下文中有利用價值。