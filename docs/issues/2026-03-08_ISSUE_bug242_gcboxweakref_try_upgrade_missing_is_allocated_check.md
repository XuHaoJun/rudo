# [Bug]: GcBoxWeakRef::try_upgrade 缺少 is_allocated 檢查導致潛在 Slot Reuse UAF

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 slot 被回收並重新分配才能觸發 |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free |
| **Reproducibility (復現難度)** | Medium | 需要觸發 lazy sweep 後再升級 weak reference |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::try_upgrade()` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcBoxWeakRef::try_upgrade()` 方法缺少 `is_allocated` 檢查，而同類的 `GcBoxWeakRef::upgrade()` 方法已有此檢查。這可能導致在 lazy sweep 回收 slot 並重新分配後，返回指向錯誤物件的 Gc指標。

### 預期行為 (Expected Behavior)
`try_upgrade()` 應該與 `upgrade()` 具有相同的安全檢查，在成功升級後檢查 `is_allocated` 以防止 slot reuse 問題。

### 實際行為 (Actual Behavior)
`try_upgrade()` 在以下位置缺少 `is_allocated` 檢查：
1. `try_inc_ref_from_zero()` 成功後 (ptr.rs:715-727)
2. `try_inc_ref_if_nonzero()` 成功後 (ptr.rs:732-745)

---

## 🔬 根本原因分析 (Root Cause Analysis)

對比 `GcBoxWeakRef::upgrade()` 和 `GcBoxWeakRef::try_upgrade()`:

**`upgrade()` 有 is_allocated 檢查 (ptr.rs:558-564):**
```rust
// Try atomic transition from 0 to 1 (resurrection)
if gc_box.try_inc_ref_from_zero() {
    // ...
    // Check is_allocated after successful upgrade to prevent slot reuse issues
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            GcBox::dec_ref(ptr.as_ptr());
            return None;
        }
    }
    return Some(Gc { ... });
}
```

**`try_upgrade()` 缺少此檢查 (ptr.rs:715-727):**
```rust
// Try atomic transition from 0 to 1 (same as regular upgrade)
if gc_box.try_inc_ref_from_zero() {
    // Second check: verify object wasn't dropped between check and CAS
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        let _ = gc_box;
        crate::ptr::GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
    // 缺少 is_allocated 檢查!!
    crate::gc::notify_created_gc();
    return Some(Gc {
        ptr: AtomicNullable::new(ptr),
        _marker: PhantomData,
    });
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立一個 Gc 物件並獲取其 GcBoxWeakRef
2. 執行 GC 觸發 lazy sweep 回收該 slot
3. 重新分配該 slot 為新物件
4. 調用 `GcBoxWeakRef::try_upgrade()` 
5. 預期：返回 None（因為 slot 已被重新分配）
6. 實際：可能返回指向新物件的 Gc（因為缺少 is_allocated 檢查）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBoxWeakRef::try_upgrade()` 的兩處成功升級後添加 `is_allocated` 檢查，與 `upgrade()` 方法一致：

1. 在 `try_inc_ref_from_zero()` 成功後 (line 722 之後)
2. 在 `try_inc_ref_if_nonzero()` 成功後 (line 740 之後)

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

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在 GC 後保持 dead slots 為 "allocated" 狀態直到重新分配。`is_allocated` 檢查確保在 slot reuse 場景下，不會返回指向新物件的 Gc。這與 standard GC 的 "forwarding" 機制類似。

**Rustacean (Soundness 觀點):**
缺少 `is_allocated` 檢查可能導致返回已釋放記憶體的指標，這是 Rust 中的 UB。即使機率較低，這是 soundness issue。

**Geohot (Exploit 觀點):**
攻擊者可以嘗試觸發特定 GC 時序來控制 slot reuse，進而實現任意記憶體讀寫。但需要精確控制 GC 時序，難度較高。
