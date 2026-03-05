# [Bug]: Handle::to_gc and AsyncHandle::to_gc missing post-increment safety check (TOCTOU)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent access during object drop |
| **Severity (嚴重程度)** | Critical | Use-after-free, memory corruption |
| **Reproducibility (復現難度)** | High | Requires precise timing of concurrent operations |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::to_gc`, `AsyncHandle::to_gc`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Handle::to_gc()` and `AsyncHandle::to_gc()` should perform a post-increment safety check after calling `try_inc_ref_if_nonzero()` to prevent returning a `Gc` to an object that is currently being dropped. This pattern is already implemented in `Gc::try_clone()` at `ptr.rs:1257-1264`.

### 實際行為 (Actual Behavior)
The functions perform a pre-check before `try_inc_ref_if_nonzero()` but do NOT perform a post-check after. This creates a TOCTOU race condition:
1. Thread A checks state (not dropping, not dead) - passes assert
2. Thread B starts dropping the object (sets dropping_state)
3. Thread A calls `try_inc_ref_if_nonzero()` - may succeed if ref_count > 0
4. Thread A returns a `Gc` to an object that is being dropped → **Use-After-Free**

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `handles/mod.rs:347-362` (Handle::to_gc):
```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("Handle::to_gc: object is being dropped by another thread");
}
Gc::from_raw(gc_box_ptr as *const u8)  // No post-check!
```

In `handles/async.rs:719-722` (AsyncHandle::to_gc):
```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::to_gc: object is being dropped by another thread");
}
Gc::from_raw(gc_box_ptr as *const u8)  // No post-check!
```

Compare with the correct pattern in `ptr.rs:1254-1264` (Gc::try_clone):
```rust
if !(*gc_box_ptr).try_inc_ref_if_nonzero() {
    return None;
}
// Post-increment safety check: dropping/dead may flip between pre-check and ref bump.
if (*gc_box_ptr).has_dead_flag()
    || (*gc_box_ptr).dropping_state() != 0
    || (*gc_box_ptr).is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr);
    return None;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Create a `HandleScope` with a `Gc<T>`
2. Spawn a thread that:
   - Gets a `Handle` from the scope
   - Repeatedly calls `to_gc()` in a loop
3. Concurrently, drop the original `Gc<T>` to trigger collection
4. With precise timing, the `to_gc()` call may succeed even though the object is being dropped

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add a post-check after `try_inc_ref_if_nonzero()` in both `Handle::to_gc()` and `AsyncHandle::to_gc()`:

```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("Handle::to_gc: object is being dropped by another thread");
}
// Post-increment safety check
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr);
    panic!("Handle::to_gc: object became dead/dropping after ref increment");
}
Gc::from_raw(gc_box_ptr as *const u8)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The post-check pattern is essential for correct reference counting in concurrent GC. Without it, there's a window where `try_inc_ref_if_nonzero()` can succeed on an object that another thread is simultaneously dropping, leading to a reference to freed memory.

**Rustacean (Soundness 觀點):**
This is undefined behavior - accessing an object after its destructor has started is UB. The post-check is required to maintain memory safety.

**Geohot (Exploit 觀點):**
This TOCTOU can be exploited for use-after-free if an attacker can control the timing of the concurrent operations. The window is small but real.
