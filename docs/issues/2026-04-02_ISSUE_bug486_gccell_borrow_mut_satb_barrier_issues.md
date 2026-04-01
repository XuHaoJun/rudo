# [Bug]: GcCell::borrow_mut SATB barrier issues - OLD values not recorded when incremental_inactive, NEW values not marked when incremental transitions

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確的時序控制，單執行緒難以重現 |
| **Severity (嚴重程度)** | High | 可能導致年輕對象被錯誤回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要多執行緒並發控制，或依賴 GC 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut()` (`cell.rs:155-216`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcCell::borrow_mut()` 應該：
1. 總是捕獲 OLD GC 指針值用於 SATB barrier（當 generational_active = true 時）
2. 總是標記 NEW GC 指針值為黑色（當任何 barrier 活躍時）

### 實際行為 (Actual Behavior)

**問題 1：OLD 值未記錄**
- 當 `incremental_active = false` 時，OLD 值不被捕獲
- `borrow_mut()` 第 170 行：`if incremental_active { ... capture OLD ... }`

**問題 2：NEW 值未標記**
- 當 `incremental_active = false` 時，NEW 值不被標記為黑色
- `borrow_mut()` 第 198 行：`if incremental_active { ... mark NEW black ... }`

### 對比 `GcThreadSafeCell::borrow_mut()` (bug484, bug485)

`GcThreadSafeCell::borrow_mut()` 有同樣的問題（已報告但未修復）：
- Bug484: OLD 值未記錄
- Bug485: NEW 值未標記

但 `GcCell::borrow_mut()`（單執行緒版本）似乎沒有同等的 issue。

### 代碼位置

`cell.rs:155-216` (`GcCell::borrow_mut`):

```rust
// Line 167-168: 緩存 barrier 狀態
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
let generational_active = crate::gc::incremental::is_generational_barrier_active();

// Line 170-189: 問題 1 - 只有 incremental_active = true 時捕獲 OLD
if incremental_active {
    // ... capture OLD values ...
}

// Line 191-193: 觸發 barrier（這個是正確的）
if generational_active || incremental_active {
    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active);
}

// Line 196: 獲取 mutable borrow
let result = self.inner.borrow_mut();

// Line 198-213: 問題 2 - 只有 incremental_active = true 時標記 NEW
if incremental_active {
    // ... mark NEW values black ...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題 1：OLD 值未記錄（當 incremental_active = false）

**場景：**
1. `incremental_active = false`（major GC 結束，進入 idle）
2. 用戶調用 `borrow_mut()`
3. OLD 值不被捕獲（因為 `incremental_active = false`）
4. 用戶修改 GC 指針：`cell = old_ptr` → `cell = new_ptr`
5. 如果後來 `incremental_active` 變為 true：
   - OLD 值可達的對象可能未被保留
   - 造成 use-after-free

**時序問題：**
```
T1: borrow_mut() called, incremental_active = false
T2: OLD values NOT captured (incremental_active = false)
T3: User modifies cell = old_ptr -> new_ptr
T4: Later, incremental_active becomes true
T5: Objects reachable only from old_ptr may be prematurely collected!
```

### 問題 2：NEW 值未標記（當 incremental_active = false 但轉換為 true）

**場景：**
1. `incremental_active = false`（但即將變為 true）
2. 用戶調用 `borrow_mut()`
3. OLD 值被捕獲（第 170 行的 bug484 fix? 沒有！）
4. Barrier 觸發
5. `incremental_active` 變為 true
6. `borrow_mut()` 返回後，NEW 值不被標記

**時序問題：**
```
T1: borrow_mut() called, incremental_active = false (cached)
T2: OLD values NOT captured (bug484 - same issue in GcCell!)
T3: incremental marking starts, incremental_active = true
T4: drop() calls mark_object_black - NEW values NOT marked!
T5: Objects reachable only from NEW values may be prematurely collected!
```

### 為何 bug475 fix 沒有應用到 GcCell::borrow_mut()

`borrow_mut_simple()` 已經有 bug475 fix（第 1151 行）：
```rust
// FIX bug475: Always capture old GC pointers for SATB, regardless of incremental_active.
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
```

但 `borrow_mut()` 沒有相同的修復：
```rust
// Line 167-168: 只緩存狀態，沒有 unconditional capture
let incremental_active = crate::gc::incremental::is_incremental_marking_active();
let generational_active = crate::gc::incremental::is_generational_barrier_active();

