# [Bug]: Weak::upgrade 缺少 is_allocated 檢查 - 與 GcBoxWeakRef::upgrade 行為不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 lazy sweep 與升級發生時序競爭 |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade()`, `Weak::try_upgrade()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Weak::upgrade()` 和 `Weak::try_upgrade()` 應該在成功升級後檢查 `is_allocated`，以防止 lazy sweep 回收並重用 slot 導致的問題。這與 `GcBoxWeakRef::upgrade()` 的行為一致。

### 實際行為 (Actual Behavior)

`Weak::upgrade()` 和 `Weak::try_upgrade()` 在成功 CAS 增強 ref_count 後，只檢查 `dropping_state` 和 `has_dead_flag`，但沒有檢查 `is_allocated`。

### 程式碼位置

`ptr.rs` 第 1909-1927 行（Weak::upgrade 的成功升級後處理）：
```rust
if gc_box
    .ref_count
    .compare_exchange_weak(...)
    .is_ok()
{
    // Post-CAS safety check
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    // BUG: 缺少 is_allocated 檢查！！！
    crate::gc::notify_created_gc();
    return Some(Gc { ... });
}
```

### 對比：GcBoxWeakRef::upgrade 的正確實現

`ptr.rs` 第 558-565 行：
```rust
// Check is_allocated after successful upgrade to prevent slot reuse issues
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `Weak::upgrade()` 函數中，當 ref_count 的 CAS 成功後，程式碼檢查了 `dropping_state` 和 `has_dead_flag`，但缺少 `is_allocated` 檢查。

這與 `GcBoxWeakRef::upgrade()` 的實現不一致，後者正確地包含了 `is_allocated` 檢查。

在 lazy sweep 運行的情況下，可能存在以下時序：
1. Weak::upgrade 讀取 ref_count
2. Lazy sweep 回收並重用該 slot
3. Weak::upgrade 成功 CAS ref_count
4. 由於缺少 is_allocated 檢查，返回可能已回收的 Gc

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制來穩定重現此問題。以下是概念驗證：

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    drop(gc);
    
    // 同時觸發 lazy sweep 和 weak upgrade
    // 需要精確的時序控制才能穩定重現
    
    let upgraded = weak.upgrade();
    // 可能返回已回收的 Gc
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::upgrade()` 和 `Weak::try_upgrade()` 的成功 CAS 後，添加 `is_allocated` 檢查：

```rust
// Weak::upgrade() 修復
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(ptr.as_ptr());
    return None;
}

// 新增：檢查 is_allocated 防止 slot 被 lazy sweep 回收並重用
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
}

crate::gc::notify_created_gc();
return Some(Gc { ... });
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 lazy sweep 與 weak upgrade 時序問題的經典案例。所有成功升級后都應該檢查 slot 是否仍然有效，以確保返回的 Gc 指向有效的記憶體。

**Rustacean (Soundness 觀點):**
這可能導致 Use-After-Free，儘管在最壞情況下（slot 被重用作為不同類型的物件）可能導致類型混淆。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠控制 lazy sweep 的時序，可能利用此漏洞進行記憶體利用。但此攻擊需要精確的時序控制，實際利用難度較高。

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
