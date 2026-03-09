# [Bug]: Gc::try_deref 缺少 is_allocated 檢查可能導致類型混淆

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 deref 並發 |
| **Severity (嚴重程度)** | Critical | 類型混淆導致讀取錯誤資料 |
| **Reproducibility (復現難度)** | High | 需要並發 lazy sweep + deref |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::try_deref` in `crates/rudo-gc/src/ptr.rs`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`try_deref` 應該在解引用前驗證 slot 尚未被 sweep 並重新分配，以防止讀取已釋放或被重用的記憶體。

### 實際行為 (Actual Behavior)
`try_deref` (ptr.rs:1276-1291) 只檢查 `has_dead_flag`、`dropping_state` 和 `is_under_construction`，但沒有檢查 `is_allocated`。

對比 `Gc::clone` (ptr.rs:1691-1697) 正確地檢查了 `is_allocated`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

```rust
// Gc::try_deref (BUG - 缺少 is_allocated 檢查)
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag()
            || (*gc_box_ptr).dropping_state() != 0
            || (*gc_box_ptr).is_under_construction()
        {
            return None;
        }
        // 缺少 is_allocated 檢查！
        Some(&(*gc_box_ptr).value)
    }
}

// Gc::clone (CORRECT - 有 is_allocated 檢查)
fn clone(&self) -> Self {
    // ...
    unsafe {
        (*gc_box_ptr).inc_ref();
        // 正確檢查 is_allocated
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                GcBox::dec_ref(gc_box_ptr);
                panic!("Gc::clone: object slot was swept after inc_ref");
            }
        }
    }
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 lazy sweep
2. 建立 `Gc<T>` 物件
3. 丟棄 `Gc<T>` 讓物件變成候選 sweep 對象
4. 在 lazy sweep 進行的同時，呼叫 `Gc::try_deref()` 或 `try_deref(&gc)`
5. Race condition: slot 可能被 sweep 並重用，導致讀取到錯誤類型的資料

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_deref` 中加入 `is_allocated` 檢查：

```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag()
            || (*gc_box_ptr).dropping_state() != 0
            || (*gc_box_ptr).is_under_construction()
        {
            return None;
        }
        
        // 新增: 檢查 slot 是否仍被分配
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                return None;
            }
        }
        
        Some(&(*gc_box_ptr).value)
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在物件被丟棄後回收 slot。如果 slot 被回收並重新分配給不同類型的物件，則解引用可能導致類型混淆。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。讀取已釋放或重用的記憶體可能導致 undefined behavior。

**Geohot (Exploit 觀點):**
攻擊者可能利用此漏洞：
1. 建立一個 GC 物件
2. 讓物件被回收並 slot 被重用
3. 透過 try_deref 讀取新物件的記憶體，造成資訊洩漏

