# [Bug]: incremental_write_barrier large object path missing third is_allocated check (TOCTOU)

**Status:** Fixed
**Tags:** Verified

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing between gen_old read and record_in_remembered_buffer |
| **Severity (嚴重程度)** | High | Could cause remembered set corruption, leading to objects being incorrectly traced |
| **Reproducibility (重現難度)** | High | Requires concurrent lazy sweep during incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `incremental_write_barrier` in `cell.rs` (large object path)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The write barrier should verify `is_allocated` AFTER reading `has_gen_old_flag` and BEFORE calling `record_in_remembered_buffer`, similar to the small object path which has this third check (bug498 fix at cell.rs:1341).

### 實際行為 (Actual Behavior)
In the **large object path** of `incremental_write_barrier` (cell.rs:1280-1304):

```rust
// ... first and second is_allocated checks at lines 1291 and 1296 ...
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();    // <-- READ has_gen_old
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
NonNull::new_unchecked(h_ptr)                            // <-- Return header
// NO third is_allocated check!
```

Then at line 1347:
```rust
heap.record_in_remembered_buffer(header);                // <-- Record to buffer
```

### 對比：Small object path (FIXED in bug498)

The small object path was fixed (bug498) with a third check:
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Third is_allocated check AFTER has_gen_old read - prevents TOCTOU (bug498).
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
h
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**TOCTOU 視窗：**

1. Thread A executes write barrier on large object at slot S
2. Line 1296: Second `is_allocated` passes
3. **此時**：Lazy sweep executes and reclaims slot S for new object
4. Line 1300: Read `has_gen_old` from slot S (could read new object's flag)
5. Lines 1301-1303: Check `generation == 0 && !has_gen_old` - uses potentially NEW object's flag
6. Line 1304: Return `h_ptr`
7. Line 1347: `record_in_remembered_buffer(header)` - records potentially invalid slot

**後果：**
- The remembered buffer may contain entries for swept slots
- GC may attempt to trace invalid objects
- Could lead to memory corruption or incorrect marking

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `incremental_write_barrier` 的 large object path 中新增第三個 `is_allocated` 檢查，在 return 之前：

```rust
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX: Add third is_allocated check here for large object path
if !(*h_ptr).is_allocated(0) {
    return;
}
NonNull::new_unchecked(h_ptr)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
TOCTOU in write barrier can corrupt the remembered set. The third `is_allocated` check is essential to ensure slot validity before recording. The bug498 fix addressed this for small objects but missed the large object path.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. Recording invalid slots in the remembered buffer could cause GC to access deallocated memory. The large object path has the same vulnerability as the small object path had before bug498.

**Geohot (Exploit 觀點):**
If an attacker can control the timing of lazy sweep, they could potentially inject invalid entries into the remembered buffer via the large object path.
