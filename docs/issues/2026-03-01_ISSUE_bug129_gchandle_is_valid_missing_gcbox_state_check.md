# [Bug]: GcHandle::is_valid() 未檢查 GcBox 存活狀態 - 導致 False Positive

**Status:** Invalid
**Tags:** Not Reproduced

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發場景且 handle 仍在 root list 但 GcBox 已死亡 |
| **Severity (嚴重程度)** | Low | 不會導致記憶體錯誤，但 API 使用不一致，resolve() 可能意外失敗 |
| **Reproducibility (再現難度)** | Medium | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::is_valid()`, `handles/cross_thread.rs:99-113`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`is_valid()` 應該與 `resolve()` 有一致的行為，確保：
1. handle_id 有效
2. handle 在 root 列表中
3. GcBox 不是 dead / under construction / dropping

### 實際行為 (Actual Behavior)

`is_valid()` 只檢查：
1. handle_id != HandleId::INVALID
2. handle 在 root 列表中

但**沒有檢查**：
- `!gc_box.is_under_construction()`
- `!gc_box.has_dead_flag()`
- `gc_box.dropping_state() == 0`

這導致 `is_valid()` 可能返回 true，但隨後調用 `resolve()` 會 panic！

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `handles/cross_thread.rs:99-113`

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    self.origin_tcb.upgrade().map_or_else(
        || {
            let orphan = heap::lock_orphan_roots();
            orphan.contains_key(&(self.origin_thread, self.handle_id))
        },
        |tcb| {
            let roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.contains_key(&self.handle_id)
        },
    )
}
```

`resolve()` 有完整檢查（lines 165-224）：

```rust
// resolve() checks:
assert!(!gc_box.is_under_construction(), ...);
assert!(!gc_box.has_dead_flag(), ...);
assert!(gc_box.dropping_state() == 0, ...);
```

但 `is_valid()` 缺少這些檢查，導致 API 不一致。

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::time::Duration;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // Drop the Gc - object becomes eligible for collection
    drop(gc);
    
    // Trigger GC to collect the object
    collect_full();
    
    // Now is_valid() should ideally return false (but it may still return true!)
    // Because the handle is still registered in the root list temporarily
    let is_valid = handle.is_valid();
    println!("is_valid returned: {}", is_valid);
    
    // But resolve() will panic!
    // let resolved = handle.resolve(); // panics
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `is_valid()` 中添加 GcBox 狀態檢查：

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    
    let in_roots = self.origin_tcb.upgrade().map_or_else(
        || {
            let orphan = heap::lock_orphan_roots();
            orphan.contains_key(&(self.origin_thread, self.handle_id))
        },
        |tcb| {
            let roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.contains_key(&self.handle_id)
        },
    );
    
    if !in_roots {
        return false;
    }
    
    // Additional GcBox state checks (same as resolve())
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        if gc_box.is_under_construction() {
            return false;
        }
        if gc_box.has_dead_flag() {
            return false;
        }
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
從 GC 角度，`is_valid()` 應該返回 true 當且僅當 handle 可以被安全地 resolve。如果 GcBox 已經死亡（has_dead_flag），那麼 `is_valid()` 返回 true 會誤導用戶，以為可以調用 `resolve()` 來獲取有效的 Gc。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題（不會導致 UB），但 API 使用體驗不佳。用戶可能會根據 `is_valid()` 的返回值做出錯誤假設，導致程式 panic。

**Geohot (Exploit 觀點):**
難以利用。但潛在場景：如果攻擊者能控制時序，可能造成：
1. 線程 A 調用 `is_valid()` 返回 true
2. 線程 B 觸發 GC 收集對象
3. 線程 A 調用 `resolve()` panic
4. 這可用於 DoS（Denial of Service）

---

## 🔗 相關 Issue

- bug128: GcHandle::is_valid() TOCTOU - 已修復 root list 檢查
- bug39: GcHandle::resolve() missing validity check

---

## Resolution (2026-03-01)

**Outcome:** Invalid.

The issue incorrectly assumes that `is_valid() == true` can coexist with `has_dead_flag()`, `dropping_state() != 0`, or `is_under_construction()` on the underlying `GcBox`. This is impossible by design: registering a `GcHandle` as a **strong root** prevents the GC from collecting or dropping the object while the root entry remains in the root list. Therefore, whenever `is_valid()` returns `true`, the GcBox state checks inside `resolve()` will always pass.

**Existing test confirms this:** `test_handle_keeps_alive` (in `tests/cross_thread_handle.rs`) drops the original `Gc<T>`, runs `collect_full()`, and then calls `resolve()` — which succeeds. This demonstrates that the handle root actively keeps the object alive, making the "is_valid() true but resolve() panics" scenario impossible.

The only way `resolve()` can panic when `is_valid()` is `true` is a **thread mismatch** (called from the wrong thread), which is intentionally orthogonal to `is_valid()` — the handle's validity is thread-agnostic by design. No source code changes required.
