# [Bug]: GcRwLockWriteGuard/GcMutexGuard Drop uses stale cached incremental_active value

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 lock acquisition 和 drop 之间 incremental marking phase 发生转换 |
| **Severity (嚴重程度)** | High | 可能导致年轻对象被错误回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精确的时序控制，单线程无法重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()`, `GcMutexGuard::drop()` (`sync.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcRwLockWriteGuard` 和 `GcMutexGuard` 在 Drop 时调用 `unified_write_barrier` 应该使用当前的 incremental marking 状态，以确保 remembered buffer 被正确更新。

### 實際行為 (Actual Behavior)
当前实现缓存了 `incremental_active` 值（在 lock acquisition 时），并在 drop 时使用缓存值：

```rust
// sync.rs:470-493 (GcRwLockWriteGuard::drop)
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let incremental_active = self.incremental_active;  // 缓存的值！
        let generational_active = self.generational_active;
        // ...
        if generational_active || incremental_active {
            let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, incremental_active);  // 使用缓存值
        }
    }
}
```

问题：当 incremental marking 在 lock acquisition 和 drop 之间变为 active 时，使用的是缓存的 `incremental_active = false`，导致 `unified_write_barrier(ptr, false)` 被调用，而不是 `unified_write_barrier(ptr, true)`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 问题代码

`sync.rs` 第 287-301 行（`GcRwLock::write()`）缓存了 barrier 状态：

```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    // 在 lock acquisition 时缓存 barrier 状态
    let incremental_active = is_incremental_marking_active();
    let generational_active = is_generational_barrier_active();
    record_satb_old_values_with_state(&*guard, incremental_active);
    self.trigger_write_barrier_with_state(generational_active, incremental_active);
    mark_gc_ptrs_immediate(&*guard, incremental_active);
    GcRwLockWriteGuard {
        guard,
        _marker: PhantomData,
        incremental_active,  // 缓存并存储
        generational_active,
    }
}
```

`sync.rs` 第 470-493 行（`GcRwLockWriteGuard::drop()`）使用缓存值：

```rust
fn drop(&mut self) {
    let incremental_active = self.incremental_active;  // 来自 acquisition 时缓存的值
    let generational_active = self.generational_active;
    // ...
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);  // 使用过时值！
    }
}
```

### 问题场景

1. Thread A 调用 `GcRwLock::write()`，此时 `incremental_active = false`（incremental marking 未激活）
2. 在 lock 持有期间，incremental marking 变为 active（phase 变为 Marking）
3. Thread A 调用 `drop()`
4. 使用缓存的 `incremental_active = false` 调用 `unified_write_barrier(ptr, false)`
5. 由于 `incremental_active = false`，remembered buffer 不会被更新（line 3118-3121 in heap.rs）
6. 如果 generational barrier 也未激活，页面不会被标记为 dirty

```rust
// heap.rs:3118-3121 (unified_write_barrier)
if incremental_active {
    std::sync::atomic::fence(Ordering::AcqRel);
    heap.record_in_remembered_buffer(header);  // 当 incremental_active=false 时跳过！
}
```

### 时序问题

```
T1: Thread A acquires lock, caches incremental_active=false
T2: Collector starts incremental marking, phase -> Marking
T3: incremental_active is now true at global scope
T4: Thread A drops lock
T5: Drop uses cached incremental_active=false
T6: unified_write_barrier(ptr, false) called - remembered buffer NOT updated!
```

---

## 💣 重現步驟 / 概念驗證 (PoC)

此 bug 需要精确的时序控制，难以在单线程环境重现。建议：

1. 使用 ThreadSanitizer (TSan) 检测数据竞争
2. 创建多线程 stress test，同时触发 GC 配置变更
3. 使用 miri-test.sh 运行并发测试

理论 PoC：
```rust
// Thread A
let guard = rwlock.write();
// 修改 OLD->YOUNG 引用

// Thread B (concurrent)
// 启动 incremental marking

// Thread A
drop(guard);  // 使用过时的 incremental_active 值
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 在 Drop 时重新检查 incremental_active（推荐）

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        // 重新检查当前状态，而不是使用缓存值
        let current_incremental = crate::gc::incremental::is_incremental_marking_active();
        let current_generational = crate::gc::incremental::is_generational_barrier_active();
        
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        // Mark new GC pointers black during incremental marking
        if current_incremental {
            for gc_ptr in &ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }

        // Call barrier with CURRENT state
        if current_generational || current_incremental {
            let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
            crate::heap::unified_write_barrier(ptr, current_incremental);
        }
    }
}
```

注意：这引入了 TOCTOU 窗口，但 `mark_object_black` 是幂等的，且 barrier 调用是 "at least once" 语义。

### 方案 2: 始终使用当前状态调用 barrier

无论缓存值如何，在 drop 时使用当前状态：

```rust
if generational_active || incremental_active {
    let current_incremental = crate::gc::incremental::is_incremental_marking_active();
    let ptr = std::ptr::from_ref(&*self.guard).cast::<u8>();
    crate::heap::unified_write_barrier(ptr, current_incremental);
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier 的关键不变量是：在 marking 开始时可达的对象必须保持可达。如果 incremental marking 在 lock acquisition 和 drop 之间变为 active，我们需要在 drop 时使用正确的状态来更新 remembered buffer。使用缓存的过时值会破坏这个不变量。

**Rustacean (Soundness 觀點):**
这是一个内存安全问题。如果 remember buffer 未被正确更新，年轻的 GC 对象可能被错误回收，导致 use-after-free。这是 Rust 中的未定义行为形式。

**Geohot (Exploit 觀點):**
虽然需要精确的时序控制，攻击者可能通过构造特定的执行时序来触发此 bug。在极端情况下，这可能导致 memory corruption。

---

## 備註

此 bug 与以下已修复的 bug 相关但不同：
- bug122: Drop barrier 条件不一致 - 已修复（条件改为 `incremental_active || generational_active`）
- bug160/161: Drop TOCTOU - 已修复（总是标记）
- bug302/304: mark_object_black 调用不正确 - 已修复

此 bug 特指：cached `incremental_active` 值在 drop 时可能过时，导致 `unified_write_barrier` 被调用时使用错误的 `incremental_active` 参数。

---

## 修復記錄 (Fix Record)

- **Date:** 2026-03-24
- **Fixed:** `GcRwLockWriteGuard::drop()` and `GcMutexGuard::drop()` now re-check `is_incremental_marking_active()` and `is_generational_barrier_active()` at drop time instead of using cached values. Also removed the now-unused `incremental_active` and `generational_active` fields from the guard structs.
- **Changes:**
  - `sync.rs`: Modified `GcRwLockWriteGuard::drop()` (line 470) to re-check current barrier state
  - `sync.rs`: Modified `GcMutexGuard::drop()` (line 747) to re-check current barrier state
  - `sync.rs`: Removed `incremental_active` and `generational_active` fields from `GcRwLockWriteGuard` struct
  - `sync.rs`: Removed `incremental_active` and `generational_active` fields from `GcMutexGuard` struct
  - `sync.rs`: Updated `write()`, `try_write()`, `lock()`, and `try_lock()` to not store barrier state in guards
- **Verification:** Clippy passes, all tests pass.
