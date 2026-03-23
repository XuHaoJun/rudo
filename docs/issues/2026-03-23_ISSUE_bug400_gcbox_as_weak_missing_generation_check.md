# [Bug]: GcBox::as_weak Missing Generation Check Before inc_weak (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep to reclaim and reuse slot between flag check and inc_weak |
| **Severity (嚴重程度)** | Critical | Weak reference count corruption - inc_weak called on wrong object |
| **Reproducibility (復現難度)** | High | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs` - `GcBox::as_weak()` (line 566)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcBox::as_weak()` 應在調用 `inc_weak()` 之前驗證物件槽位的 generation，確保在 flag 檢查和 `inc_weak` 調用之間 slot 沒有被回收並重新分配。

### 實際行為 (Actual Behavior)
`GcBox::as_weak()` 存在 TOCTOU 漏洞：

1. Lines 568-572: 檢查 flag states (is_under_construction, has_dead_flag, dropping_state)
2. Line 576: 調用 `inc_weak()` - **沒有 generation 預檢查！**
3. Lines 578-587: 檢查 `is_allocated`

在步驟 1 和步驟 2 之間，slot 可能被 sweep 回收並分配新物件（帶有不同的 generation）。然後 `inc_weak()` 會調用到新物件上，導致 weak reference count 腐壞。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:566-591` (`GcBox::as_weak`):

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() || self.has_dead_flag() || self.dropping_state() != 0 {
            return GcBoxWeakRef { ptr: AtomicNullable::null() };
        }

        // BUG: No generation check before inc_weak!
        (*NonNull::from(self).as_ptr()).inc_weak();  // LINE 576

        let self_ptr = NonNull::from(self).as_ptr() as *const u8;
        if let Some(idx) = crate::heap::ptr_to_object_index(self_ptr) {
            let header = crate::heap::ptr_to_page_header(self_ptr);
            if !(*header.as_ptr()).is_allocated(idx) {
                // Don't call dec_weak - slot may be reused (bug133)
                return GcBoxWeakRef { ptr: AtomicNullable::null() };
            }
        }

        GcBoxWeakRef::new(NonNull::from(self))
    }
}
```

對比已修復的 `Gc::as_weak` (lines 1839-1846):
```rust
let pre_generation = gc_box.generation();
(*ptr.as_ptr()).inc_weak();
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();
    return GcBoxWeakRef { ptr: AtomicNullable::null() };
}
```

`GcBox::as_weak()` 缺少這個 generation 檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This requires concurrent testing environment
// 1. Create a Gc object
// 2. Trigger lazy sweep to reclaim the object
// 3. Allocate new object in same slot (different generation)
// 4. Concurrently call Gc::as_weak()
// 5. Observe weak_count corruption on wrong object
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 之前添加 generation 檢查：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() || self.has_dead_flag() || self.dropping_state() != 0 {
            return GcBoxWeakRef { ptr: AtomicNullable::null() };
        }

        // Get generation BEFORE inc_weak to detect slot reuse.
        let pre_generation = (*NonNull::from(self).as_ptr()).generation();
        (*NonNull::from(self).as_ptr()).inc_weak();

        // Verify generation hasn't changed - if slot was reused, undo inc_weak.
        if pre_generation != (*NonNull::from(self).as_ptr()).generation() {
            (*NonNull::from(self).as_ptr()).dec_weak();
            return GcBoxWeakRef { ptr: AtomicNullable::null() };
        }

        let self_ptr = NonNull::from(self).as_ptr() as *const u8;
        if let Some(idx) = crate::heap::ptr_to_object_index(self_ptr) {
            let header = crate::heap::ptr_to_page_header(self_ptr);
            if !(*header.as_ptr()).is_allocated(idx) {
                return GcBoxWeakRef { ptr: AtomicNullable::null() };
            }
        }

        GcBoxWeakRef::new(NonNull::from(self))
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`Gc::as_weak` 已被修復（bug367），但 `GcBox::as_weak` 被遺漏。這是同一個 TOCTOU 模式 - 在 flag 檢查和 ref 遞增之間，slot 可能被 sweep 回收並重用。

**Rustacean (Soundness 觀點):**
對錯誤物件調用 `inc_weak` 是嚴重的記憶體安全問題。可能導致新物件的 weak_count 人為提高，阻止正確回收。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制配置時機，可以操縱 weak_count 腐壞狀態。

---

## 🔗 相關 Issue

- bug367: Gc::as_weak missing generation check before inc_weak (Fixed)
- bug366: Gc::weak_cross_thread_handle missing generation check before inc_weak (Fixed)
- bug356: Gc::downgrade missing generation check before inc_weak (Fixed)
- bug354: GcBoxWeakRef::clone inc_weak before is_allocated (Fixed)
