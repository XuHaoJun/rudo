# [Bug]: GcBox::as_weak inc_weak missing is_allocated check

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Slot reuse after object deallocation can trigger this |
| **Severity (嚴重程度)** | Critical | Potential use-after-free with weak reference count |
| **Reproducibility (復現難度)** | Medium | Requires slot reuse timing scenario |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcBox::as_weak` should check `is_allocated()` before incrementing weak reference count, similar to other `inc_weak()` call sites in the codebase.

### 實際行為 (Actual Behavior)
`GcBox::as_weak` calls `inc_weak()` without checking if the slot has been reused (object deallocated). If the slot was reused, incrementing weak count on a new object leads to corruption.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `ptr.rs:452`, `GcBox::as_weak`:
```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() || self.has_dead_flag() || self.dropping_state() != 0 {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*NonNull::from(self).as_ptr()).inc_weak();  // Missing is_allocated check!
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

Compare with the correct pattern in `GcBoxWeakRef::new` at lines 600-611:
```rust
(*ptr.as_ptr()).inc_weak();

if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();
        return Self { ptr: AtomicNullable::null() };
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Create a GcBox and obtain weak reference via `as_weak()`
2. Trigger GC to deallocate the object
3. Allocate a new object in the same slot (slot reuse)
4. Call `as_weak()` again - should check is_allocated but doesn't

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check before `inc_weak()` in `GcBox::as_weak`, similar to the pattern in `GcBoxWeakRef::new`.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This pattern is consistent with other bugs found recently (bug217, bug218, bug219). The slot reuse vulnerability is a common theme in garbage collector implementations.

**Rustacean (Soundness 觀點):**
Incrementing weak reference count on a deallocated/reallocated slot leads to reference count corruption and potential use-after-free.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of slot reuse, they could corrupt weak reference counts leading to use-after-free scenarios.
