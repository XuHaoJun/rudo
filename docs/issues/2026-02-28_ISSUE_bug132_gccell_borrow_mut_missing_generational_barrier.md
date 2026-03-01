# [Bug]: GcCell::borrow_mut 缺少 Generational Barrier 檢查，與 GcRwLockWriteGuard 行為不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 當 Generational GC 模式啟用但 Incremental Marking 關閉時觸發 |
| **Severity (嚴重程度)** | `High` | 導致 Young 物件被錯誤回收，造成記憶體安全問題 |
| **Reproducibility (復現難度)** | `Medium` | 需要僅啟用generational barrier的場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcCell::borrow_mut` 函數在標記新的 GC 指針為黑色時，只檢查 `is_incremental_marking_active()`，但缺少 `is_generational_barrier_active()` 檢查。

這與 `GcRwLockWriteGuard::drop` 和 `GcMutexGuard::drop` 的實現不一致，後兩者都正確檢查了兩種 barrier。

### 預期行為 (Expected Behavior)
當 Generational Barrier 啟用時（無論 Incremental Marking 是否啟用），`borrow_mut` 應該標記新的 GC 指針。

### 實際行為 (Actual Behavior)
當只有 Generational Barrier 啟用（Incremental Marking 關閉）時，`GcCell::borrow_mut` 不會標記新的 GC 指針，導致：
1. 新指標指向的 Young 物件可能在標記期間不被視為可達
2. 與 GcRwLockWriteGuard::drop 行為不一致

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/cell.rs:155-208`:

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    // BUG: 只檢查 incremental marking，應該也要檢查 generational barrier!
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();

    if incremental_active {
        // ... capture old GC pointers to SATB
    }

    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active);

    let result = self.inner.borrow_mut();

    // BUG: 這裡只檢查 incremental_active!
    if incremental_active {
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

    result
}
```

對比 `GcRwLockWriteGuard::drop` (sync.rs:410-420) 的正確實現:
```rust
fn drop(&mut self) {
    let barrier_active_at_start =
        is_generational_barrier_active() || is_incremental_marking_active();
    // ...
    if barrier_active_at_start || barrier_active_before_mark {
        for gc_ptr in ptrs {
            let _ = unsafe {
                crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
            };
        }
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 關閉 Incremental Marking，只啟用 Generational Barrier
2. 創建 `Gc<GcCell<Gc<T>>>` 
3. 調用 `cell.borrow_mut()` 並寫入新的 GC 指針
4. 執行並發標記
5. 驗證新的 GC 指標指向的物件是否被正確標記

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `GcCell::borrow_mut` 以檢查兩種 barrier：

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    // 修復：檢查兩種 barrier
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    let barrier_active = generational_active || incremental_active;

    if incremental_active {
        // ... capture old GC pointers to SATB (only needed for incremental)
    }

    crate::heap::gc_cell_validate_and_barrier(ptr, "borrow_mut", incremental_active);

    let result = self.inner.borrow_mut();

    // 修復：使用 combined barrier check
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

    result
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generational barrier 的核心目的是記錄 OLD→YOUNG 引用。當新的 GC 指針被存儲到 OLD 物件時，應該標記該指針指向的物件，以確保在並發標記期間該物件被視為可達。這與 incremental marking 的 SATB  semantics 不同，但在兩種 barrier 啟用時都需要正確處理。

**Rustacean (Soundness 觀點):**
當 generational barrier 啟用但 incremental marking 關閉時，新的 GC 指針可能不會被標記，導致潛在的記憶體安全問題。這與 GcRwLockWriteGuard 的實現不一致，後者正確處理了這種情況。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過觸發 generational barrier 模式並在borrow_mut 期間觀察不一致的行為來利用此 bug。雖然實際利用需要精確的時序控制，但這是可能的攻擊向量。

---

## Resolution (2026-03-01)

**Outcome:** Fixed.

**Root cause confirmed:** `GcCell::borrow_mut` in `cell.rs` (lines 155–211) only checked `incremental_active` when deciding whether to mark new GC pointers after mutation, missing the `generational_active` case.

**Fix applied:** Added `let generational_active = crate::gc::incremental::is_generational_barrier_active();` and combined both flags into `let barrier_active = generational_active || incremental_active;`. The "mark new pointers" block now uses `barrier_active`, matching the pattern in `GcRwLockWriteGuard::drop` and `GcMutexGuard::drop`. The SATB old-value capture block remains gated on `incremental_active` only (SATB is incremental-specific).

**Verification:** Full test suite passes (`bash test.sh`). Clippy reports no warnings.
