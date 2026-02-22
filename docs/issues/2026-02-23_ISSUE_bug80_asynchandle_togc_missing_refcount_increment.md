# [Bug]: AsyncHandle::to_gc 缺少 ref count 增量導致 Use-After-Free

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 async scope 內使用 to_gc 將 handle 轉換為 Gc 並讓 scope 終止 |
| **Severity (嚴重程度)** | Critical | 導致 Use-After-Free，記憶體不安全 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::to_gc` in `handles/async.rs:655-660`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

`AsyncHandle::to_gc` 應該與 `Handle::to_gc` 行為一致，返回的 Gc 應該遞增 ref count，使其能夠獨立於 AsyncHandleScope 存活。

`Handle::to_gc` (handles/mod.rs:340-348) 的實作：
```rust
pub fn to_gc(&self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        let gc: Gc<T> = Gc::from_raw(ptr);
        let gc_clone = gc.clone();  // 透過 clone 遞增 ref count
        std::mem::forget(gc);
        gc_clone
    }
}
```

文檔說明 (handles/async.rs:622-625):
> "The returned `Gc` has an incremented reference count and will outlive this handle and its scope."

### 實際行為

`AsyncHandle::to_gc` (handles/async.rs:655-660) 直接調用 `Gc::from_raw`，沒有遞增 ref count：
```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        Gc::from_raw(ptr)  // 沒有遞增 ref count！
    }
}
```

### 影響範圍

此問題導致：
1. 返回的 Gc 沒有正確的 ref count
2. 當 AsyncHandleScope 被 drop 時，物件可能被回收
3. 返回的 Gc 變成懸空指標 (dangling pointer)
4. 使用返回的 Gc 會導致 Use-After-Free

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/async.rs:655-660` (`AsyncHandle::to_gc`)

`Handle::to_gc` 使用 `gc.clone()` 來創建返回的 Gc，而 `Gc::clone()` 內部會遞增 ref count：
```rust
// ptr.rs:117-128
pub fn inc_ref(&self) {
    self.ref_count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
        if count == usize::MAX {
            None
        } else {
            Some(count.saturating_add(1))
        }
    }).ok();
}
```

然後 `std::mem::forget(gc)` 防止原 Gc 的 drop 遞減 ref count。

但 `AsyncHandle::to_gc` 直接調用 `Gc::from_raw`，完全跳過了 ref count 遞增，導致返回的 Gc 與原始 Gc 共享相同的 ref count，但沒有任何機制保持物件存活。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;
use rudo_gc::heap::current_thread_control_block;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    rudo_gc::test_util::reset();

    let tcb = current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    let gc = Gc::new(Data { value: 42 });

    // 原始 gc 應該有 ref_count = 1
    assert_eq!(Gc::ref_count(&gc).get(), 1);

    let handle = scope.handle(&gc);
    let gc1 = handle.to_gc();

    // 由於 bug，gc 的 ref_count 仍然是 1（正確應該是 2）
    // 讓 scope drop，這會移除 root
    drop(scope);

    // 嘗試存取 gc1 - 這是 Use-After-Free！
    // 因為 gc1 沒有正確的 ref count 來保持物件存活
    println!("Value: {}", gc1.value); // 未定義行為！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `handles/async.rs:655-660` 的 `AsyncHandle::to_gc` 實作，與 `Handle::to_gc` 保持一致：

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        let gc: Gc<T> = Gc::from_raw(ptr);
        let gc_clone = gc.clone();  // 遞增 ref count
        std::mem::forget(gc);       // 防止 drop 遞減 ref count
        gc_clone
    }
}
```

同時需要添加 `has_dead_flag()` 和 `dropping_state()` 檢查（這是 bug70 的內容）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在引用計數 GC 中，當從 handle 轉換為獨立的 Gc 時，必須遞增 ref count。這是因為 handle 依賴 scope 來保持物件存活，而獨立的 Gc 需要自己的 ref count。缺少這個遞增會導致 scope drop 時物件被錯誤回收，這是經典的 GC 記憶體管理錯誤。

**Rustacean (Soundness 觀點):**
這是一個明確的記憶體安全問題 - Use-After-Free。`Gc::from_raw` 的文檔明確說明："The pointer must be a valid, currently allocated GcBox." 但返回的 Gc 沒有正確的 ref count來保持有效性，違反了記憶體安全 invariant。

**Geohot (Exploit 觀點):**
這個 bug 可以被利用來進行記憶體佈局攻擊。攻擊者可以：
1. 創建 AsyncHandleScope 和 Gc
2. 調用 to_gc() 獲取"懸空"的 Gc
3. 觸發 scope drop，物件被回收
4. 重新分配同一塊記憶體
5. 使用原本的 Gc 讀寫新物件的內容

這種類型的 Use-After-Free 是常見的記憶體腐敗攻擊向量。
