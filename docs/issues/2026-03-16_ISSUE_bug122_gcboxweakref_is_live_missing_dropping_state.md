# [Bug]: GcBoxWeakRef::is_live() 缺少 dropping_state 檢查導致不一致行為

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在物件正在被 drop 時調用 is_live() |
| **Severity (嚴重程度)** | Medium | API 不一致導致邏輯錯誤，非記憶體安全問題 |
| **Reproducibility (復現難度)** | Low | 可透過比對程式碼發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::is_live()` (`ptr.rs:790-813`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBoxWeakRef::is_live()` 應該與 `GcBoxWeakRef::upgrade()` 具有一致的行為。當物件正在被 drop (`dropping_state() != 0`) 時，兩者都應該返回表示物件不可存活的值：
- `is_live()` 應返回 `false`
- `upgrade()` 應返回 `None`

### 實際行為 (Actual Behavior)

目前 `GcBoxWeakRef::is_live()` 只檢查 `has_dead_flag()`，但沒有檢查 `dropping_state()`：

```rust
// ptr.rs:790-813
pub(crate) fn is_live(&self) -> bool {
    let Some(ptr) = self.as_ptr() else {
        return false;
    };
    // ... 省略有效性檢查 ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        // 問題：只檢查 has_dead_flag()，沒有檢查 dropping_state()!
        if gc_box.has_dead_flag() {
            return false;
        }
    }
    true
}
```

相比之下，`GcBoxWeakRef::upgrade()` 正確地檢查了兩者：

```rust
// ptr.rs:641-645
if gc_box.dropping_state() != 0 {  // ✓ 檢查
    return None;
}
if gc_box.has_dead_flag() {  // ✓ 檢查
    return None;
}
```

這導致當物件正在被 drop 時，`is_live()` 返回 `true`，但 `upgrade()` 返回 `None`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:790-813`，`is_live()` 函數只檢查 `has_dead_flag()`：
```rust
if gc_box.has_dead_flag() {
    return false;
}
```

但漏掉了 `dropping_state() != 0` 的檢查。

正確的實現應該同時檢查兩者：
1. `has_dead_flag()` - 物件是否被標記為死亡
2. `dropping_state() != 0` - 物件是否正在被 drop 過程中

這與 bug58 (`Weak::is_alive()` 缺少 dropping_state 檢查) 是相同的模式問題，但發生在內部 API `GcBoxWeakRef` 而非公開 API `Weak`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 影響內部 API `GcBoxWeakRef::is_live()`，可用於 cross-thread handles 等場景。雖然無法直接從用戶代碼觸發，但此不一致性可能導致內部邏輯錯誤。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ptr.rs:790-813` 處修改 `is_live()` 方法：

```rust
pub(crate) fn is_live(&self) -> bool {
    let Some(ptr) = self.as_ptr() else {
        return false;
    };

    let addr = ptr.as_ptr() as usize;
    let alignment = std::mem::align_of::<GcBox<T>>();
    if addr < MIN_VALID_HEAP_ADDRESS || addr % alignment != 0 {
        return false;
    }

    if !is_gc_box_pointer_valid(addr) {
        return false;
    }

    unsafe {
        let gc_box = &*ptr.as_ptr();
        // 檢查 has_dead_flag()
        if gc_box.has_dead_flag() {
            return false;
        }
        // 檢查 dropping_state()
        if gc_box.dropping_state() != 0 {
            return false;
        }
    }
    true
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在引用計數 GC 中，`dropping_state` 是用來防止在物件 drop 過程中建立新強引用的關鍵機制。當物件正在被 drop 時（`dropping_state != 0`），即使 `ref_count > 0`，也不應該允許建立新的強引用。`is_live()` 和 `upgrade()` 應該具有一致的語義，否則會造成 API 使用上的困惑。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，因為 `is_live()` 本身是一個「非確定性」的檢查。但這是 API 一致性問題 - 當 `upgrade()` 返回 `None` 時，`is_live()` 應該也返回 `false`，否則會造成邏輯錯誤。類似的問題在 bug58 中已經修復過，這次是內部 API 的遺漏。

**Geohot (Exploit 攻擊觀點):**
雖然這不是安全性問題，但不一致的 API 可能被利用來构造複雜的 bug。特別是在並發場景下，這種不一致可能導致難以預測的行為。

---

## 關聯 Issue

- **Bug58**: `Weak::is_alive()` 缺少 dropping_state 檢查 (已修復，公開 API)
- **Bug42**: `Weak::try_upgrade()` 缺少 dropping_state 檢查 (已修復)
- **Bug52**: `Weak::strong_count()` 缺少 dropping_state 檢查 (已修復)

---

## Resolution (2026-03-21)

**Outcome:** Already fixed in current tree; issue file was stale.

`GcBoxWeakRef::is_live()` in `crates/rudo-gc/src/ptr.rs` now matches `try_upgrade()` / `upgrade()` semantics: after `is_under_construction()` and `has_dead_flag()` checks it returns `false` when `dropping_state() != 0`, and also requires `is_allocated` when the slot index is known (lines ~851–865).

No source change was required.
