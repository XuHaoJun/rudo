# [Bug]: Handle::get / AsyncHandle::get 缺少 is_gc_box_pointer_valid 檢查

**Status:** Open
**Tags:** Defense-in-depth, Soundness

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 handle 指向的 slot 被 sweep 且記憶體被重用，但 handle 仍然有效 |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free，訪問到錯誤物件的資料 |
| **Reproducibility (復現難度)** | High | 需要精確控制 GC 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::get()`, `AsyncHandle::get()` in `handles/mod.rs` and `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Handle::get()` 和 `AsyncHandle::get()` 在解引用 GcBox 之前應該先驗證指標是否有效，類似於 `Weak` reference 的實現模式。

### 實際行為 (Actual Behavior)

`Handle::get()` (`handles/mod.rs:301-314`) 和 `AsyncHandle::get()` (`handles/async.rs:570-605`) 直接解引用 GcBox 而不檢查指標有效性：

```rust
// handles/mod.rs:301-314
pub fn get(&self) -> &T {
    unsafe {
        let slot = &*self.slot;
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        let gc_box = &*gc_box_ptr;  // <-- 沒有 is_gc_box_pointer_valid 檢查！
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            ...
        );
        gc_box.value()
    }
}
```

對比：`Weak::clone()` (`ptr.rs:615-619`) 正確地調用 `is_gc_box_pointer_valid()`：

```rust
// ptr.rs:615-619 - 正確的實現
if !is_gc_box_pointer_valid(ptr_addr) {
    return Self { ... };
}
```

### 程式碼位置

1. `handles/mod.rs` 第 301-314 行：`Handle::get()`
2. `handles/async.rs` 第 570-605 行：`AsyncHandle::get()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `Handle::get()` 和 `AsyncHandle::get()` 的實現與 `Weak` reference 的實現不一致。`Weak` reference 正確地調用 `is_gc_box_pointer_valid()` 來檢查指標有效性，但 `Handle` 沒有這個檢查。

雖然在正常情況下這不應該造成問題（因為 handle 是 GC root，會在 GC 時被追蹤），但從 defense-in-depth 的角度來看，缺少這個檢查是一個潛在的安全隱患。

理論上的攻擊場景：
1. Handle 指向 GcBox X
2. Handle scope 被 drop（但 handle 本身仍被引用，例如存儲在某個資料結構中）
3. GC 運行 - handle 不再是 root
4. GcBox X 未被標記
5. Lazy sweep 運行並收回 slot
6. 同一個 slot 分配了新物件
7. 使用舊的 handle 訪問 GcBox
8.指標現在指向不同的 GcBox！

注意：Handle 的生命週期與 scope 綁定，理論上不應該在 scope drop 後使用。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在解引用 GcBox 之前添加 `is_gc_box_pointer_valid` 檢查，類似於 `Weak` reference 的實現：

```rust
pub fn get(&self) -> &T {
    unsafe {
        let slot = &*self.slot;
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        
        // 添加有效性檢查
        let ptr_addr = gc_box_ptr as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {
            panic!("Handle::get: invalid GcBox pointer");
        }
        
        let gc_box = &*gc_box_ptr;
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "Handle::get: cannot access a dead, dropping, or under construction Gc"
        );
        gc_box.value()
    }
}
```

需要導入 `is_gc_box_pointer_valid` 函數（從 ptr.rs）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
雖然 handle 在正常情況下是 GC root，其 GcBox 在 GC 期間會被標記，但從防御性編程的角度來看，添加額外的有效性檢查可以提高代碼的健壯性。

**Rustacean (Soundness 觀點):**
這是一個潜在的 soundness 問題。缺少有效性檢查可能導致在極端情況下訪問到錯誤的記憶體。

**Geohot (Exploit 攻擊觀點):**
理論上，如果攻擊者能夠控制 GC 時序，可能可以利用這個缺陷訪問到不應訪問的記憶體。

---

## 備註

此問題與 bug130（WeakCrossThreadHandle::drop 缺少有效性檢查）類似，但影響的是不同的 code path：
- bug130: 發生在 WeakCrossThreadHandle::drop 過程中
- 本 bug: 發生在 Handle::get / AsyncHandle::get 過程中
