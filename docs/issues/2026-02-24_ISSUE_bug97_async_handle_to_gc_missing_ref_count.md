# [Bug]: AsyncHandle::to_gc() 漏增引用計數導致雙重釋放

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 使用 AsyncHandle::to_gc() 逃逸 handle 時會觸發 |
| **Severity (嚴重程度)** | Critical | 導致雙重釋放 (double-free) 或 use-after-free |
| **Reproducibility (Reproducibility)** | High | 每次使用 to_gc() 都會觸發問題 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::to_gc()` in `crates/rudo-gc/src/handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`AsyncHandle::to_gc()` 應該遞增 `GcBox` 的引用計數，確保返回的 `Gc<T>` 擁有獨立的擁有權。

### 實際行為 (Actual Behavior)
`AsyncHandle::to_gc()` 直接從原始指標創建 `Gc<T>` 而不遞增引用計數，導致：
1. 原始 `Gc` 熄滅時會釋放物件
2. 逃逸的 `Gc` 熄滅時會錯誤地再次嘗試釋放，造成雙重釋放

---

## 🔬 根本原因分析 (Root Cause Analysis)

比較同步版本 (`Handle::to_gc()` at `src/handles/mod.rs:340-348`)：

```rust
pub fn to_gc(&self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        let gc: Gc<T> = Gc::from_raw(ptr);
        let gc_clone = gc.clone();  // ✅ 正確遞增 ref_count
        std::mem::forget(gc);       // 忘記原始 Gc，避免遞減
        gc_clone
    }
}
```

異步版本 (`AsyncHandle::to_gc()` at `src/handles/async.rs:662-667`)：

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        Gc::from_raw(ptr)  // ❌ 沒有遞增 ref_count!
    }
}
```

`Gc::from_raw()` 只會包裝指標，不會修改引用計數。這導致返回的 `Gc<T>` 與底層物件的引用計數不同步。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;
use rudo_gc::heap::with_heap_and_tcb_arc;

#[derive(Trace)]
struct TestData { value: i32 }

fn main() {
    with_heap_and_tcb_arc(|_, tcb| {
        let scope = AsyncHandleScope::new(tcb);
        let gc = Gc::new(TestData { value: 42 });
        
        // handle 不遞增 ref_count
        let handle = scope.handle(&gc);
        
        // to_gc 也不遞增 ref_count - BUG!
        let gc2 = handle.to_gc();
        
        // 熄滅 gc: ref_count 從 1 -> 0，物件被釋放
        drop(gc);
        
        // 熄滅 gc2: ref_count 已經是 0，嘗試再次釋放 - 雙重釋放!
        drop(gc2);
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `AsyncHandle::to_gc()` 與同步版本一致：

```rust
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let ptr = (*self.slot).as_ptr() as *const u8;
        let gc: Gc<T> = Gc::from_raw(ptr);
        let gc_clone = gc.clone();  // 遞增 ref_count
        std::mem::forget(gc);       // 避免遞減
        gc_clone
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 引用計數必須與所有強引用保持同步
- 每個 `Gc<T>` 應該對應一個 ref_count 的擁有權
- 逃逸的 handle 實質上是複製了一個擁有權，必須遞增計數

**Rustacean (Soundness 觀點):**
- 雙重釋放是未定義行為 (UB)
- `dec_ref` 在 count == 0 時返回 true，但實際上 gc2 並不擁有最後一個引用

**Geohot (Exploit 觀點):**
- 雙重釋放可用於堆溢位利用
- 在多執行緒環境中，此問題可能導致 race condition 可被利用