if incremental_active {  // BUG: 只在 incremental_active = true 時捕獲
    // ... capture OLD ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要多執行緒並發測試或精確的 GC 時序控制。

理論 PoC（需要精確時序）：
```rust
use rudo_gc::{Gc, GcCell, Trace, GcCapture, collect_full, set_incremental_config, IncrementalConfig};

#[derive(Trace, GcCapture)]
struct Data {
    value: i32,
}

fn main() {
    let cell = Gc::new(GcCell::new(vec![Gc::new(Data { value: 1 })]));
    
    // Case 1: incremental_active = false at call
    let mut guard = cell.borrow_mut();
    let old_ptr = guard.get(0).clone();
    guard[0] = Gc::new(Data { value: 2 }); // new_ptr
    
    // If incremental becomes active here...
    drop(guard);
    // OLD reachable objects may be collected!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：應用 bug475 + bug479 fix（推薦）

修改 `GcCell::borrow_mut()` 與 `borrow_mut_simple()` 和 `GcRwLock::write()` 一致：

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    self.validate_thread_affinity("borrow_mut");

    let ptr = std::ptr::from_ref(self).cast::<u8>();

    // Cache barrier states once to avoid TOCTOU
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();

    // FIX bug484: Always capture old GC pointers for SATB, regardless of incremental_active.
    // If incremental becomes active between borrow_mut() and drop(),
    // OLD values must already be recorded to preserve SATB invariant.
    {
        let value = &*self.inner.as_ptr();
        let mut gc_ptrs = Vec::with_capacity(32);
        value.capture_gc_ptrs_into(&mut gc_ptrs);
        if !gc_ptrs.is_empty() {
            crate::heap::with_heap(|heap| {
                for gc_ptr in gc_ptrs {
                    if !heap.record_satb_old_value(gc_ptr) {
                        crate::gc::incremental::IncrementalMarkState::global()
                            .request_fallback(
                                crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                            );
                        break;
                    }
                }
            });
        }
    }

    if generational_active || incremental_active {
        crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active);
    }

    let result = self.inner.borrow_mut();

    // FIX bug485: Always mark GC pointers black when barrier is active.
    // If incremental becomes active between entry and drop, NEW values
    // must also be marked to maintain SATB consistency.
    {
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

    result
}
```

### 方案 2：使用與 borrow_mut_simple 相同的模式

`borrow_mut_simple()` 已經有正確的模式。`borrow_mut()` 應該採用相同的模式。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 不變性要求：當記錄了 OLD 值時，相應的 NEW 值也應該被標記。如果 `incremental_active` 在 `borrow_mut()` 和 `drop()` 之间發生變化，OLD 和 NEW 對象都可能未被正確保護。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果對象被錯誤回收，透過 GC 指針訪問會導致 use-after-free。`GcCell::borrow_mut()` 應該在所有 barrier 活躍情況下都執行完整的 SATB 協議。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能通過控制 GC 時序來觸發此 bug。如果能夠讓 `incremental_active` 在特定時間點發生變化，可以構造 use-after-free 場景。

---

## 備註

- 與 bug484/bug485 相關：`GcThreadSafeCell::borrow_mut()` 有同樣的問題（單執行緒版本）
- 與 bug475 相關：`borrow_mut_simple()` 已經有 bug475 fix
- 與 bug479 相關：`GcRwLock::write()` 已經有 bug479 fix
- 需要 Miri 或 ThreadSanitizer 驗證
