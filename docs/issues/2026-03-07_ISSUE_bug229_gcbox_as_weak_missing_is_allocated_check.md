# [Bug]: GcBox::as_weak Missing is_allocated Check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 object 被 sweep 後但 GcBox 指標仍存在時呼叫 as_weak |
| **Severity (嚴重程度)** | High | 可能產生對已釋放記憶體的 weak 引用，導致 UAF |
| **Reproducibility (復現難度)** | Medium | 需要精確控制 GC timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBox::as_weak` 函數在建立 weak reference 時，沒有檢查 object 是否仍然 allocated。這與 `GcBoxWeakRef::clone` 的行為不同，後者有完整的檢查流程。

### 預期行為 (Expected Behavior)
`GcBox::as_weak` 應該在建立 weak reference 前檢查 object 是否仍然 allocated，確保不會返回對已釋放記憶體的 weak 引用。

### 實際行為 (Actual Behavior)
`GcBox::as_weak` 只檢查 `is_under_construction()`, `has_dead_flag()`, 和 `dropping_state()`，但缺少 `is_allocated` 檢查。如果 object 被 sweep 後指標仍然存在，可能會返回一個無效的 weak reference。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:443-455`，`GcBox::as_weak` 的實作：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() || self.has_dead_flag() || self.dropping_state() != 0 {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*NonNull::from(self).as_ptr()).inc_weak();
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

相比之下，`GcBoxWeakRef::clone` (lines 577-616) 有完整的檢查：

```rust
// 1. 檢查 alignment 和 min address
// 2. 檢查 is_gc_box_pointer_valid
// 3. 檢查 has_dead_flag
// 4. 檢查 dropping_state
// 5. 呼叫 inc_weak()
// 6. 檢查 is_allocated，如果沒有 allocated 就 dec_weak() 並返回 null
```

缺少 `is_allocated` 檢查可能導致在 object 被 sweep 後，仍然返回一個 weak reference，該 reference 指向已釋放的記憶體。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立一個 Gc object
2. 觸發 GC 並使用 lazy sweep 回收該 object
3. 在 object 被 sweep 後，呼叫 `GcBox::as_weak`
4. 預期：返回 null weak reference
5. 實際：返回有效的 weak reference（指向已釋放記憶體）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBox::as_weak` 中新增 `is_allocated` 檢查，類似 `GcBoxWeakRef::clone` 的實作：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() || self.has_dead_flag() || self.dropping_state() != 0 {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        
        // 新增：檢查 object 是否仍然 allocated
        if let Some(idx) = crate::heap::ptr_to_object_index(self as *const GcBox<T> as *const u8) {
            let header = crate::heap::ptr_to_page_header(self as *const GcBox<T> as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                return GcBoxWeakRef {
                    ptr: AtomicNullable::null(),
                };
            }
        }
        
        (*NonNull::from(self).as_ptr()).inc_weak();
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 lazy sweep 實現中，object 被標記為 "needs sweep" 但尚未實際釋放記憶體。此時指標仍然有效，但記憶體可能被重複使用。GcBox::as_weak 應該像 GcBoxWeakRef::clone 一樣，在建立 weak reference 前驗證 object 的 allocation 狀態。

**Rustacean (Soundness 觀點):**
缺少 `is_allocated` 檢查可能導致返回指向已釋放記憶體的 weak reference，這是一個記憶體安全問題。雖然實際使用這個 weak reference 時會有其他檢查（如 has_dead_flag），但最好在源頭就防止這種情況。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 GC timing，可能利用這個漏洞在 object 被釋放後但指標仍然有效時，建立一個 weak reference，進一步利用記憶體佈局進行攻擊。
