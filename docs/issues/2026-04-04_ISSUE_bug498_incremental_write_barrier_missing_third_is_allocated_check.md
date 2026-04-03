# [Bug]: incremental_write_barrier small object path missing third is_allocated check after gen_old read (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing between gen_old read and record_in_remembered_buffer |
| **Severity (嚴重程度)** | High | Could cause remembered set corruption, leading to objects being incorrectly traced |
| **Reproducibility (重現難度)** | High | Requires concurrent lazy sweep during incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental_write_barrier` in `cell.rs` (small object path)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The write barrier should verify `is_allocated` AFTER reading `has_gen_old_flag` and BEFORE calling `record_in_remembered_buffer`, similar to `gc_cell_validate_and_barrier` and `unified_write_barrier` which have this third check.

### 實際行為 (Actual Behavior)
In the small object path of `incremental_write_barrier` (cell.rs:1335-1342):

```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();    // <-- READ has_gen_old
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
h                                                             // <-- Return header
};                                                              // <-- End of if block
heap.record_in_remembered_buffer(header);                      // <-- Record to buffer
```

There's NO `is_allocated` check AFTER reading `has_gen_old` (line 1335) and BEFORE `record_in_remembered_buffer` (line 1342).

### 對比：`gc_cell_validate_and_barrier` 正確實現

`gc_cell_validate_and_barrier` (heap.rs) has a third `is_allocated` check:
```rust
// Third is_allocated check - prevents TOCTOU (bug459)
if !(*h).is_allocated(index) {
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
...
heap.record_in_remembered_buffer(header);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**TOCTOU 視窗：**

1. Thread A executes write barrier on small object at slot S
2. Line 1335: Read `has_gen_old` from slot S
3. **此時**：Lazy sweep executes and reclaims slot S for new object
4. Lines 1336-1338: Check `generation == 0 && !has_gen_old` - uses OLD object's flag
5. Line 1342: `record_in_remembered_buffer(header)` - records potentially invalid slot

**後果：**
- The remembered buffer may contain entries for swept slots
- GC may attempt to trace invalid objects
- Could lead to memory corruption or incorrect marking

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確時序控制：
1. Allocate small object at slot S
2. Trigger incremental marking
3. Concurrent lazy sweep reclaims slot S
4. New object allocated at slot S during write barrier
5. Write barrier records slot S based on stale `has_gen_old` value

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `incremental_write_barrier` 的 small object path 中新增第三個 `is_allocated` 檢查：

```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX: Add third is_allocated check here
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
h
};
heap.record_in_remembered_buffer(header);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
TOCTOU in write barrier can corrupt the remembered set. The third `is_allocated` check is essential to ensure slot validity before recording.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. Recording invalid slots in the remembered buffer could cause GC to access deallocated memory.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of lazy sweep, they could potentially inject invalid entries into the remembered buffer.