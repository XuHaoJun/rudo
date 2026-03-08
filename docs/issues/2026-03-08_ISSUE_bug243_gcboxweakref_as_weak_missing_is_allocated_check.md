# [Bug]: GcBoxWeakRef::as_weak 缺少 is_allocated 檢查導致 TOCTOU

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 GcBoxWeakRef::as_weak 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Medium | 可能導致 weak count 增加在錯誤的 slot 上，導致記憶體洩漏或 UAF |
| **Reproducibility (復現難度)** | Medium | 需要並發執行才能穩定重現，單執行緒難以觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::as_weak()` (ptr.rs:1511-1533)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcBoxWeakRef::as_weak()` 方法缺少 `is_allocated` 檢查，而同類的 `GcBox::as_weak()` 和 `Gc::downgrade()` 方法已有此檢查。這可能導致在 lazy sweep 回收 slot 並重新分配後，weak count 錯誤地增加在新物件上。

### 預期行為 (Expected Behavior)
`GcBoxWeakRef::as_weak()` 應該在 `inc_weak()` 之後檢查 `is_allocated()`，與 `Gc::downgrade()` 的模式一致，防止 TOCTOU race。

### 實際行為 (Actual Behavior)
`GcBoxWeakRef::as_weak()` 在驗證檢查後直接呼叫 `inc_weak()`，沒有檢查 slot 是否仍然 allocated。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1511-1533`，`GcBoxWeakRef::as_weak()` 的實作如下：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let Some(ptr) = ptr.as_option() else {
        return GcBoxWeakRef { ptr: AtomicNullable::null() };
    };
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.is_under_construction()
            || gc_box.has_dead_flag()
            || gc_box.dropping_state() != 0
        {
            return GcBoxWeakRef { ptr: AtomicNullable::null() };
        }
        (*ptr.as_ptr()).inc_weak();  // 缺少 is_allocated 檢查
    }
    GcBoxWeakRef { ptr: AtomicNullable::new(ptr) }
}
```

對比 `Gc::downgrade()` (ptr.rs:1462-1486) 有正確的模式：

```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    // ...
    (*gc_box_ptr).inc_weak();

    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            (*gc_box_ptr).dec_weak();
            panic!("Gc::downgrade: slot was swept during downgrade");
        }
    }
    // ...
}
```

問題在於：驗證檢查 (is_under_construction, has_dead_flag, dropping_state) 與 `inc_weak()`言之間存在 TOCTOU window。在 lazy sweep 回收 slot 並重新分配給新物件後，`inc_weak()` 會錯誤地增加新物件的 weak count。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立一個 `GcBoxWeakRef` 指向某物件
2. 在另一執行緒觸發 lazy sweep 回收該 slot
3. 在 sweep 完成後、slot 重新分配前，呼叫 `GcBoxWeakRef::as_weak()`
4. 觀察 weak count 是否錯誤地增加

```rust
// PoC 需要並發執行才能穩定重現
// 單執行緒版本僅供參考
fn main() {
    // 1. Create Gc and GcBoxWeakRef
    // 2. Trigger lazy sweep to reclaim the slot
    // 3. Call GcBoxWeakRef::as_weak()
    // 4. Observe incorrect weak count
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBoxWeakRef::as_weak()` 的 `inc_weak()` 之後添加 `is_allocated` 檢查，與 `Gc::downgrade()` 的模式一致：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let Some(ptr) = ptr.as_option() else {
        return GcBoxWeakRef { ptr: AtomicNullable::null() };
    };
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.is_under_construction()
            || gc_box.has_dead_flag()
            || gc_box.dropping_state() != 0
        {
            return GcBoxWeakRef { ptr: AtomicNullable::null() };
        }
        (*ptr.as_ptr()).inc_weak();

        // Add is_allocated check after inc_weak
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                (*ptr.as_ptr()).dec_weak();
                return GcBoxWeakRef {
                    ptr: AtomicNullable::null(),
                };
            }
        }
    }
    GcBoxWeakRef { ptr: AtomicNullable::new(ptr) }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
lazy sweep 的非確定性使得在 `inc_weak()` 後檢查 slot 是否仍然 allocated 變得必要。與 `Gc::downgrade()` 保持一致的模式可以確保 GC 的正確性。

**Rustacean (Soundness 觀點):**
缺少 `is_allocated` 檢查可能導致 UB - weak count 被錯誤地增加在已回收的 slot 上。這類似於其他 TOCTOU 問題，需要在關鍵操作後進行驗證。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 lazy sweep 的時機，可能利用此 TOCTOU 漏洞造成 memory corruption 或資訊洩露。

---

## ✅ 驗證記錄 (Verification Record)

**驗證日期:** 2026-03-08
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `crates/rudo-gc/src/ptr.rs:1511-1533`:

1. `GcBoxWeakRef::as_weak()` 在調用 `inc_weak()` 後沒有檢查 `is_allocated()`
2. 對比 `Gc::downgrade()` (ptr.rs:1475-1481) 有正確的 `is_allocated` 檢查
3. 此不一致導致 TOCTOU race：lazy sweep 回收 slot 後，weak count 可能錯誤地增加在新物件上
