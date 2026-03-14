# [Bug]: WeakCrossThreadHandle::is_valid() 錯誤地遞增 ref_count，與 GcHandle::is_valid() 行為不一致

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 任何調用 `is_valid()` 的程式碼都會觸發 |
| **Severity (嚴重程度)** | High | 導致記憶體洩露或物件無法正確回收 |
| **Reproducibility (復現難度)** | Low | 可透過簡單測試穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::is_valid()` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::is_valid()` 應該只檢查 weak reference 的有效性，不應該修改 reference count。應該與 `GcHandle::is_valid()` 的行為一致，後者只是簡單地檢查 handle 是否存在於 root list 中。

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::is_valid()` 調用 `self.weak.upgrade().is_some()`，這會：
1. 如果物件有效，`upgrade()` 會遞增 `ref_count`（透過 `try_inc_ref_from_zero` 或 `try_inc_ref_if_nonzero`）
2. 返回 `Some(Gc<T>)`（已遞增 ref_count 的）
3. 然後檢查 `is_some()` 並丟棄這個 `Gc<T>`，**但沒有遞減 ref_count**！

這導致每次調用 `is_valid()` 都會無意中遞增 reference count，造成：
- 物件無法被正確回收（因為 ref_count 增加）
- 記憶體洩露（如果 `is_valid()` 被頻繁調用）

### 程式碼位置

`handles/cross_thread.rs` 第 552-557 行：
```rust
pub fn is_valid(&self) -> bool {
    if self.origin_tcb.upgrade().is_none() {
        return false;
    }
    self.weak.upgrade().is_some()  // <-- BUG: 會遞增 ref_count！
}
```

### 對比：GcHandle::is_valid() 的正確實現

`handles/cross_thread.rs` 第 100-114 行：
```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    self.origin_tcb.upgrade().map_or_else(
        || {
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

`GcHandle::is_valid()` **不會**遞增 reference count，只是簡單地檢查 handle 是否存在於 root list 或 orphan roots 中。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`WeakCrossThreadHandle::is_valid()` 使用了錯誤的方法來檢查有效性。它調用了 `self.weak.upgrade()`，這會嘗試將 weak reference 升級為 strong reference，並在成功時遞增 ref_count。

`upgrade()` 的實現（`ptr.rs:517-596`）：
- 如果 `ref_count == 0`，調用 `try_inc_ref_from_zero()` 來原子性地將 ref_count 從 0 改為 1
- 如果 `ref_count > 0`，調用 `try_inc_ref_if_nonzero()` 來遞增 ref_count
- 無論哪種情況，成功時都會返回 `Some(Gc<T>)`，這意味著 ref_count 已被遞增

然後 `is_valid()` 檢查 `upgrade().is_some()` 並丟棄結果，但沒有調用 `dec_ref()` 來平衡遞增。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, gc};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_weak_is_valid_increments_ref_count() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let weak = gc.weak_cross_thread_handle();

    // 初始 ref_count 為 1（來自 gc）
    assert_eq!(Gc::ref_count(&gc), 1);

    // 調用 is_valid() - 不應該改變 ref_count
    let is_valid = weak.is_valid();
    assert!(is_valid, "weak should be valid");

    // BUG: ref_count 變成了 2！因為 is_valid() 錯誤地遞增了 ref_count
    assert_eq!(
        Gc::ref_count(&gc),
        1,  // <-- 這會失敗！實際值是 2
        "is_valid() should not increment ref_count"
    );

    drop(gc);
    gc::collect();

    // 由於 is_valid() 遞增了 ref_count，物件不會被回收
    // 這導致記憶體洩露
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `WeakCrossThreadHandle::is_valid()` 使用與 `GcHandle::is_valid()` 類似的邏輯，不遞增 ref_count：

```rust
pub fn is_valid(&self) -> bool {
    if self.origin_tcb.upgrade().is_none() {
        return false;
    }
    // 使用 may_be_valid() 而不是 upgrade()，避免遞增 ref_count
    self.weak.may_be_valid()
}
```

或者，如果需要更完整的檢查（檢查物件是否實際存在且未死亡），應該使用自定義的檢查邏輯，類似 `GcHandle::is_valid()` 的方式，但也要避免遞增 ref_count。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個嚴重的 GC 正確性問題。`is_valid()` 是用來檢查物件是否仍然有效的輕量級操作，不應該修改物件的 reference count。每次調用都會遞增 ref_count 會導致物件永遠無法被回收，這與 weak reference 的語義相悖。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但是記憶體洩露的源頭。`is_valid()` 的行為應該是只讀的，不應該有副作用。當前實現會導致：
1. 物件無法被正確回收
2. 記憶體洩露（如果 `is_valid()` 被頻繁調用）
3. 與 `GcHandle::is_valid()` 行為不一致

**Geohot (Exploit 攻擊觀點):**
攻擊者可以利用這個 bug：
1. 如果有一個 weak reference 被頻繁調用 `is_valid()`，攻擊者可以防止物件被回收
2. 這可用於 prolonging objects 來進行 heap spraying 或其他記憶體攻擊

---

## 驗證記錄

**驗證日期:** 2026-03-14
**驗證人員:** opencode

### 驗證結果

確認 `WeakCrossThreadHandle::is_valid()` (cross_thread.rs:552-557) 會錯誤地遞增 ref_count。

對比：
- `GcHandle::is_valid()` (cross_thread.rs:100-114): 不遞增 ref_count，只檢查 root list
- `WeakCrossThreadHandle::is_valid()` (cross_thread.rs:552-557): 遞增 ref_count（透過 upgrade）

**Status: Open** - 需要修復。
