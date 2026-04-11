# [Bug]: GcCell<Vec<Gc<T>>> slot swept and reused during deep tree construction

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 100% reproducible with `./test.sh` |
| **Severity (嚴重程度)** | `Critical` | Slot swept and reused causes panic - memory safety issue |
| **Reproducibility (Reproducibility)** | `Very Low` | Always fails with `./test.sh` |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::deref` (ptr.rs:2107), `GcCell::borrow_mut`, minor GC sweep
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When building a deep tree structure with parent/child relationships using `GcCell<Vec<Gc<T>>>`, all nodes should remain accessible and their slots should not be swept while the tree is being constructed.

### 實際行為 (Actual Behavior)

Two tests in `deep_tree_allocation_test.rs` are failing:
- `test_deep_tree_allocation`
- `test_collect_between_deep_trees`

Both fail with:
```
Gc::deref: slot has been swept and reused
panicked at crates/rudo-gc/src/ptr.rs:2107:17
```

The error occurs during `build_deep_tree()` when calling `root.add_child(Gc::clone(&child2))`.

### Test Structure
```rust
#[derive(Trace)]
pub struct TestComponent {
    pub id: u64,
    pub children: GcCell<Vec<Gc<Self>>>,  // Vec of child references
    pub parent: GcCell<Option<Gc<Self>>>,
    pub is_updating: AtomicBool,
}
```

When building tree2, the root's slot (0x600000000d80) is reported as swept and reused when trying to add child2.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Error Location:** `ptr.rs:2107-2110`

```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    // ...
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),
                "Gc::deref: slot has been swept and reused"  // <-- Line 2109
            );
        }
    }
    // ...
}
```

**問題發生時機：**
1. Tree2's root is at 0x600000000d80
2. Tree2's child1 is at 0x600000000e00
3. Tree2's child2 is at 0x600000000f00
4. When calling `root.add_child(Gc::clone(&child2))`, the root's slot (0x600000000d80) is reported as not allocated

**可能的根本原因：**
1. The `register_test_root` only registers the root pointer, not the entire tree
2. During minor GC, the tree's children (stored in `GcCell<Vec<Gc<T>>>`) might not be properly traced
3. When a new tree is built after `collect()`, old tree's slots might be incorrectly reclaimed

**懷疑的方向：**
- `GcCell<Vec<Gc<T>>>` 的 `GcCapture` 實作可能沒有正確捕获 Vec 內部的 Gc 指標
- 或者 minor GC 的追蹤邏輯有問題

---

## 🔍 進一步分析 (Additional Analysis)

### 測試失敗確認
測試在 `deep_tree_allocation_test.rs` 中失敗:
- `test_deep_tree_allocation`: 在第二次 `build_deep_tree()` 呼叫時崩潰
- `test_collect_between_deep_trees`: 在 `collect()` 後的 `build_deep_tree()` 崩潰

### 錯誤發生時機
1. Tree2's root 分配於 0x600000000d80
2. Tree2's child1 分配於 0x600000000e00
3. Tree2's child2 分配於 0x600000000f00
4. 當呼叫 `root.add_child(Gc::clone(&child2))` 時，root 的 slot (0x600000000d80) 被報告為已釋放

### 初步分析
1. `GcCell<Vec<Gc<T>>>::capture_gc_ptrs_into` 正確迭代 Vec 並捕獲每個 Gc<T> 的指標
2. `Vec<Gc<T>>::capture_gc_ptrs_into` 正確調用每個元素的 `capture_gc_ptrs_into`
3. `Gc<T>::capture_gc_ptrs_into` 將 GcBox 指針推入向量，但**不檢查 is_allocated**
4. `mark_object_black` 會檢查 is_allocated，但捕獲發生在 barrier 記錄 OLD 值期間

### 可能的根本原因
- test_root 可能在 minor GC 期間未被正確追蹤
- 或者 `mark_object_minor` 沒有正確標記 test_root
- 或者分配的新 slot 覆蓋了舊的 root slot

### 需要進一步調查
1. `mark_minor_roots` 中 test_roots 的處理邏輯
2. `find_gc_box_from_ptr` 對 test_root 指針的處理
3. minor GC 期間的對象追蹤順序

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```bash
cd /home/noah/Desktop/workspace/rudo-gc/rudo
./test.sh 2>&1 | grep -A10 "test_deep_tree_allocation"
```

**預期：** 所有測試通過
**實際：** `test_deep_tree_allocation` 和 `test_collect_between_deep_trees` 失敗

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

需要進一步調查：
1. 檢查 `GcCell<Vec<Gc<T>>>` 的 `GcCapture` 實作是否正確
2. 檢查 minor GC 是否正確追蹤到所有 child 物件
3. 檢查 `register_test_root` 是否只註冊了 root 而沒有註冊 children

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GcCell<Vec<Gc<T>>> 的追蹤是關鍵。如果 `borrow_mut()` 的 `GcCapture` 沒有正確捕獲 Vec 內部的 Gc 指標，minor GC 可能會錯誤地回收仍然可達的物件。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。當我們嘗試解引用一個 Gc 指針時，其 slot 已經被回收並重新分配。這是經典型型的 UAF。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 GC 的時序，可能會利用這個漏洞來實現記憶體佈局操縱。