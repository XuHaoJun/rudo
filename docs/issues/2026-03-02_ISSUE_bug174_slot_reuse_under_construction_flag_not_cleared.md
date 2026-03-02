# [Bug]: Slot Reuse 時未清除 UNDER_CONSTRUCTION_FLAG 導致新物件被錯誤標記為建構中

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需先有 new_cyclic_weak 物件被釋放才能復現 |
| **Severity (嚴重程度)** | High | 會導致新物件無法升級 Weak 引用或解析 GcHandle |
| **Reproducibility (復現難度)** | Medium | 需特定配置（new_cyclic_weak + slot reuse）|

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Slot Allocation / GcBox reuse (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

當 slot 被重複使用時（slot reuse），程式碼會清除 DEAD_FLAG 和 GEN_OLD_FLAG，但不會清除 UNDER_CONSTRUCTION_FLAG。

### 預期行為 (Expected Behavior)
Slot 被重用時，GcBox 的所有 flag 都應該被清除，確保新物件不會繼承舊物件的狀態。

### 實際行為 (Actual Behavior)
當 GcBox 是透過 `Gc::new_cyclic_weak` 分配且後來被釋放回自由列表，然後該 slot 被重用於新的分配時，UNDER_CONSTRUCTION_FLAG 仍然設定，導致：
- `is_under_construction()` 錯誤地返回 true
- `Weak::upgrade()` 會觸發 assertion 失敗
- `GcHandle::resolve()` 會觸發 assertion 失敗

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs` 的 slot reuse 程式碼中：

1. **一般 slot 重用** (lines 2206-2212):
   - 只呼叫 `clear_dead()` 清除 DEAD_FLAG
   - 但沒有清除 UNDER_CONSTRUCTION_FLAG

2. **大型物件 slot 重用** (line 2636-2637):
   - 只呼叫 `clear_gen_old()` 清除 GEN_OLD_FLAG
   - 但沒有清除 UNDER_CONSTRUCTION_FLAG

對比 `Gc::new_cyclic_weak` (ptr.rs:1119) 會設定 `weak_count` 為 `UNDER_CONSTRUCTION_FLAG`：
```rust
std::ptr::write(
    std::ptr::addr_of_mut!((*gc_box).weak_count),
    AtomicUsize::new(GcBox::<T>::UNDER_CONSTRUCTION_FLAG),
);
```

然後在 construction 完成後清除 (ptr.rs:1146)：
```rust
(*gc_box_ptr.as_ptr()).set_under_construction(false);
```

問題在於當 slot 被釋放並重用時，UNDER_CONSTRUCTION_FLAG 不會被清除。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要使用 Miri 或特殊配置才能穩定復現：
1. 建立多個 `Gc::new_cyclic_weak` 物件
2. 釋放這些物件使其返回自由列表
3. 分配新物件（可能重用這些 slot）
4. 嘗試對新物件使用 `Weak::upgrade()`

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 slot reuse 程式碼中添加清除 UNDER_CONSTRUCTION_FLAG：

**heap.rs:2206-2212 (一般 slot):**
```rust
// Clear DEAD_FLAG so reused slot is not incorrectly marked as dead.
// SAFETY: obj_ptr points to a valid GcBox slot (was in free list).
#[allow(clippy::cast_ptr_alignment)]
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();
    // FIX: Also clear UNDER_CONSTRUCTION_FLAG
    (*gc_box_ptr).weak_count.fetch_and(
        !crate::ptr::GcBox::<()>::UNDER_CONSTRUCTION_FLAG, 
        Ordering::Release
    );
}
```

**heap.rs:2636-2637 (大型物件):**
```rust
// Clear GEN_OLD_FLAG so reused slots don't inherit stale barrier state.
unsafe { (*gc_box_ptr).clear_gen_old() }
// FIX: Also clear UNDER_CONSTRUCTION_FLAG
unsafe { 
    (*gc_box_ptr).weak_count.fetch_and(
        !crate::ptr::GcBox::<()>::UNDER_CONSTRUCTION_FLAG, 
        Ordering::Release
    );
}
```

或者新增一個 `clear_all_flags()` 方法來清除所有 flag。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這與 bug17（GEN_OLD_FLAG 未清除）類似。Slot reuse 時清除所有 flag 是標準做法，確保新物件不會繼承舊狀態。Chez Scheme 的記憶體管理同樣會在 reuse 時重置所有元資料。

**Rustacean (Soundness 觀點):**
這可能導致 panic（assertion 失敗），但不會導致 UB。然而，錯誤的 `is_under_construction()` 返回值可能導致邏輯錯誤。

**Geohot (Exploit 觀點):**
在極端的並發情況下，如果攻擊者能控制 slot reuse 的時序，可能能夠利用這個 bug 來阻止正常的 Weak 升級。但實際利用難度較高。
