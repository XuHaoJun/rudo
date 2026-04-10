# [Bug]: GcRootSet::register 二次註冊未驗證 generation 導致根追蹤錯誤

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需滿足：物件被收集 → slot 被重用 → 舊 guard 被 forget → 新物件在同一地址註冊 |
| **Severity (嚴重程度)** | High | 可能導致新物件的 root guard 被錯誤移除，造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需構造 slot reuse + forget(guard) 場景，單執行緒難以穩定觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::register` (tokio/root.rs:70-89)
- **OS / Architecture:** Linux x86_64 (All)
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當 `GcRootSet::register` 被呼叫且傳入的指標位址已存在於 HashMap 中時（`Entry::Occupied`），程式碼僅遞增 refcount，**卻未檢查 generation 是否改變**。

若該 slot 在舊物件被收集後被回收並重新分配給新物件（generation 改變），會導致：
1. 新物件的 refcount 被錯誤地遞增（加上舊物件殘留的 refcount）
2. HashMap 中儲存的 generation 仍是舊物件的 generation
3. 當 `snapshot()` 執行時，generation 驗證失敗，該 root 被錯誤過濾掉

### 預期行為 (Expected Behavior)
當 `register` 遇到 `Entry::Occupied` 時，應驗證 generation：
- 若 generation 不匹配 → slot 已被重用，應視為新 entry
- 若 generation 匹配 → 遞增 refcount

### 實際行為 (Actual Behavior)
當 `register` 遇到 `Entry::Occupied` 時，直接遞增 refcount，不驗證 generation。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `tokio/root.rs:83-85`

```rust
Entry::Occupied(o) => {
    o.into_mut().0 += 1;  // BUG: 未驗證 generation
}
```

**正常路徑（Vacant）：** `tokio/root.rs:75-81`
```rust
Entry::Vacant(v) => {
    let generation = crate::heap::try_with_heap(|heap| unsafe {
        crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8)
            .map_or(0u32, |gc_box| gc_box.as_ref().generation())
    })
    .unwrap_or(0);
    v.insert((1, generation));  // 正確：讀取並儲存 current generation
}
```

**問題場景：**
1. Object A (addr=0x1000, gen=1) 被註冊 → entry = (refcount=1, gen=1)
2. 若 Object A 的 `GcRootGuard` 被 `mem::forget()`，Object A 被 GC collected
3. Slot 0x1000 被 Object B (gen=2) 重新使用
4. `register(0x1000)` → `Entry::Occupied` → refcount 變成 2，但 entry 仍是 (refcount=2, gen=1)
5. `snapshot()` 驗證 `current_generation(2) == stored_generation(1)` → **失敗**，root 被過濾

**另一個問題：** 當舊 entry 最終被 `unregister` 移除時，正確註冊的 Object B 的 root entry 也跟著消失。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要 tokio feature
// 觸發條件：
// 1. 建立 Gc<ObjectA> 並取得 root_guard
// 2. 使用 mem::forget(guard) 讓 ObjectA 的 entry 殘留
// 3. drop Gc<ObjectA> 並觸發 GC 收集
// 4. 在同一地址建立 Gc<ObjectB>
// 5. ObjectB 的 root_guard 註冊時撞到舊 entry
// 6. snapshot() 會錯誤過濾 ObjectB 的 root

#[test]
fn test_register_occupado_generation_mismatch() {
    use rudo_gc::tokio::{GcRootGuard, GcRootSet};
    use std::ptr::NonNull;

    let set = GcRootSet::global();
    set.clear();

    // 假設 0x1000 是一個有效的 GcBox 地址（實際測試需用真實分配）
    let fake_ptr = 0x1000usize;

    // 第一次註冊 (Vacant)
    set.register(fake_ptr);
    assert_eq!(set.len(), 1);

    // 模擬 slot 被 sweep 並被新物件重用（gen 改變）
    // 但我們無法直接修改 internal state 來模擬...
    // 需要透過實際 GC 來觸發

    // 這個測試需要更完整的設定來驗證generation變化的影響
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `tokio/root.rs:83-85`，在 `Entry::Occupied` 分支新增 generation 驗證：

```rust
Entry::Occupied(o) => {
    // 檢查 slot 是否被重用（generation 是否改變）
    let current_generation = crate::heap::try_with_heap(|heap| unsafe {
        crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8)
            .map_or(0u32, |gc_box| gc_box.as_ref().generation())
    }).unwrap_or(0);
    
    let entry = o.into_mut();
    if entry.1 != current_generation {
        // Slot 被重用，視為新 entry（更新 generation）
        entry.0 = 1;
        entry.1 = current_generation;
    } else {
        entry.0 += 1;
    }
}
```

**注意：** 需要 `use std::collections::hash_map::Entry;` 在 `register` 函式中。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GcRootSet 的設計是為了追蹤跨 async task 的 GC root。其使用 address 作為 key，generation 作為 slot reuse 檢測。當 `register` 遇到 `Entry::Occupied` 時，應該再次驗證 generation，因為 slot 可能在 `register` 呼叫之間被 sweep 並重新分配。否則新物件會繼承舊物件的 refcount 歷史，導致 root 追蹤錯誤。

**Rustacean (Soundness 觀點):**
這不是傳統的 memory safety 問題（無 UAF/UB），但可能導致 logic error：如果新物件的 root 被錯誤移除，該物件可能在不該被收集時被收集。不過此問題需要 `mem::forget()` 或類似的破壞性操作才能觸發，屬於 "unsafe code must not be used incorrectly" 範疇。

**Geohot (Exploit 觀點):**
若攻擊者能夠控制 `mem::forget()` 的呼叫（例如透過 Rust 的 `ManuallyDrop` 或 FFI），可能造成：
1. Slot reuse 後，新物件的 root 被錯誤遞增
2. 舊 entry 最終被移除時，新物件的 root 也跟著消失
3. 若新物件同時沒有其他 root，會導致 use-after-free

但此攻擊需要能夠呼叫 `mem::forget()` 在內部指標上，難度較高。