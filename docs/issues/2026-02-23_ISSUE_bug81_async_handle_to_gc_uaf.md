# [Bug]: AsyncHandle::to_gc 缺少 ref count 增量與 dead check 導致 UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 async scope 內使用 to_gc 將 handle 轉換為 Gc |
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

`AsyncHandle::to_gc` 應該與 `Handle::to_gc` 行為一致：
1. 返回的 Gc 應該遞增 ref count，使其能夠獨立於 AsyncHandleScope 存活
2. 應該檢查物件是否已死亡或正在 dropping

`Handle::to_gc` (handles/mod.rs:340-348) 的正確實作：
```rust
pub fn to_gc(&self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        let gc: Gc<T> = Gc::from_raw(ptr);
        let gc_clone = gc.clone();  // 透過 clone 遞增 ref count 並檢查 dead state
        std::mem::forget(gc);
        gc_clone
    }
}
```

### 實際行為

`AsyncHandle::to_gc` (handles/async.rs:655-660) 有兩個問題：
1. 沒有遞增 ref count
2. 沒有檢查 dead flag / dropping state

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        Gc::from_raw(ptr)  // 缺少 ref count 增量與 dead check！
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/async.rs:655-660` (`AsyncHandle::to_gc`)

1. **缺少 ref count 增量：** 
   - `Handle::to_gc` 使用 `gc.clone()` 來創建返回的 Gc，而 `Gc::clone()` 內部會遞增 ref count
   - `AsyncHandle::to_gc` 直接調用 `Gc::from_raw`，跳過了 ref count 增量
   - 當 AsyncHandleScope 被 drop 時，物件可能被回收，導致 UAF

2. **缺少 dead check：**
   - `Handle::to_gc` 的 `clone()` 會檢查 `has_dead_flag()` 和 `dropping_state()`
   - `AsyncHandle::to_gc` 沒有這些檢查
   - 可能返回已死亡或正在 dropping 的 Gc

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
    println!("Value: {}", gc1.value); // 未定義行為！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `handles/async.rs:655-660` 的 `AsyncHandle::to_gc` 實作：

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        let gc: Gc<T> = Gc::from_raw(ptr);
        
        // 檢查物件狀態（參考 Handle::to_gc 的 clone() 行為）
        let gc_clone = gc.clone();  // 遞增 ref count 並檢查 dead state
        std::mem::forget(gc);       // 防止 drop 遞減 ref count
        gc_clone
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 GC 記憶體管理錯誤。從 handle 轉換為獨立的 Gc 時，必須遞增 ref count 否則物件會被錯誤回收。與 Handle::to_gc 的行為不一致會造成 API 語意混亂。

**Rustacean (Soundness 觀點):**
這是明確的記憶體安全問題 - Use-After-Free。`Gc::from_raw` 的文檔明確說明返回的指標必須是有效的，但這裡沒有正確的 ref count 來保持有效性。

**Geohot (Exploit 攻擊觀點):**
這個 bug 可以被利用來進行記憶體佈局攻擊。通過控制 GC 時序，可以實現 Use-After-Free 並進一步進行記憶體操縱。

---

## 關聯 Issue

- bug70: AsyncHandle::to_gc 缺少 dead_flag 檢查
- bug80: AsyncHandle::to_gc 缺少 ref count 增量

本 issue 合併這兩個相關問題，提供完整的修復方案。

---

## Resolution (2026-02-26)

**Outcome:** Already fixed (same as bug70 + bug80).

The current `AsyncHandle::to_gc` in `handles/async.rs` (lines 671–686) correctly implements both fixes: (1) `gc_box.inc_ref()` before `Gc::from_raw`, (2) asserts for `has_dead_flag()`, `dropping_state()`, and `is_under_construction()`. No code changes required.
