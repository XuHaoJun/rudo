# [Bug]: scan_dirty_page_minor_trace 缺少 is_allocated 檢查可能導致 UAF

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 在 Minor GC 期間，slot 被 sweep 後重新分配且標記為 dirty |
| **Severity (嚴重程度)** | `High` | 可能導致 GC 嘗試追蹤已釋放記憶體，造成不確定行為 |
| **Reproducibility (復現難度)** | `Medium` | 需要精確時序控制，或可通過單執行緒重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `scan_dirty_page_minor_trace()` in `gc/gc.rs:1101-1123`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`scan_dirty_page_minor_trace()` 應該只追蹤有效的（已分配的）GC 物件。在調用 `trace_fn` 前應檢查 `is_allocated` 以確保物件未被釋放並重新分配。

### 實際行為 (Actual Behavior)
當執行 Minor GC 時，`scan_dirty_page_minor_trace()` 遍歷 dirty pages 中的所有 dirty slots，並直接調用 `trace_fn` 而不檢查 `is_allocated`。如果一個 slot 已經被 sweep 後重新分配（且新物件也被標記為 dirty），GC 可能會嘗試追蹤錯誤的物件或已釋放的記憶體。

對比其兄弟函數 `scan_dirty_page_minor()` (gc/gc.rs:1074)，後者正確地調用了 `mark_and_trace_incremental`，該函數在內部進行了 `is_allocated` 檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/gc/gc.rs` 的 `scan_dirty_page_minor_trace` 函數 (lines 1101-1123)：

```rust
unsafe fn scan_dirty_page_minor_trace(page_ptr: NonNull<PageHeader>, visitor: &mut GcVisitor) {
    let header = page_ptr.as_ptr();
    if (*header).is_large_object() {
        let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
        #[allow(clippy::cast_ptr_alignment)]
        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
        // Bug: 沒有檢查 is_allocated！
        ((*gc_box_ptr).trace_fn)(obj_ptr, visitor);
    } else {
        let obj_count = (*header).obj_count as usize;
        for i in 0..obj_count {
            if (*header).is_dirty(i) {
                let block_size = (*header).block_size as usize;
                let header_size = PageHeader::header_size(block_size);
                let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                // Bug: 沒有檢查 is_allocated！
                ((*gc_box_ptr).trace_fn)(obj_ptr, visitor);
            }
        }
    }
    // ...
}
```

相比之下，`scan_dirty_page_minor()` 正確地調用 `mark_and_trace_incremental`：

```rust
unsafe fn scan_dirty_page_minor(page_ptr: NonNull<PageHeader>, visitor: &mut GcVisitor) {
    // ...
    mark_and_trace_incremental(std::ptr::NonNull::new_unchecked(gc_box_ptr), visitor);
    // ...
}
```

`mark_and_trace_incremental` 內部有 `is_allocated` 檢查 (gc/gc.rs:2400)：

```rust
unsafe fn mark_and_trace_incremental(ptr: NonNull<GcBox<()>>, visitor: &mut GcVisitor) {
    // ...
    // Skip if slot was swept and potentially reused; avoids UAF
    if !(*header.as_ptr()).is_allocated(index) {
        return;
    }
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 創建 GC 物件 A，位於 page P 的 slot S
2. 執行 Minor GC 讓 A promotion 到 old generation，並標記 page P 為 dirty
3. 執行 full GC 讓 page P 被 sweep，slot S 被釋放
4. 在同一個 slot S 分配新物件 B
5. 對 slot S 進行 mutation，觸發 dirty bit
6. 執行 Minor GC
7. `scan_dirty_page_minor_trace` 會遍歷 dirty slot S，並調用 trace_fn
8. 由於沒有 is_allocated 檢查，可能會追蹤到無效記憶體

---

## 🛠️ 建議修復 (Suggested Fix)

在 `scan_dirty_page_minor_trace` 中添加 `is_allocated` 檢查，類似 `scan_dirty_page_minor` 的做法：

```rust
unsafe fn scan_dirty_page_minor_trace(page_ptr: NonNull<PageHeader>, visitor: &mut GcVisitor) {
    let header = page_ptr.as_ptr();
    if (*header).is_large_object() {
        let obj_ptr = header.cast::<u8>().add((*header).header_size as usize);
        #[allow(clippy::cast_ptr_alignment)]
        let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
        
        // 添加 is_allocated 檢查
        if let Some(idx) = ptr_to_object_index(gc_box_ptr) {
            if (*header).is_allocated(idx) {
                ((*gc_box_ptr).trace_fn)(obj_ptr, visitor);
            }
        }
    } else {
        let obj_count = (*header).obj_count as usize;
        for i in 0..obj_count {
            if (*header).is_dirty(i) {
                // 添加 is_allocated 檢查
                if !(*header).is_allocated(i) {
                    continue;
                }
                let block_size = (*header).block_size as usize;
                let header_size = PageHeader::header_size(block_size);
                let obj_ptr = header.cast::<u8>().add(header_size + (i * block_size));
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                ((*gc_box_ptr).trace_fn)(obj_ptr, visitor);
            }
        }
    }
    // ...
}
```

或者，更好的做法是重構為調用 `mark_and_trace_incremental`，就像 `scan_dirty_page_minor` 那樣。

---

## 💬 內部討論記錄 (Internal Discussion Record)

### R. Kent Dybvig (GC Expert)

The issue is clear: `scan_dirty_page_minor_trace` directly invokes `trace_fn` without validating slot allocation state. This mirrors the bug in `trace_and_mark_object` (bug274). The fix should mirror that solution: add `is_allocated` checks before dereferencing, or refactor to use `mark_and_trace_incremental` which already has proper checks.

### Rustacean (Memory Safety Expert)

This is a use-after-free vulnerability in unsafe code. The function doesn't check `is_allocated` before calling `trace_fn`, which could access freed memory. This pattern is exactly what bug274 fixed in `trace_and_mark_object`. All code paths that access GC objects must verify allocation status first.

### Geohot (Exploit/Edge Case Expert)

The attack requires precise timing: allocate object A → promote to old gen → trigger minor GC → sweep frees slot → reallocate object B in same slot → mutate to mark dirty → trigger minor GC again. If an attacker can control allocation timing, they could potentially cause the GC to trace arbitrary memory contents. The impact is limited by the need for concurrent allocation, but the bug is real.
