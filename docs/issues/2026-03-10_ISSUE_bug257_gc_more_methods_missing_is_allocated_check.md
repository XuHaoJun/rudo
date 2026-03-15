# [Bug]: Gc::try_clone/as_ptr/internal_ptr/as_weak 缺少 is_allocated 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與並發存取同時發生 |
| **Severity (嚴重程度)** | Critical | 類型混淆導致讀取錯誤資料 / UAF |
| **Reproducibility (復現難度)** | High | 需要並發 lazy sweep + 存取 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Multiple Gc methods in `crates/rudo-gc/src/ptr.rs`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為
所有解引用 Gc 指標的方法都應該在存取前驗證 slot 尚未被 sweep 並重新分配，以防止讀取已釋放或被重用的記憶體。

### 實際行為
以下方法缺少 `is_allocated` 檢查，與 bug255 中描述的 `try_deref` 問題相同：

1. **`Gc::try_clone`** (ptr.rs:1296-1326) - 缺少 is_allocated 檢查
2. **`Gc::as_ptr`** (ptr.rs:1336-1349) - 缺少 is_allocated 檢查  
3. **`Gc::internal_ptr`** (ptr.rs:1352-1368) - 缺少 is_allocated 檢查
4. **`Gc::as_weak`** (ptr.rs:1511-1533) - 缺少 is_allocated 檢查
5. **`Gc::weak_cross_thread_handle`** (ptr.rs:1619-1638) - 缺少 is_allocated 檢查

對比以下已正確實作的方法：
- `Gc::deref` (ptr.rs:1649-1655) - 有 is_allocated 檢查
- `Gc::downgrade` (ptr.rs:1475-1481) - 有 is_allocated 檢查  
- `Gc::cross_thread_handle` (ptr.rs:1580-1586) - 有 is_allocated 檢查
- `Weak::clone` (ptr.rs:2201-2209) - 有 is_allocated 檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

這些方法只檢查以下狀態：
- `has_dead_flag()`
- `dropping_state()`
- `is_under_construction()`

但**沒有**檢查 `is_allocated`，導致在 lazy sweep 期間 slot 被回收並重用時，可能發生：
- 類型混淆：讀取到新物件的資料
- UAF：解引用已釋放的記憶體

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 lazy sweep
2. 建立 `Gc<T>` 物件
3. 丟棄 `Gc<T>` 讓物件變成候選 sweep 對象
4. 在 lazy sweep 進行的同時，呼叫 `try_clone()` / `as_ptr()` / `internal_ptr()` / `as_weak()`
5. Race condition: slot 可能被 sweep 並重用

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在每個受影響的方法中加入 `is_allocated` 檢查，參考 `Gc::deref` 的實作：

```rust
// Gc::try_clone 修復範例
pub fn try_clone(gc: &Self) -> Option<Self> {
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
        
        // ... 原有邏輯 ...
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在物件被丟棄後回收 slot。如果 slot 被回收並重新分配給不同類型的物件，則解引用可能導致類型混淆。此問題影響多個方法，比 bug255 報告的更廣泛。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。讀取已釋放或重用的記憶體可能導致 undefined behavior。多個方法都受影響，需要全面修復。

**Geohot (Exploit 觀點):**
攻擊者可能利用這些漏洞：
1. 建立一個 GC 物件
2. 讓物件被回收並 slot 被重用
3. 透過 try_clone/as_ptr/internal_ptr/as_weak 讀取新物件的記憶體

---

## Resolution (2026-03-15)

Fixed in `crates/rudo-gc/src/ptr.rs`. Investigation showed that `try_clone`, `as_ptr`, and `internal_ptr` already had `is_allocated` checks. Added pre-`inc_weak` `is_allocated` checks to:
- `Gc::as_weak()` — returns null `GcBoxWeakRef` if slot was swept
- `Gc::weak_cross_thread_handle()` — asserts before `inc_weak` (matches `downgrade` pattern)

All tests pass.
