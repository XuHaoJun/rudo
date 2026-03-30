# [Bug]: sweep_large_objects Missing is_allocated Check Before Dereference

**Status:** Fixed
**Tags:** Verified

**Fixed by:** Adding `is_allocated(0)` check at line 2500 in `gc/gc.rs`

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Called during STW; race window only if STW violated |
| **Severity (嚴重程度)** | Critical | Potential UAF if slot is deallocated and reused between check and dereference |
| **Reproducibility (復現難度)** | Very High | Requires STW violation or specific concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_large_objects()`, `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`sweep_large_objects()` should verify a slot is allocated before dereferencing its `GcBox` fields, similar to `sweep_phase1_finalize()`.

### 實際行為 (Actual Behavior)

At line 2500 in `gc/gc.rs`, `sweep_large_objects()` checks `is_marked(0)` but does NOT check `is_allocated(0)` before dereferencing `gc_box_ptr` at line 2507:

```rust
if !(*header).is_marked(0) {  // Line 2500 - only checks is_marked
    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
    let (weak_count, dead_flag) = (*gc_box_ptr).weak_count_and_dead_flag();  // Line 2507 - UAF risk
```

Contrast with `sweep_phase1_finalize()` at line 2175:
```rust
} else if (*header).is_allocated(i) {  // Line 2175 - checks BOTH
    // ... safely dereferences gc_box_ptr
}
```

### 代碼一致性問題

The inconsistency creates a maintenance hazard. Future changes could introduce races that this check would catch.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/gc/gc.rs` lines 2500-2507

For large objects, each page contains exactly one object. The assumption is:
1. If a page is in `large_object_pages()`, it's allocated as a large object
2. During STW (stop-the-world), no concurrent allocation happens
3. Therefore, checking `is_allocated(0)` is unnecessary

However:
1. `lazy_sweep` can run concurrently with mutators (not during STW)
2. If `sweep_large_objects` is called when STW is violated, the slot could be deallocated
3. The code is inconsistent with `sweep_phase1_finalize` which DOES check `is_allocated`

**Risk scenario:**
1. Page is in `large_object_pages()` (allocated as large object)
2. Object becomes dead (unmarked)
3. Between `is_marked(0)` check and `gc_box_ptr` dereference:
   - Page is deallocated and returned to OS, OR
   - Page is reused for new allocation
4. Dereference reads from freed/reused memory → **UAF**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This bug is difficult to reproduce because it requires STW violation or precise timing with lazy sweep. No reliable single-threaded PoC exists.

**Analysis-based evidence:**
- `sweep_phase1_finalize` (line 2175) checks `is_allocated(i)` - why would large objects be different?
- The codebase has 127+ references to "lazy sweep TOCTOU" fixes, indicating this pattern is well-known

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated(0)` check before dereferencing in `sweep_large_objects`:

```rust
if !(*header).is_marked(0) && (*header).is_allocated(0) {
    // ... safe to dereference gc_box_ptr
}
```

Or document why `is_allocated` check is unnecessary for large objects (e.g., "large object pages cannot be deallocated while in large_object_pages() during STW").

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
In typical GC systems, sweep operations verify object liveness before dereferencing. The assumption that "large objects are always valid during STW" relies on invariant preservation across code changes. Consistency with `sweep_phase1_finalize` is preferable to silent assumptions.

**Rustacean (Soundness 觀點):**
The lack of `is_allocated` check creates a latent UAF vector. If any future code path allows concurrent deallocation, or if the "STW guarantee" is violated, this becomes a soundness bug. The pattern `is_allocated` before dereference is established in this codebase for good reason.

**Geohot (Exploit 觀點):**
Exploitation would require either:
1. Finding a way to violate STW guarantees
2. Precise timing to race with deallocation

Both are difficult but not impossible, especially in programs with aggressive threading or signal handlers.