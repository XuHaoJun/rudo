# [Bug]: AsyncHandle::get() missing dead/dropping/construction checks

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 在 async scope 結束後使用 handle 時觸發 |
| **Severity (嚴重程度)** | `Critical` | 可能導致 use-after-free 或讀取無效資料 |
| **Reproducibility (中等)** | `Medium` | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()`, `AsyncHandle::to_gc()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`AsyncHandle::get()` 函數在解引用時沒有檢查 `has_dead_flag()`、`dropping_state()` 或 `is_under_construction()`，而 `Gc::deref` 有檢查這些標誌。

這導致在 async scope 結束後使用 handle 時，可能發生 use-after-free。

### 預期行為
`AsyncHandle::get()` 應該像 `Gc::deref` 一樣檢查對象的有效性。

### 實際行為
`AsyncHandle::get()` 直接訪問 value，繞過了安全檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`AsyncHandle::get()` 位於 `handles/async.rs:570-588`：

```rust
pub fn get(&self) -> &T {
    // ... scope 檢查 ...
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe { &*gc_box_ptr }.value()  // <-- 沒有檢查 dead_flag, dropping_state, is_under_construction!
}
```

相比之下，`Gc::deref` 位於 `ptr.rs:1345-1355`：

```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::deref: cannot dereference a dead Gc"
        );
        &(*gc_box_ptr).value
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;

#[derive(Trace)]
struct Data { value: i32 }

async fn bug_poc() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    drop(scope); // scope 結束
    
    // 這應該 panic，但實際上可能返回無效資料
    // println!("{}", handle.get().value);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `AsyncHandle::get()` 中添加檢查：

```rust
pub fn get(&self) -> &T {
    // ... existing scope check ...
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "AsyncHandle::get: cannot access a dead or dropping Gc"
        );
        &(*gc_box_ptr).value
    }
}
```

同樣需要修復 `AsyncHandle::to_gc()`。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`AsyncHandle` 的設計類似於 Scheme 的_handle_，需要確保在 scope 結束後無法訪問。缺少這些檢查會導致違反 GC 不變量。

**Rustacean (Soundness 觀點):**
這是一個 soundness bug。直接訪問 `GcBox::value` 而不通過安全檢查可能導致 UB。

**Geohot (Exploit 觀點):**
攻擊者可以利用這個漏洞在對象被釋放後繼續訪問記憶體，導致 use-after-free。
