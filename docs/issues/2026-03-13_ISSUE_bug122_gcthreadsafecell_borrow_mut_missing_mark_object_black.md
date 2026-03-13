# [Bug]: GcThreadSafeCell::borrow_mut() 缺少標記新 GC 指針為黑色的程式碼

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 incremental/generational marking 活躍時使用 GcThreadSafeCell |
| **Severity (嚴重程度)** | High | 可能導致新分配的 GC 物件被錯誤回收，造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要minor GC測試來驗證 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut()`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcThreadSafeCell::borrow_mut()` 應該與 `GcCell::borrow_mut()` 保持一致的行为：
- 計算 `barrier_active = generational_active || incremental_active`
- 當 barrier 活躍時，捕獲新的 GC 指針並將它們標記為黑色（live）

### 實際行為 (Actual Behavior)

`GcThreadSafeCell::borrow_mut()` 缺少將新 GC 指針標記為黑色的代碼。

在 `GcCell::borrow_mut()` (cell.rs:193-208) 中：
```rust
if barrier_active {
    unsafe {
        let new_value = &*result;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            crate::heap::with_heap(|_heap| {
                for gc_ptr in new_gc_ptrs {
                    let _ = crate::gc::incremental::mark_object_black(
                        gc_ptr.as_ptr() as *const u8
                    );
                }
            });
        }
    }
}
```

但在 `GcThreadSafeCell::borrow_mut()` (cell.rs:1041-1087) 中：
```rust
self.trigger_write_barrier_with_incremental(incremental_active, generational_active);

GcThreadSafeRefMut {
    inner: guard,
    _marker: std::marker::PhantomData,
}
// 缺少：標記新 GC 指針為黑色的代碼！
```

### 程式碼位置

`cell.rs` 第 1081-1086 行 - 缺少 `barrier_active` 條件下的 `mark_object_black` 調用。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`GcThreadSafeCell::borrow_mut()` 與 `GcCell::borrow_mut()` 的實現不一致：

1. **GcCell** 有完整的 barrier 實現：
   - 計算 `barrier_active = generational_active || incremental_active`
   - 捕獲並記錄舊值（SATB）
   - 觸發 write barrier
   - **標記新 GC 指針為黑色**（bug132 修復）

2. **GcThreadSafeCell** 不完整：
   - 計算 `incremental_active` 和 `generational_active`
   - 捕獲並記錄舊值（SATB）- 觸發 write barrier
   - **缺少：標記新 GC 指針為黑色**

這導致當 generational 或 incremental marking 活躍時，賦值給 `GcThreadSafeCell` 的新 GC 物件可能會被錯誤回收。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect};
use std::sync::Arc;
use std::thread;

#[derive(Trace, Clone)]
struct Data {
    value: i32,
}

#[derive(Trace)]
struct Container {
    cell: GcThreadSafeCell<Option<Gc<Data>>>,
}

// 啟用 incremental marking
rudo_gc::set_incremental_config(rudo_gc::gc::incremental::IncrementalConfig {
    enabled: true,
    ..Default::default()
});

let gc = Gc::new(Container {
    cell: GcThreadSafeCell::new(None),
});

// 建立 OLD→YOUNG 引用：先 promote
collect_full();

// 在 OLD 物件中建立新的 young GC 指針
{
    let mut container = gc.cell.borrow_mut();
    *container = Some(Gc::new(Data { value: 42 }));
}

// 運行 minor GC（不應該回收 young 物件）
collect();

// 嘗試訪問 - 如果 bug 存在，這裡可能會 use-after-free
if let Some(d) = &*gc.cell.borrow() {
    println!("Value: {}", d.value); // 可能會崩潰！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcThreadSafeCell::borrow_mut()` 的第 1081 行之後添加：

```rust
let barrier_active = incremental_active || generational_active;

if barrier_active {
    let new_value = &*guard;
    let mut new_gc_ptrs = Vec::with_capacity(32);
    new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
    if !new_gc_ptrs.is_empty() {
        crate::heap::with_heap(|_heap| {
            for gc_ptr in new_gc_ptrs {
                let _ = crate::gc::incremental::mark_object_black(
                    gc_ptr.as_ptr() as *const u8
                );
            }
        });
    }
}

GcThreadSafeRefMut {
    inner: guard,
    _marker: std::marker::PhantomData,
}
```

注意：這與 `GcCell::borrow_mut()` 的實現完全一致（參考 bug132 修復）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個實現不一致的問題。`GcThreadSafeCell` 是線程安全的 cell，應該與 `GcCell` 保持相同的 barrier 语义。缺少 `mark_object_black` 会导致 SATB  invariant 被破壞。

**Rustacean (Soundness 觀點):**
這不是 UB，但可能導致 use-after-free。如果新的 GC 物件被錯誤回收，而代碼仍然嘗試訪問它，會造成記憶體安全問題。

**Geohot (Exploit 攻擊觀點):**
這個 bug 可以被利用來進行 use-after-free 攻擊。攻擊者可以通過觸發 GC 來釋放目標物件，然後利用 TOCTOU 漏洞訪問已釋放的記憶體。

---

## 修復狀態

- [x] 已修復
- [ ] 未修復

---

## Resolution (2026-03-13)

**Fix applied:** `GcThreadSafeRefMut::drop()` already called `mark_object_black` when `incremental_active`, but did not when only `generational_active`. Updated the condition to `incremental_active || generational_active` to match `GcCell::borrow_mut()` behavior (barrier_active = generational || incremental). The Drop is the correct place because mutation occurs through the guard; we capture the final value after the user writes.