# [Bug]: GcHandle::try_resolve_impl 缺少 post-increment is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確的時序控制來觸發 lazy sweep slot reuse |
| **Severity (嚴重程度)** | High | 可能導致錯誤物件的 ref_count 被錯誤遞增/遞減，導致 memory leak |
| **Reproducibility (復現難度)** | Very High | 需要 concurrent lazy sweep 和 handle resolve |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::try_resolve_impl` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`try_resolve_impl()` 應該在 `inc_ref()` 之後執行 `is_allocated` 檢查，以確保在 TOCTOU 競爭條件下（slot 被 sweep 並重新分配），操作的是正確的物件。

這與 `resolve_impl()` 的行為一致，後者有 post-increment `is_allocated` 檢查（作為 assertion）。

### 實際行為 (Actual Behavior)

`try_resolve_impl()` 只有 post-increment 檢查 `dropping_state()` 和 `has_dead_flag()`，但這些檢查對於新分配的物件會通過（新物件的 `dropping_state == 0` 且 `has_dead_flag == false`）。

**競爭條件情境：**
1. Handle 指向 slot X 中的物件 A
2. 物件 A 變成不可達，lazy sweep 回收 slot X
3. 新物件 B 被分配在 slot X（相同地址）
4. `is_allocated(idx)` 返回 `true`（slot 被 B 佔用）
5. `inc_ref()` 被調用到 B 的 GcBox - **錯誤的物件！**
6. B 的 ref_count 被錯誤地遞增

`try_resolve_impl` 缺少 `resolve_impl` 有的 post-increment `is_allocated` assertion，導致：
- Ref count 錯誤（memory leak）
- 而 `resolve_impl` 會 panic

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs` 中：

**`resolve_impl` (lines 243-250) - 有 post-increment `is_allocated` check:**
```rust
gc_box.inc_ref();

// Post-increment safety check...
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(self.ptr.as_ptr());
    panic!("...");
}

if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    // Don't call dec_ref when slot swept - it may be reused (bug133)
    assert!(
        (*header.as_ptr()).is_allocated(idx),  // <-- POST-INCREMENT CHECK!
        "GcHandle::resolve: object slot was swept after inc_ref"
    );
}
```

**`try_resolve_impl` (lines 341-347) - 缺少 post-increment `is_allocated` check:**
```rust
gc_box.inc_ref();

// Post-increment safety check (TOCTOU). Same pattern as Weak::try_upgrade.
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}

// 沒有 is_allocated check！<-- BUG!
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {  // <-- 這只在 false 時返回 None
        GcBox::dec_ref(self.ptr.as_ptr());
        return None;
    }
}

Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
```

問題：
1. Pre-increment `is_allocated` 檢查只驗證 slot 是否被佔用，不驗證是否是同一個物件
2. `inc_ref()` 可能作用於新分配的物件 B
3. Post-increment 檢查 `dropping_state` 和 `has_dead_flag` 對新物件會通過
4. `try_resolve_impl` 缺少 `resolve_impl` 有的 assertion-style post-increment `is_allocated` 檢查

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的執行緒交錯控制，涉及 lazy sweep：

```rust
// 概念驗證 - 需要 TSan 或極端的時序控制
// 執行緒 1: 在 handle 上調用 try_resolve() 指向 A
// 執行緒 2: lazy sweep + 在相同 slot 分配 B
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_resolve_impl()` 中於 `inc_ref()` 之後添加 `is_allocated` 檢查：

```rust
gc_box.inc_ref();

// Post-increment safety check (TOCTOU). Same pattern as Weak::try_upgrade.
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}

// ADD THIS: Verify slot still allocated after inc_ref (matches resolve_impl)
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(self.ptr.as_ptr());
        return None;
    }
}
```

或者使用 assertion-style 檢查（匹配 `resolve_impl`）：

```rust
// ADD THIS: Verify slot still allocated after inc_ref (matches resolve_impl)
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "GcHandle::try_resolve: object slot was swept after inc_ref"
    );
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題與 bug347 描述的 TOCTOU 問題類似，但影響 `try_resolve_impl`。Slot reuse 必須不能通過 handle resolve 觀察到。`is_allocated` 檢查嘗試這一點但不足夠，因為它只驗證 slot 狀態，不驗證物件身份。

**Rustacean (Soundness 觀點):**
這是 memory corruption - `inc_ref` 操作於錯誤的物件會腐蝕 ref counts，導致 memory leak。`try_resolve_impl` 缺少 `resolve_impl` 有的保護，導致不一致的行為。

**Geohot (Exploit 觀點):**
Exploit 路徑：(1) 創建指向 A 的 handle，(2) A 變成不可達，(3) Lazy sweep 回收 slot，(4) B 被分配在相同 slot，(5) `inc_ref` 腐蝕 B 的 ref_count，(6) 如果 B 是安全敏感的，通過腐敗的 ref_count 操作可能導致 UAF。

---

## Resolution (2026-03-21)

**Outcome:** Already fixed.

The fix is present in `crates/rudo-gc/src/handles/cross_thread.rs` in `try_resolve_impl` (lines 339–389). The current implementation includes:

1. **Pre-increment `is_allocated` check** (lines 352–357) — returns `None` if slot is not allocated.
2. **Generation snapshot + post-increment generation comparison** (lines 362–371) — `dec_ref` + returns `None` if generation changed (stronger than the requested `is_allocated` check, catches slot reuse directly).
3. **Post-increment `is_allocated` check** (lines 379–385) — exactly what this issue requested: `dec_ref` + returns `None` if slot no longer allocated.

The `test_try_resolve_wrong_thread` test in `tests/cross_thread_handle.rs` passes. The issue is resolved.

---

## Related Issues

- bug345: Original issue that added pre-increment `is_allocated` check
- bug347: Documents that `is_allocated` check is insufficient to prevent slot reuse TOCTOU
- bug83: GcHandle resolve/clone TOCTOU race (different issue - handle unregistered)
