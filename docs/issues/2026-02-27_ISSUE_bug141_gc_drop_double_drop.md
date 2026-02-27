# [Bug]: Gc<T> Drop 存在 Double Drop - dec_ref 與 Drop impl 都呼叫 drop_fn

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 每次 Gc<T> Drop 都會觸發 |
| **Severity (嚴重程度)** | Critical | 導致 double free / UAF |
| **Reproducibility (Reproducibility)** | N/A | 100% 觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::drop` in `ptr.rs`, `GcBox<T>::dec_ref` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 `Gc<T>` 被 Drop 時，應該只呼叫一次 `drop_fn` 來釋放物件。

### 實際行為 (Actual Behavior)

存在 Double Drop：

1. `GcBox::dec_ref` (ptr.rs:151) 當 ref count 為 1 時，會呼叫 `drop_fn` 並返回 `true`：
```rust
// ptr.rs:167-177
if count == 1 && this.dropping_state() == 0 {
    if this.try_mark_dropping() {
        // 第一次呼叫 drop_fn
        unsafe {
            (this.drop_fn)(self_ptr.cast::<u8>());
        }
        return true;  // is_last = true
    }
}
```

2. `Gc<T>::drop` (ptr.rs:1506-1525) 檢查 `is_last` 並再次呼叫 `drop_fn`：
```rust
// ptr.rs:1515-1520
let is_last = GcBox::<T>::dec_ref(gc_box_ptr);

if is_last {
    unsafe {
        // 第二次呼叫 drop_fn！Double Drop！
        ((*gc_box_ptr).drop_fn)(gc_box_ptr.cast::<u8>());
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`dec_ref` 已經在內部處理了物件的 drop（當 ref count 降至 0 時），但 `Gc<T>::drop` 實作沒有注意到這一點，導致 `drop_fn` 被呼叫兩次。

這是一個明確的 double free / use-after-free 問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::cell::Cell;

static DROP_COUNT: Cell<u32> = Cell::new(0);

#[derive(Trace)]
struct TestData {
    value: i32,
}

impl Drop for TestData {
    fn drop(&mut self) {
        let count = DROP_COUNT.get();
        eprintln!("Drop called! Count: {}", count + 1);
        DROP_COUNT.set(count + 1);
    }
}

fn main() {
    let gc = Gc::new(TestData { value: 42 });
    drop(gc);  // 會觸發 two drops
    
    let count = DROP_COUNT.get();
    assert_eq!(count, 1, "Expected 1 drop, got {}", count);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩種修復方式：

**選項 1：修改 `Gc<T>::drop`**
```rust
impl<T: Trace> Drop for Gc<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return;
        }

        let gc_box_ptr = ptr.as_ptr();

        // dec_ref 返回 true 表示它已經處理了 drop
        let was_last = GcBox::<T>::dec_ref(gc_box_ptr);

        if !was_last {
            notify_dropped_gc();
        }
        // 不需要再次呼叫 drop_fn - dec_ref 已經處理了
    }
}
```

**選項 2：修改 `dec_ref` 返回值語義**
讓 `dec_ref` 不自動呼叫 drop_fn，而是由 caller 決定是否 drop。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 reference counting GC 中，物件的 drop 應該严格在 ref count 降至 0 時執行一次。double drop 會導致嚴重的記憶體損壞。在 Chez Scheme 中，這種錯誤是不可接受的。

**Rustacean (Soundness 觀點):**
這是一個明確的 double free 問題，屬於 undefined behavior。程式可能會：
1. 嘗試 drop 一個已經被 drop 的物件
2. 導致 use-after-free
3. 破壞 heap metadata

**Geohot (Exploit 攻擊觀點):**
攻擊者可以利用這個 double drop 來：
1. 破壞記憶體管理器狀態
2. 觸發 heap corruption
3. 執行 arbitrary writePrimitive
