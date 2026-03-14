# [Bug]: Gc::as_weak inc_weak missing is_allocated check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Slot reuse after lazy sweep can trigger this |
| **Severity (嚴重程度)** | Critical | Potential use-after-free with weak reference count corruption |
| **Reproducibility (重現難度)** | Medium | Requires lazy sweep slot reuse timing scenario |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::as_weak()` in `ptr.rs:1465-1487`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::as_weak()` should check `is_allocated()` after incrementing weak reference count, similar to other `inc_weak()` call sites in the codebase (e.g., `GcBoxWeakRef::new`, `Gc::downgrade`).

### 實際行為 (Actual Behavior)
`Gc::as_weak()` calls `inc_weak()` without checking if the slot has been swept and reused (object deallocated). If the slot was reused by lazy sweep, incrementing weak count on a new object leads to reference count corruption.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `ptr.rs:1482`, `Gc::as_weak`:
```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let Some(ptr) = ptr.as_option() else {
        return GcBoxWeakRef {
            ptr: AtomicNullable::null(),
        };
    };
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.is_under_construction()
            || gc_box.has_dead_flag()
            || gc_box.dropping_state() != 0
        {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*ptr.as_ptr()).inc_weak();  // Missing is_allocated check!
    }
    GcBoxWeakRef {
        ptr: AtomicNullable::new(ptr),
    }
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

Note: This is different from bug224 which reports the same issue for `GcBox::as_weak()` at line 452. This bug is for `Gc::as_weak()` at line 1482.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Create a `Gc<T>` object
2. Trigger GC to mark the object as dead
3. Lazy sweep reclaims the slot and allocates a new object in the same slot (slot reuse)
4. Call `gc.as_weak()` - should check is_allocated but doesn't
5. Weak count is incorrectly incremented on the new object

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` check after `inc_weak()` in `Gc::as_weak`, similar to the pattern in `GcBoxWeakRef::new`:

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let Some(ptr) = ptr.as_option() else {
        return GcBoxWeakRef {
            ptr: AtomicNullable::null(),
        };
    };
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.is_under_construction()
            || gc_box.has_dead_flag()
            || gc_box.dropping_state() != 0
        {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*ptr.as_ptr()).inc_weak();

        // Add is_allocated check after inc_weak
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                (*ptr.as_ptr()).dec_weak();
                return GcBoxWeakRef {
                    ptr: AtomicNullable::null(),
                };
            }
        }
    }
    GcBoxWeakRef {
        ptr: AtomicNullable::new(ptr),
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is the same slot reuse vulnerability pattern as bug224, but specifically for the `Gc::as_weak()` method. Lazy sweep creates the opportunity for slot reuse, and without proper validation, weak reference counts can be corrupted.

**Rustacean (Soundness 觀點):**
Incrementing weak reference count on a swept/reallocated slot leads to reference count corruption. The new object in that slot will have an incorrect weak_count, potentially causing use-after-free when the weak reference is later upgraded.

**Geohot (Exploit 攻擊觀點):**
An attacker who can control the timing of lazy sweep slot reuse could corrupt weak reference counts. This could lead to:
1. Premature memory reclamation (weak count incorrectly high -> object kept alive incorrectly)
2. Use-after-free scenarios (weak count incorrectly low -> object freed while weak refs exist)

---

## 🔗 相關 Issue

- bug224: GcBox::as_weak inc_weak missing is_allocated check - Same issue but for GcBox::as_weak()
- bug226: Gc::try_deref missing is_allocated check - Similar pattern for try_deref

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

The fix is present in `ptr.rs` (lines 1617–1626). `Gc::as_weak` now checks `is_allocated(idx)` via `ptr_to_object_index` and `ptr_to_page_header` after calling `inc_weak()`. If the slot is not allocated, it returns `GcBoxWeakRef { ptr: AtomicNullable::null() }` without returning the weak ref. Per bug133, `dec_weak` is not called when the slot may be reused (to avoid operating on freed memory). No code changes required.
