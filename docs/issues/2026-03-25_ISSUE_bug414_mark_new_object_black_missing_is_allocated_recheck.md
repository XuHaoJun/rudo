# [Bug]: mark_new_object_black returns true without re-checking is_allocated when generations mismatch

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires incremental marking + lazy sweep + slot reuse timing |
| **Severity (嚴重程度)** | Medium | Could leave incorrect mark state, affecting GC correctness |
| **Reproducibility (復現難度)** | Medium | Needs careful timing of mark + sweep + reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_new_object_black` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`mark_new_object_black` should verify that the slot is still allocated after detecting a generation mismatch before returning `true`. Compare with `mark_object_black` which properly re-checks `is_allocated` after the generation check.

### 實際行為 (Actual Behavior)

When `current_generation != marked_generation` (indicating slot reuse), the function returns `true` without re-checking `is_allocated`:

```rust
// Line 1077-1086
let marked_generation = (*gc_box).generation();
(*header.as_ptr()).set_mark(idx);
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return false;
}
let current_generation = (*gc_box).generation();
if current_generation != marked_generation {
    return true;  // BUG: No is_allocated re-check here!
}
return true;
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**檔案:** `crates/rudo-gc/src/gc/incremental.rs:1076-1088`

The issue is in `mark_new_object_black` - after detecting a generation mismatch (slot was reused), the function returns `true` without verifying the slot is still valid.

**Compare with `mark_object_black`** (lines 1144-1167):
```rust
Ok(true) => {
    let marked_generation = (*gc_box).generation();
    if (*h).is_allocated(idx) {
        let current_generation = (*gc_box).generation();
        if current_generation != marked_generation {
            return None;  // Proper handling
        }
        return Some(idx);
    }
    // Slot was swept - check generation to distinguish swept from reused
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        return None;
    }
    (*h).clear_mark_atomic(idx);
    return None;
}
```

The `mark_object_black` function re-checks `is_allocated` after detecting generation mismatch and handles swept-vs-reused cases properly.

**Problem scenario:**
1. Object A allocated at generation G1, `mark_new_object_black` called
2. `marked_generation` captured as G1
3. `set_mark` succeeds on slot
4. `is_allocated` check passes (slot still valid)
5. Lazy sweep runs, slot becomes free
6. BEFORE generation check: new object B allocated in slot with generation G2
7. `current_generation` = G2 ≠ G1
8. Function returns `true` WITHOUT verifying slot is still allocated
9. But object B was allocated AFTER our mark - did B get marked correctly?

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical race requiring precise timing:
// 1. Create Gc<A> during incremental marking
// 2. Call mark_new_object_black - captures generation G1, sets mark
// 3. Slot passes is_allocated check
// 4. Lazy sweep runs, slot enters free list
// 5. NEW object B allocated in slot before generation check
// 6. Generation check: G2 != G1, returns true
// 7. Result: mark belongs to B, but we returned true for A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_allocated` re-check after generation mismatch:

```rust
if current_generation != marked_generation {
    // FIX: Re-check is_allocated to distinguish swept+reused from just-reused
    if !(*header.as_ptr()).is_allocated(idx) {
        // Slot was swept after set_mark - our mark is invalid
        return false;
    }
    return true;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Incremental marking relies on accurate mark state
- If mark_new_object_black incorrectly returns true, the mark state could be inconsistent
- The generational GC relies on generation to track slot lifetime

**Rustacean (Soundness 觀點):**
- Code inconsistency with mark_object_black which handles this case properly
- The generation check is incomplete without verifying slot validity

**Geohot (Exploit 觀點):**
- Unclear mark state could potentially be exploited
- Precise timing required but could affect GC correctness under stress

---

## Related Issues

- bug358: Similar mark_new_object_black generation check issue
- bug412: mark_new_object_black missing generation check (mentioned fix not merged)
- mark_object_black: Proper pattern for generation + is_allocated check
