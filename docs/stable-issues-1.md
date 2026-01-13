# rudo-gc 穩定性問題清單

根據 Gemini Code Review 與原始碼交叉驗證，以下是確認需要修復的問題：

---

## 🔴 高優先級 (High Priority)

### 1. `GcCell::Trace` 實作中的 Panic 風險

**檔案：** `cell.rs:108-112`

**問題：**
```rust
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        self.inner.borrow().trace(visitor);  // ← 可能 Panic！
    }
}
```

**情境：**
1. 程式持有 `let mut borrow = my_gc_cell.borrow_mut();`
2. 在 borrow 存活期間觸發 GC（例如透過 `Gc::new()`）
3. GC 遍歷到此 GcCell，呼叫 `trace()`
4. `RefCell::borrow()` 發現已有 mutable borrow → **Panic: "already borrowed: BorrowMutError"**

**修正建議：**
```rust
unsafe impl<T: Trace + ?Sized> Trace for GcCell<T> {
    fn trace(&self, visitor: &mut impl crate::trace::Visitor) {
        // GC 在 STW 期間執行，不會有並發寫入
        let ptr = self.inner.as_ptr();
        unsafe { (*ptr).trace(visitor); }
    }
}
```

---

### 2. Sweep 階段的 Drop 重入風險 (Single-Pass Sweep)

**檔案：** `gc.rs:876-934` (`copy_sweep_logic`)

**問題：**
目前的 sweep 實作是「單次遍歷」—— 在同一個迴圈中同時執行 `drop_fn` 和回收記憶體：

```rust
// gc.rs:907
((*gc_box_ptr).drop_fn)(obj_ptr);  // 立即執行 drop

(*header).clear_allocated(i);       // 立即回收 slot
```

**風險情境：**
1. GC 決定回收物件 A
2. 呼叫 A 的 `drop_fn`
3. A 的 Drop 內部持有 `Gc<B>` 並嘗試存取 B
4. 如果 B 已經在這個迴圈中被回收（slot 已歸還 free list）→ **Use-After-Free**

**修正建議：**
實作兩階段清除 (Two-Phase Sweep)：
- **Phase 1 (Finalize):** 遍歷所有死物件，呼叫 `drop_fn`，但不釋放記憶體
- **Phase 2 (Reclaim):** Drop 全部結束後，再回收 slots 到 free list

---

### 3. `LocalHeap::drop` 導致跨執行緒 Use-After-Free

**檔案：** `heap.rs:1302-1347`

**問題：**
當執行緒結束時，其 `LocalHeap` 被 Drop，所有 Pages 直接 unmap：

```rust
// heap.rs:1343
sys_alloc::Mmap::from_raw(header.cast::<u8>(), alloc_size);
```

**風險：**
- 系統允許跨執行緒引用（Thread A 的物件可指向 Thread B 的物件）
- 若 Thread B 結束，其 Heap 被 unmap
- Thread A 手上的指標變成懸空指標 (Dangling Pointer)
- 下次 GC 掃描到此指標 → **SIGSEGV**

**修正建議：**
實作「孤兒頁面 (Orphan Pages)」機制：
- 執行緒死亡時，不要直接 unmap Pages
- 將 Pages 轉移給 `GlobalSegmentManager` 或 "Zombie Heap"
- 等待下一次 Major GC 確認無人引用後再回收

---

### 4. Mark Bitmap 的並發寫入風險

**檔案：** `heap.rs:449-461`

**問題：**
`set_mark`, `clear_mark`, `set_dirty` 操作使用普通的 `u64` 欄位：

```rust
pub const fn set_mark(&mut self, index: usize) {
    let word = index / 64;
    let bit = index % 64;
    self.mark_bitmap[word] |= 1 << bit;  // ← 非原子操作
}
```

**風險：**
在 `perform_multi_threaded_collect` 中，多個執行緒可能同時標記同一個 Page 上的不同物件。
對同一個 `u64` 的 read-modify-write 操作若無同步，可能導致 bit 遺失（Lost Update）。

**修正建議：**
將 `mark_bitmap` 改為 `[AtomicU64; 4]` 並使用 `fetch_or` 操作：
```rust
pub fn set_mark(&self, index: usize) {
    let word = index / 64;
    let bit = index % 64;
    self.mark_bitmap[word].fetch_or(1 << bit, Ordering::Relaxed);
}
```

---

## 🟡 中優先級 (Medium Priority)

### 5. `perform_single_threaded_collect_with_wake` 喚醒順序問題

**檔案：** `gc.rs:379-445`

**問題：**
函式在執行 GC 之前就清除了 `GC_REQUESTED` flag 並喚醒等待中的執行緒：

```rust
fn perform_single_threaded_collect_with_wake() {
    // ... 先喚醒執行緒 ...
    {
        let registry = ...;
        crate::heap::GC_REQUESTED.store(false, Ordering::SeqCst);
        // wake threads...
    }
    
    // ... 然後才執行 GC ...
    crate::heap::with_heap(|heap| {
        // collection ...
    });
}
```

