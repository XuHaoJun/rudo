# [Bug]: Gc::downgrade() second is_allocated check leaks inc_weak

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires GC sweep between generation check and second is_allocated check |
| **Severity (嚴重程度)** | `Medium` | Weak reference count leak → objects with weak refs never collected |
| **Reproducibility (復現難度)** | `High` | Requires specific GC timing; PoC difficult |

---

## 🧩 受影響組件與環境 (Affected Component & Environment)
- **Component:** `Gc::downgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `Gc::downgrade()` fails the second `is_allocated` check at `ptr.rs:1883-1890`, it should call `dec_weak()` to undo the `inc_weak()` performed at line 1875, preventing a weak reference count leak.

### 實際行為 (Actual Behavior)
The code panics with an assertion failure but does NOT undo the `inc_weak()`, causing a weak reference count leak. The leaked weak reference prevents the object from ever being fully collected.

### Code Location
- `ptr.rs:1875` - `inc_weak()` call
- `ptr.rs:1883-1890` - Second `is_allocated` check that fails to undo `inc_weak()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is at `ptr.rs:1883-1890`:

```rust
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    // Don't call dec_weak when slot swept - it may be reused (bug133)
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::downgrade: slot was swept during downgrade"
    );
}
```

The comment "Don't call dec_weak when slot swept - it may be reused (bug133)" is **outdated and incorrect**:

1. The generation check at lines 1877-1880 already catches slot **reuse**:
   ```rust
   if pre_generation != (*gc_box_ptr).generation() {
       (*gc_box_ptr).dec_weak();
       panic!("Gc::downgrade: slot was reused...");
   }
   ```

2. If we reach the second `is_allocated` check with **generation unchanged** but `is_allocated=false`, the slot was **swept but NOT reused** - we should undo `inc_weak()` to avoid leaking.

This is the **same bug pattern as bug601**, but in `Gc::downgrade` instead of `GcBoxWeakRef::upgrade/try_upgrade`.

### Comparison with `as_weak()` (correct pattern)

The `as_weak()` function at lines 1947-1952 correctly handles this case:

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    (*ptr.as_ptr()).dec_weak();  // ✓ Correctly undoes inc_weak
    return GcBoxWeakRef::null();
}
```

`Gc::downgrade` should follow the same pattern.

### Code Flow Analysis
1. `pre_generation = gc_box.generation()` (line 1874)
2. `inc_weak()` succeeds (line 1875) - weak_count atomically incremented
3. Generation check passes - `pre_generation == gc_box.generation()` (line 1878)
4. Second `is_allocated` check **fails** - slot was swept after generation check (line 1887)
5. **Panic without undoing inc_weak** → weak reference count leak

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Creating a stable PoC is difficult due to GC timing dependencies. The issue would manifest as a memory leak when:
1. Objects with weak refs are created via `Gc::downgrade()`
2. All strong refs are dropped
3. GC runs sweep between the generation check and second is_allocated check
4. The weak ref's `upgrade()` returns None but the weak_count is leaked at 1 instead of 0

```rust
// Conceptual PoC (timing-dependent, may not reliably reproduce)
fn leak_bug_poc() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);  // inc_weak called here
    
    drop(gc);  // ref_count -> 0
    
    // If sweep runs during the narrow window in Gc::downgrade:
    // - Generation check passes
    // - Second is_allocated check fails
    // - inc_weak is NOT undone → weak_count leaked
    
    // Later:
    // weak.upgrade() returns None (expected)
    // But object may not be collected due to leaked weak_count
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Modify `ptr.rs:1883-1890` to undo `inc_weak()` before panicking:

```rust
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    // FIX bug602: Undo inc_weak if slot was swept.
    // The generation check above catches slot REUSE (where generation would change).
    // If we reach here with generation unchanged but is_allocated=false,
    // the slot was simply swept - dec_weak is safe.
    if !(*header.as_ptr()).is_allocated(idx) {
        (*gc_box_ptr).dec_weak();
        panic!("Gc::downgrade: slot was swept during downgrade");
    }
}
```

Alternatively, to match the graceful error handling of `as_weak()`:
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    (*gc_box_ptr).dec_weak();
    panic!("Gc::downgrade: slot was swept during downgrade");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The weak reference count leak in `Gc::downgrade` is a correctness issue. When `is_allocated` returns false but generation is unchanged, the slot was swept without being reused. The `dec_weak()` call is necessary to maintain accurate weak reference counts. The `as_weak()` function already has this correct pattern.

**Rustacean (Soundness 觀點):**
This is not a soundness issue (no UB), but a resource leak. The `dec_weak` function properly decrements the weak count. The existing panic indicates this scenario was considered problematic, but the missing `dec_weak` call before the panic causes the leak.

**Geohot (Exploit 觀點):**
The weak reference count leak could theoretically be weaponized for memory exhaustion DoS if an attacker can trigger the specific GC timing. However, the window for exploitation is extremely narrow (between generation check and second is_allocated check). Not a practical exploit vector.

---

## 📎 相關修復記錄 (Related Fix History)

- **bug601**: Fixed same issue in `GcBoxWeakRef::upgrade()` and `try_upgrade()` - added `undo_inc_ref` call
- **bug525**: Fixed similar issue in `try_inc_ref_from_zero` path
- **bug133**: Original reason for not calling dec_weak in this path - reasoning appears flawed/outdated after generation mechanism was added (bug347/bug354)
- **bug356**: Added generation check to detect slot reuse