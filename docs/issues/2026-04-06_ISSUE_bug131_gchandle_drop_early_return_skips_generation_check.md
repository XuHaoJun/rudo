# [Bug]: GcHandle::drop early return when !is_allocated skips generation check and dec_ref

**Status:** Invalid
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要精確的時序控制來觸發 TOCTOU race |
| **Severity (嚴重程度)** | Medium | 可能導致 reference count leak 或 panic |
| **Reproducibility (復現難度)** | High | 需要 concurrent marking + handle drop 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::drop()`, `handles/cross_thread.rs:862-876`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::drop()` 應該在所有情況下都檢查 generation 是否改變（以檢測 slot 是否被重用），只有在 generation 匹配時才調用 `dec_ref`。如果 slot 未分配，應該先驗證 generation 再決定是否 skip `dec_ref`。

### 實際行為 (Actual Behavior)

在 `GcHandle::drop()` 中，當 `!is_allocated(idx)` 為 true 時，程式碼會 early return，跳過：
1. Generation 匹配檢查（bug407 的保護）
2. `dec_ref()` 調用

```rust
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return;  // <-- BUG: Early return 跳過 generation check 和 dec_ref!
        }
    }
    // FIX bug407: Verify generation hasn't changed before dec_ref.
    let current_generation = (*self.ptr.as_ptr()).generation();
    if pre_generation != current_generation {
        panic!("GcHandle::drop: slot was reused during drop (generation mismatch)");
    }
}
crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
```

對比 `WeakCrossThreadHandle::drop()` (已正確修復)：
```rust
let ptr_addr = ptr.as_ptr() as usize;
if !is_gc_box_pointer_valid(ptr_addr) {
    return;  // 只跳過 dec_weak_raw，不跳過 generation 檢查
}
let current_generation = (*ptr.as_ptr()).generation();
if current_generation != self.weak.generation() {
    return;  // Generation 不匹配時也 skip dec_weak_raw
}
let _ = GcBox::dec_weak_raw(ptr.as_ptr().cast::<GcBox<()>>());
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**TOCTOU Race Window:**

1. Line 850: `pre_generation` 被捕獲
2. Lines 852-858: Handle 從 TCB/orphan 移除
3. Lines 862-867: `is_allocated(idx)` 檢查

問題：如果 slot 在步驟 2 和 3 之間被 sweep，會發生：
- `is_allocated(idx)` 返回 false
- Early return 發生
- Generation 檢查被跳過
- `dec_ref()` 被跳過

**為什麼這可能是問題：**

雖然正常情況下，當 slot 被 sweep 時，物件應該已經是 dead (ref_count = 0)，但：
1. 如果存在 TOCTOU bug在其他路徑，ref_count 可能不正確
2. Generation 檢查本來是為了防止 slot reuse 時的 corruption
3. 跳過 generation 檢查等於跳過了 bug407 的保護

**與 WeakCrossThreadHandle::drop 的不一致：**

`WeakCrossThreadHandle::drop()` 即使在 `!is_gc_box_pointer_valid` 時也會檢查 generation，確保不匹配時 skip `dec_weak_raw`。

但 `GcHandle::drop()` 在 `!is_allocated` 時完全跳過 generation 檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的並髮條件：

1. 啟用 `lazy-sweep` feature
2. 啟用 concurrent marking
3. 創建 Gc 並通過 `downgrade()` 取得 GcHandle
4. 調用 `resolve()` 獲取 Gc（增加 ref_count）
5. 在另一執行緒觸發 concurrent sweep
6. 同時在 origin 執行緒 drop GcHandle
7. 時序：sweep 在 handle 從 roots 移除後、dec_ref 前發生

```rust
// 需要精確控制時序，理論上可能但難以穩定重現
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `GcHandle::drop()` 以在 early return 前檢查 generation：

```rust
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            // FIX: 即使 early return 也檢查 generation
            let current_generation = (*self.ptr.as_ptr()).generation();
            if pre_generation != current_generation {
                // Slot 被重用，但物件已經 dead，skip dec_ref
                return;
            }
            // Slot swept 但未重用，物件應該 dead (ref_count=0)
            // 為安全起見，仍然 skip dec_ref
            return;
        }
    }
    // Generation check 只在 is_allocated 為 true 時執行
    let current_generation = (*self.ptr.as_ptr()).generation();
    if pre_generation != current_generation {
        panic!("GcHandle::drop: slot was reused during drop (generation mismatch)");
    }
}
crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
```

或者更好的方式：總是檢查 generation，不在 `!is_allocated` path 跳過：

```rust
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            // Slot swept：物件應該 dead，skip dec_ref
            return;
        }
    }
    // 檢查 generation（只有在 is_allocated 為 true 時有意義）
    let current_generation = (*self.ptr.as_ptr()).generation();
    if pre_generation != current_generation {
        panic!("GcHandle::drop: slot was reused during drop (generation mismatch)");
    }
}
crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**

`GcHandle::drop` 的 early return 設計是為了避免在 slot 已經被 sweep 時調用 `dec_ref`。理論上，當 slot 被 sweep 時，物件應該已經是 dead (ref_count = 0)，所以 skip `dec_ref` 不會造成 leak。

但這建立在一個假設上：沒有其他路徑可以讓 ref_count 不正確。如果存在 TOCTOU bug 在其他地方，可能導致 ref_count 在 slot 被 sweep 時仍然 > 0。

Generation 檢查（bug407 fix）是為了防止 slot reuse 時の corruption。即使 `is_allocated` 為 false，generation 檢查也可以提供額外的安全保障。

**Rustacean (Soundness 觀點):**

問題在於不一致的錯誤處理。`WeakCrossThreadHandle::drop` 即使在 early return path 也會檢查 generation，但 `GcHandle::drop` 跳過這個檢查。

這不是嚴重的 soundness 問題（因為 slot sweep 時物件應該已經 dead），但可能是防禦性編程的漏洞。

**Geohot (Exploit 攻擊觀點):**

如果攻擊者可以控制 GC 時序，可能可以：
1. 讓 handle 在 slot 被 sweep 後才 drop
2. 這會導致 early return 發生
3. 如果有其他 bug 導致 ref_count 不正確，可能造成 leak 或進一步的 corruption

攻擊面相對較小，但理論上存在。

---

## 備註

此問題與 bug407 相關，但 bug407 的焦點是「remove 和 dec_ref 之間的 TOCTOU」，而本問題的焦點是「is_allocated 檢查和 dec_ref 之間的 TOCTOU」。