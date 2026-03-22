# [Bug]: Weak::may_be_valid() 缺少 is_gc_box_pointer_valid 檢查

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 lazy sweep 回收 slot 後，Weak 可能返回 true |
| **Severity (嚴重程度)** | Low | 僅返回不正確的 boolean，不會導致 UAF |
| **Reproducibility (復現難度)** | Medium | 需要並發場景才能穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::may_be_valid()` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`Weak::may_be_valid()` 方法在檢查指標有效性時，沒有調用 `is_gc_box_pointer_valid()` 來驗證 slot 是否仍然被分配。當 slot 被 lazy sweep 回收並重新分配給新物件時，`may_be_valid()` 會錯誤地返回 `true`。

### 預期行為
- `may_be_valid()` 應該只在指標可能有效的情況下返回 `true`
- 當 slot 已被 sweep 回收並重新分配時，應返回 `false`

### 實際行為
- `may_be_valid()` 只檢查：
  1. 指針不為 null
  2. 地址 >= 4096 (MIN_VALID_HEAP_ADDRESS)
  3. 地址對齊正確
- 但**不檢查** `is_gc_box_pointer_valid()`，這會驗證 slot 是否仍被分配

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1938-1953` 的 `Weak::may_be_valid()` 實現中：

```rust
pub fn may_be_valid(&self) -> bool {
    let ptr = self.ptr.load(Ordering::Acquire);

    if ptr.is_null() {
        return false;
    }

    let Some(ptr) = ptr.as_option() else {
        return false;
    };

    let addr = ptr.as_ptr() as usize;

    let alignment = std::mem::align_of::<GcBox<T>>();
    addr >= 4096 && addr % alignment == 0  // <-- 缺少 is_gc_box_pointer_valid 檢查!
}
```

對比 `Weak::clone()` (ptr.rs:2091) 的正確實現：

```rust
// Validate pointer is still in heap before dereferencing (avoids TOCTOU with sweep).
if !is_gc_box_pointer_valid(ptr_addr) {
    return Self { ... };
}
```

`Weak::may_be_valid()` 應該與 `Weak::clone()` 具有一致的有效性檢查行為。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立一個 Gc 物件並取得 Weak 引用
2. 觸發 GC 讓物件被標記為可回收
3. 呼叫 lazy sweep 回收 slot
4. 在同一個 slot 位置建立新的 Gc 物件
5. 呼叫 `weak.may_be_valid()` - 預期應返回 `false`，但會返回 `true`

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `Weak::may_be_valid()` 中添加 `is_gc_box_pointer_valid()` 檢查：

```rust
pub fn may_be_valid(&self) -> bool {
    let ptr = self.ptr.load(Ordering::Acquire);

    if ptr.is_null() {
        return false;
    }

    let Some(ptr) = ptr.as_option() else {
        return false;
    };

    let addr = ptr.as_ptr() as usize;

    let alignment = std::mem::align_of::<GcBox<T>>();
    if addr < MIN_VALID_HEAP_ADDRESS || addr % alignment != 0 {
        return false;
    }

    // 新增：檢查 slot 是否仍然被分配
    if !is_gc_box_pointer_valid(addr) {
        return false;
    }

    true
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- `may_be_valid()` 是用於快速預過濾的輕量級檢查
- 當前實現只檢查地址範圍和对齐，沒有驗證 slot 分配狀態
- 這與 `Weak::clone()` 的實現不一致，後者正確地調用了 `is_gc_box_pointer_valid()`

**Rustacean (Soundness 觀點):**
- 這不會導致 UAF，因爲 `may_be_valid()` 只是返回 boolean
- 但會導致邏輯錯誤：程式可能會嘗試升級一個已經無效的 Weak 引用
- 雖然後續的 `try_upgrade()` 會做完整檢查，但這浪費了預過濾的設計目的

**Geohot (Exploit 觀點):**
- 這不是安全漏洞，只是不正確的預過濾
- 攻擊者無法利用此行爲，因爲後續操作會有完整檢查
- 但可能導致 DoS（通過讓 `may_be_valid()` 錯誤返回 true 來浪費 CPU 週期）
