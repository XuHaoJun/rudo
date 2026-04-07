# [Bug]: process_owned_page missing is_under_construction check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | During concurrent GC with parallel marking |
| **Severity (嚴重程度)** | `High` | Incorrect object tracing during incremental marking |
| **Reproducibility (復現難度)** | `Medium` | Requires parallel marking with objects under construction |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `process_owned_page` in `gc/marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`process_owned_page` should skip objects under construction, consistent with:
- `mark_object_black` (incremental.rs:1160-1162)
- `mark_new_object_black` (incremental.rs:1086-1092)  
- `worker_mark_loop` (marker.rs:993-997)
- `scan_page_for_marked_refs` (incremental.rs:861-865)
- `trace_and_mark_object` (incremental.rs:772-774)

### 實際行為 (Actual Behavior)

`process_owned_page` marks and pushes objects WITHOUT checking `is_under_construction`. This means objects under construction (e.g., during `Gc::new_cyclic`) could be incorrectly traced during parallel marking.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Comparison of patterns:**

**mark_object_black** (incremental.rs:1160-1162):
```rust
if gc_box.is_under_construction() {
    return None;
}
```

**mark_new_object_black** (incremental.rs:1086-1092):
```rust
if gc_box.is_under_construction() {
    return false;
}
```

**worker_mark_loop** (marker.rs:993-997):
```rust
if (*gc_box_ptr).is_under_construction() {
    break;
}
```

**scan_page_for_marked_refs** (incremental.rs:861-865):
```rust
if unsafe { (*gc_box_ptr).is_under_construction() } {
    break;
}
```

**trace_and_mark_object** (incremental.rs:772-774):
```rust
if (*gc_box.as_ptr()).is_under_construction() && state.phase() != MarkPhase::FinalMark {
    return;
}
```

**process_owned_page** (marker.rs:717-746):
```rust
match (*header).try_mark(i) {
    Ok(true) => {
        // ... is_allocated checks ...
        marked += 1;
        self.push(gc_box_ptr.as_ptr());  // NO is_under_construction check!
        break;
    }
    // ...
}
```

The `process_owned_page` function does NOT check `is_under_construction` before pushing the object to be traced. This is inconsistent with all other marking paths.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires parallel marking with objects under construction
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // Create Gc in one thread
    // Trigger parallel marking while Gc::new_cyclic is in progress
    // process_owned_page may trace object before construction complete
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_under_construction` check in `process_owned_page` after successful mark:

```rust
Ok(true) => {
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        break;
    }
    if !(*header).is_allocated(i) {
        (*header).clear_mark_atomic(i);
        break;
    }
    // FIX bug528: Skip objects under construction (e.g. Gc::new_cyclic).
    // Matches mark_object_black / mark_new_object_black (bug238).
    if unsafe { (*gc_box_ptr).is_under_construction() } {
        (*header).clear_mark_atomic(i);
        break;
    }
    marked += 1;
    self.push(gc_box_ptr.as_ptr());
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Objects under construction should be skipped by all marking paths. `Gc::new_cyclic` creates objects where the trace_fn may not be fully initialized. Missing this check in `process_owned_page` creates inconsistent behavior during parallel marking.

**Rustacean (Soundness 觀點):**
The inconsistency between `process_owned_page` and other marking paths is a code smell. All paths that call `trace_fn` should skip objects under construction to avoid tracing incomplete objects.

**Geohot (Exploit 觀點):**
If an attacker can timing the allocation of an object during parallel GC marking, they could potentially cause a partially-initialized object to be traced, leading to incorrect behavior or memory corruption.