# [Bug]: GcThreadSafeRefMut::drop() uses stale cached incremental_active value (bug409 pattern)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 borrow_mut 和 drop 之间 incremental marking phase 发生转换 |
| **Severity (嚴重程度)** | High | 可能导致年轻对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut::drop()` (`cell.rs:1429-1454`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcThreadSafeRefMut` 在 Drop 时调用 `unified_write_barrier` 应该使用当前的 incremental marking 状态，就像 `GcRwLockWriteGuard` 和 `GcMutexGuard` 在 bug409 修复中所做的那样。

### 實際行為 (Actual Behavior)
当前实现缓存了 `incremental_active` 和 `generational_active` 值（在 `borrow_mut` 时），并在 drop 时使用缓存值：

```rust
// cell.rs:1429-1432 (GcThreadSafeRefMut::drop)
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        let incremental_active = self.incremental_active;  // 缓存的值！
        let generational_active = self.generational_active;  // 缓存的值！
        // ...
    }
}
```

问题：当 incremental marking 在 `borrow_mut()` 和 `drop()` 之间变为 active 时，使用的是缓存的 `incremental_active = false`，导致 `unified_write_barrier(ptr, false)` 被调用，而不是 `unified_write_barrier(ptr, true)`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 问题代码

`cell.rs` 第 1127-1132 行（`GcThreadSafeCell::borrow_mut()`）缓存了 barrier 状态：

```rust
GcThreadSafeRefMut {
    inner: guard,
    _marker: std::marker::PhantomData,
    incremental_active,  // 缓存并存储
    generational_active,
}
```

`cell.rs` 第 1429-1454 行（`GcThreadSafeRefMut::drop()`）使用缓存值：

```rust
fn drop(&mut self) {
    let incremental_active = self.incremental_active;  // 来自 borrow_mut() 时缓存的值
    let generational_active = self.generational_active;
    // ...
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);  // 使用过时值！
    }
}
```

### 对比：sync.rs 的修复（bug409）

`sync.rs` 第 469-472 行（`GcRwLockWriteGuard::drop()`）正确地重新检查：

```rust
// FIX bug409: Re-check current barrier state instead of using cached values.
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
let generational_active = crate::gc::incremental::is_generational_barrier_active();
```

`GcMutexGuard::drop()` 也做了相同的修复。

但 `GcThreadSafeRefMut::drop()` 没有应用这个修复！

### 问题场景

1. Thread A 调用 `GcThreadSafeCell::borrow_mut()`，此时 `incremental_active = false`（incremental marking 未激活）
2. 在 lock 持有期间，incremental marking 变为 active（phase 变为 Marking）
3. Thread A 调用 `drop()`
4. 使用缓存的 `incremental_active = false` 调用 `unified_write_barrier(ptr, false)`
5. 由于 `incremental_active = false`，remembered buffer 不会被更新
6. 如果 generational barrier 也未激活，页面不会被标记为 dirty

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精确的时序控制，难以在单线程环境重现。建议：

1. 使用 ThreadSanitizer (TSan) 检测数据竞争
2. 创建多线程 stress test，同时触发 GC 配置变更
3. 使用 miri-test.sh 运行并发测试

理论 PoC：
```rust
// Thread A
let guard = cell.borrow_mut();
// 修改 OLD->YOUNG 引用

// Thread B (concurrent)
// 启动 incremental marking

// Thread A
drop(guard);  // 使用过时的 incremental_active 值
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案：重新检查 Drop 时的 barrier 状态

修改 `GcThreadSafeRefMut::drop()` 以重新检查当前状态：

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        (*self.inner).capture_gc_ptrs_into(&mut ptrs);

        // FIX bug409 (cell.rs): Re-check current barrier state instead of using cached values.
        // The incremental marking phase may have started after borrow_mut().
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();

        // Mark new GC pointers black only during incremental marking (bug302).
        if incremental_active {
            for gc_ptr in &ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }

        // Call barrier with CURRENT state
        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(&*self.inner).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier 的关键不变量是：在 marking 开始时可达的对象必须保持可达。如果 incremental marking 在 borrow_mut 和 drop 之间变为 active，我们需要在 drop 时使用正确的状态来更新 remembered buffer。使用缓存的过时值会破坏这个不变量。

**Rustacean (Soundness 觀點):**
这是一个内存安全问题。如果 remember buffer 未被正确更新，年轻的 GC 对象可能被错误回收，导致 use-after-free。这是 Rust 中的未定义行为形式。

**Geohot (Exploit 觀點):**
虽然需要精确的时序控制，攻击者可能通过构造特定的执行时序来触发此 bug。在极端情况下，这可能导致 memory corruption。

---

## 備註

此 bug 是 bug409 的延伸：
- bug409 修复了 `GcRwLockWriteGuard::drop()` 和 `GcMutexGuard::drop()` 中的相同问题
- 但 `GcThreadSafeRefMut::drop()` 没有应用相同的修复

相关 bug：
- bug302: GcThreadSafeRefMut::drop incorrectly marks GC pointers during generational barrier
- bug122: GcThreadSafeCell::borrow_mut missing barrier check
- bug160/161: GcThreadSafeRefMut::drop TOCTOU

---

## Fix Applied (2026-03-24)

**File Modified:** `crates/rudo-gc/src/cell.rs`

**Change:** `GcThreadSafeRefMut::drop()` now re-checks barrier state at drop time instead of using cached values.

```rust
// Before (buggy):
let incremental_active = self.incremental_active;  // CACHED value!
let generational_active = self.generational_active;  // CACHED value!

// After (fixed):
// FIX bug411: Re-check current barrier state instead of using cached values.
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
let generational_active = crate::gc::incremental::is_generational_barrier_active();
```

This matches the fix applied to `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` in bug409.
