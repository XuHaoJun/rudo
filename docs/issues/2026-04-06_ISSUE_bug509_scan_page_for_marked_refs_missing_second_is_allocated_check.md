# [Bug]: scan_page_for_marked_refs missing second is_allocated check before push_work

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | TOCTOU race window exists between is_under_construction check and push_work |
| **Severity (嚴重程度)** | High | Could push pointer to swept slot, causing worklist corruption |
| **Reproducibility (復現難度)** | Medium | Requires concurrent lazy sweep during incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs::scan_page_for_marked_refs` (line 800-862)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
All scanning functions should have consistent defensive checks. `scan_page_for_unmarked_refs` has a second `is_allocated` check before `push_work` to defend against TOCTOU races with lazy sweep.

### 實際行為 (Actual Behavior)
`scan_page_for_marked_refs` performs only one `is_allocated` check (line 833) before the `is_under_construction` check (line 847), but lacks the second check before `push_work` (line 852). This creates a race window where lazy sweep could reclaim the slot between these checks.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `scan_page_for_marked_refs` (lines 847-853):
```rust
// Skip partially initialized objects (e.g. Gc::new_cyclic_weak); matches
// mark_object_black / mark_new_object_black (bug238, bug309).
if unsafe { (*gc_box_ptr).is_under_construction() } {
    break;
}
refs_found += 1;
if let Some(gc_box) = NonNull::new(gc_box_ptr as *mut GcBox<()>) {
    state.push_work(gc_box);  // BUG: No second is_allocated check!
}
```

Compare with `scan_page_for_unmarked_refs` (lines 1000-1013) which has the defensive check:
```rust
// Skip partially initialized objects (e.g. Gc::new_cyclic_weak); matches
// mark_object_black / mark_new_object_black (bug238, bug309).
if unsafe { (*gc_box_ptr).is_under_construction() } {
    break;
}
// Second is_allocated re-check to fix TOCTOU with lazy sweep (bug258).
// If slot was swept after is_under_construction check but before push_work,
// clear mark and skip to avoid pushing a pointer to a swept slot.
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}
if let Some(gc_box) = NonNull::new(gc_box_ptr) {
    let ptr = IncrementalMarkState::global();
    ptr.push_work(gc_box);
}
```

**Race scenario:**
1. `is_allocated(i)` returns true (line 833)
2. Generation matches (line 840)
3. `is_under_construction()` returns false (line 847)
4. **Between line 847 and 852**: Lazy sweep reclaims slot
5. `push_work()` pushes potentially invalid pointer

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check provides some protection against slot reuse, but lazy sweep can reclaim a slot without immediate reallocation, leaving the same generation. The second `is_allocated` check provides defense-in-depth.

**Rustacean (Soundness 觀點):**
Pushing a pointer to a swept slot could cause the worklist to contain invalid references, leading to undefined behavior when processed.

**Geohot (Exploit 觀點):**
Under high GC pressure with concurrent lazy sweep, this creates a reliable race to cause worklist corruption.