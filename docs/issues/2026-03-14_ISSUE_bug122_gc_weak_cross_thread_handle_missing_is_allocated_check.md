# [Bug]: Gc::weak_cross_thread_handle missing is_allocated check after inc_weak

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要涉及时序恰好在 GC sweep 发生时的竞争条件 |
| **Severity (嚴重程度)** | High | 可能导致使用已回收的 slot，造成 use-after-free |
| **Reproducibility (復現難度)** | Very High | 需要精确的时序控制来触发 slot 复用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::weak_cross_thread_handle`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::weak_cross_thread_handle` 方法在调用 `inc_weak()` 后缺少 `is_allocated` 检查。

### 預期行為 (Expected Behavior)
应该在 `inc_weak()` 后检查 slot 是否仍然被分配，与 `Gc::cross_thread_handle` 保持一致。

### 實際行為 (Actual Behavior)
调用 `inc_weak()` 后没有验证 slot 是否已被 sweep 复用，可能导致使用已释放的内存。

---

## 🔬 根本原因分析 (Root Cause Analysis)

对比 `Gc::cross_thread_handle` (line 1667-1676) 和 `Gc::weak_cross_thread_handle` (line 1718):

**cross_thread_handle (有 is_allocated 检查):**
```rust
(*ptr.as_ptr()).inc_ref();

if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    // Don't call dec_ref when slot swept - it may be reused (bug133)
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::cross_thread_handle: object slot was swept after inc_ref"
    );
}
```

**weak_cross_thread_handle (缺少 is_allocated 检查):**
```rust
gc_box.inc_weak();
// 缺少 is_allocated 检查!!!
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    
    // Create weak cross-thread handle
    let weak_handle = gc.weak_cross_thread_handle();
    
    // Drop the original Gc
    drop(gc);
    
    // Force collection
    rudo_gc::collect_full();
    
    // Now try to use the weak handle - this might access a reused slot
    // because is_allocated check is missing
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::weak_cross_thread_handle` 的 `inc_weak()` 调用后添加 `is_allocated` 检查：

```rust
gc_box.inc_weak();

if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    // Don't call dec_weak when slot swept - it may be reused (bug133)
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::weak_cross_thread_handle: object slot was swept after inc_weak"
    );
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
这与之前修复的 bug133 (slot 复用问题) 属于同一类问题。GcBoxWeakRef 的其他方法如 `clone`, `upgrade`, `try_upgrade` 都已有此检查，唯独 `weak_cross_thread_handle` 遗漏。

**Rustacean (Soundness 觀點):**
缺少 `is_allocated` 检查会导致在 slot 被 sweep 复用后继续访问旧对象，可能造成 memory corruption 或 use-after-free。

**Geohot (Exploit 觀點):**
这是一个 TOCTOU (Time-of-Check to Time-of-Use) 问题。虽然需要精确时序触发，但利用成功可导致任意内存读写。

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

The `is_allocated` check after `inc_weak()` is present in the current implementation at `ptr.rs:1721-1728`. The code matches the suggested fix:

```rust
gc_box.inc_weak();

if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    // Don't call dec_weak when slot swept - it may be reused (bug133)
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::weak_cross_thread_handle: object slot was swept after inc_weak"
    );
}
```

Behavior now matches `Gc::cross_thread_handle` as described in the issue.