**風險：**
被喚醒的執行緒可能在 Collector 完成標記/清除之前就開始執行，導致：
- 分配新物件干擾標記
- 修改物件引用導致標記不一致

**修正建議：**
確保 GC 完成後才喚醒其他執行緒。

---

### 6. `Gc::drop` 中的 `is_collecting()` 檢查風險

**檔案：** `ptr.rs` (Drop impl)

**問題：**
在 GC Sweep 期間，如果物件 A 的 drop 觸發了 `Gc<B>` 的 drop：

```rust
if is_collecting() {
    unsafe {
        let header = ptr_to_page_header(ptr.as_ptr().cast());
        // 檢查 B 的 mark bit...
    }
}
```

如果 B 已被 sweep 且其 slot 已回收，存取其 Header 或 mark bit 可能是 UB。

**關聯：** 與 Issue #2 (Two-Phase Sweep) 相關，一旦實作兩階段清除，此問題將自動解決。

---

### 7. Large Object 的三份資料結構維護風險

**檔案：** `gc.rs:1000-1060` (`sweep_large_objects`)

**問題：**
Large Object 在三處維護狀態：
1. `heap.pages`
2. `heap.large_object_map`
3. `segment_manager().large_object_map`

```rust
heap.pages.retain(|&p| p != page_ptr);
heap.large_object_map.remove(&page_addr);
manager.large_object_map.remove(&page_addr);
```

如果在步驟間發生 panic，會導致 Heap 狀態不一致。

**修正建議：**
簡化資料結構，將 Large Object 管理權完全下放給 `GlobalSegmentManager`，或使用 Transaction 確保原子性。

---

## 🟢 低優先級 / 效能優化 (Low Priority)

### 8. TLAB `alloc` 中的 Bitmap 更新開銷

**檔案：** `heap.rs:568-599` (`Tlab::alloc`)

**問題：**
每次 TLAB 分配都會更新 `allocated_bitmap`：

```rust
if let Some(mut page) = self.current_page {
    header.set_allocated(idx);  // 每次分配都寫
}
```

**優化建議：**
- 對於 Young Gen TLAB，可延遲更新 bitmap
- 只在 TLAB 換頁或 GC 開始時批量設定
- GC 掃描 Young Page 時，直接掃描到 `tlab.bump_ptr` 位置即可

---

### 9. `GcCell::write_barrier` 重複計算

**檔案：** `cell.rs:77-103`

**問題：**
`write_barrier` 已經取得 `header`，但又呼叫 `ptr_to_object_index(ptr)` 內部重新計算 header：

```rust
fn write_barrier(&self) {
    let header = ptr_to_page_header(ptr);
    // ...
    if let Some(index) = ptr_to_object_index(ptr) {  // 再次算 header
        (*header.as_ptr()).set_dirty(index);
    }
}
```

**優化建議：**
手動計算 index 以避免重複的 header 查找：
```rust
let block_size = (*header.as_ptr()).block_size as usize;
let header_size = (*header.as_ptr()).header_size as usize;
let offset = ptr as usize - (header.as_ptr() as usize + header_size);
let index = offset / block_size;
```

---

### 10. `new_cyclic` 的 Rehydration 限制

**檔案：** `ptr.rs` (`rehydrate_self_refs`)

**現狀：**
程式碼註解承認此功能有限制：
```rust
// "For now, we can't easily rehydrate due to type mismatch"
```

**建議：**
暫時標記為 `unimplemented!()` 或提供明確的 API 限制說明，避免使用者誤用。

---

## 驗證狀態

| Issue # | 描述 | 驗證結果 |
|---------|------|----------|
| 1 | GcCell::Trace Panic | ✅ 確認存在 |
| 2 | Single-Pass Sweep | ✅ 確認存在 |
| 3 | LocalHeap::drop UAF | ✅ 確認存在 |
| 4 | Non-atomic Mark Bitmap | ✅ 確認存在 |
| 5 | Wake-before-GC-complete | ✅ 確認存在 |
| 6 | Gc::drop is_collecting | ✅ 確認存在（依賴 #2） |
| 7 | Large Object 三份資料 | ✅ 確認存在 |
| 8 | TLAB Bitmap 開銷 | ⚠️ 效能優化 |
| 9 | write_barrier 重複計算 | ⚠️ 效能優化 |
| 10 | new_cyclic 限制 | ⚠️ 已知限制 |

**注意：** Gemini 提到的「Write Barrier 缺失」經驗證後確認**已存在**於 `cell.rs` 中的 `GcCell::borrow_mut()` 實作。

---

## 建議修復順序

1. **Issue #1** (GcCell Trace) - 簡單修復，影響大
2. **Issue #2** (Two-Phase Sweep) - 核心安全性
3. **Issue #4** (Atomic Bitmap) - 多執行緒正確性
4. **Issue #3** (Orphan Pages) - 跨執行緒安全性
5. **Issue #5** (Wake Order) - 並發正確性
6. 其餘優化項目
