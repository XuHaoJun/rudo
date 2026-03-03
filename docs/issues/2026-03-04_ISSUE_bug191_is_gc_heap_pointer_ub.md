# [Bug]: is_gc_heap_pointer has UB due to dereferencing potentially invalid pointer without validation

**Status:** Open
**Tags:** Verified


## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Dead code - functions are never called |
| **Severity (嚴重程度)** | Medium | UB if called with invalid pointer |
| **Reproducibility (復現難度)** | N/A | Dead code - cannot be triggered |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `GcCellImpl::is_gc_heap_pointer` in `cell.rs:1235-1253`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 問題描述 (Description)

The function `is_gc_heap_pointer` has undefined behavior (UB) because it dereferences a potentially invalid pointer before validating it.

### 預期行為

The function should validate that the pointer is within the GC heap before dereferencing any memory.

### 實際行為

The function calculates `page_addr = addr & page_mask()` and then immediately dereferences `(*header.as_ptr()).magic` without first checking if `ptr` points to valid GC heap memory.

```rust
#[inline]
fn is_gc_heap_pointer(ptr: *const u8) -> bool {
    let addr = ptr as usize;
    if addr == 0 {
        return false;
    }

    unsafe {
        crate::heap::with_heap(|heap| {
            let page_addr = addr & crate::heap::page_mask();

            if heap.large_object_map.contains_key(&page_addr) {
                return true;
            }

            let header = ptr_to_page_header(ptr);  // No validation!
            (*header.as_ptr()).magic == MAGIC_GC_PAGE  // UB: Dereferencing potentially invalid pointer
        })
    }
}
```

If you pass a random pointer (e.g., `0x1234_5678`), the function computes `page_addr = 0x1234_5000` (for 4KB pages) and then reads `magic` from that arbitrary memory location - this is undefined behavior.

---

## 根本原因分析 (Root Cause Analysis)

The function `ptr_to_page_header` just masks the address with `page_mask()` to get a page-aligned address, then casts it to `*mut PageHeader`. It does **NOT** validate that `ptr` is actually within the GC heap.

The caller should validate one of:
1. Heap bounds (`ptr_addr < heap.min_addr || ptr_addr > heap.max_addr`)
2. Magic check (`(*header.as_ptr()).magic == MAGIC_GC_PAGE`)

The function does #2 AFTER dereferencing, but this is still UB because the dereference itself is undefined behavior when the address is not valid.

---

## 重現步驟 / 概念驗證 (PoC)

This is dead code - `incremental_write_barrier` and `generational_write_barrier` which call `is_gc_heap_pointer` are never invoked. They are marked with `#[allow(dead_code)]`.

However, the UB exists in the code and would manifest if these functions were ever called with a non-GC pointer.

---

## 建議修復方案 (Suggested Fix)

Add heap bounds validation before dereferencing:

```rust
fn is_gc_heap_pointer(ptr: *const u8) -> bool {
    let addr = ptr as usize;
    if addr == 0 {
        return false;
    }

    unsafe {
        crate::heap::with_heap(|heap| {
            // FIX: Check heap bounds first
            if addr < heap.min_addr || addr > heap.max_addr {
                return false;
            }

            let page_addr = addr & crate::heap::page_mask();

            if heap.large_object_map.contains_key(&page_addr) {
                return true;
            }

            let header = ptr_to_page_header(ptr);
            (*header.as_ptr()).magic == MAGIC_GC_PAGE
        })
    }
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is dead code from an older barrier implementation. The current `unified_write_barrier` in heap.rs properly validates heap bounds before dereferencing. However, leaving dead code with UB is dangerous as it could be accidentally re-enabled in the future.

**Rustacean (Soundness 觀點):**
Dereferencing an invalid pointer is undefined behavior even in unsafe Rust. The code should either be removed or fixed.

**Geohot (Exploit 攻擊觀點):**
If this dead code is ever re-enabled without fixing, it could be exploited to read arbitrary memory or cause crashes.

---

## 驗證記錄 (Verification Record)

**驗證日期:** 2026-03-04
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `crates/rudo-gc/src/cell.rs:1235-1253`:

1. 函數 `is_gc_heap_pointer` 確實存在於 cell.rs 第 1235-1253 行
2. 函數在沒有驗證指標是否在 heap 範圍內的情況下直接解引用 `(*header.as_ptr()).magic`
3. 對比 heap.rs 中的 `incremental_write_barrier` 函數（第 2971 行），該函數正確地先檢查 heap bounds
4. 這個 UB bug 確實存在，如果這些 dead code 函數被重新啟用，會造成問題
