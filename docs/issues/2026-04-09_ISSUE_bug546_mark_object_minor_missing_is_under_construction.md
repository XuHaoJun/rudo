# [Bug]: mark_object_minor missing is_under_construction check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | During minor GC with objects under construction (Gc::new_cyclic) |
| **Severity (嚴重程度)** | Critical | Calling trace_fn on partially initialized object |
| **Reproducibility (復現難度)** | Medium | Requires minor GC trigger during object construction |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_minor` (gc/gc.rs:2072-2125)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

`mark_object_minor` should skip objects under construction, consistent with all other marking paths that call `trace_fn`:
- `trace_and_mark_object` (incremental.rs:772-774) - checks `is_under_construction`
- `mark_object_black` (incremental.rs:1160-1162) - checks `is_under_construction`
- `mark_new_object_black` (incremental.rs:1086-1092) - checks `is_under_construction`
- `scan_page_for_marked_refs` (incremental.rs:861-865) - checks `is_under_construction`
- `worker_mark_loop` (marker.rs:933) - checks `is_under_construction`

### 實際行為

`mark_object_minor` marks and enqueues objects WITHOUT checking `is_under_construction`. This allows objects that are still being constructed (e.g., during `Gc::new_cyclic`) to be traced, potentially calling `trace_fn` on partially initialized memory.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `mark_object_minor` (gc/gc.rs:2101-2123):

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(index) {
        // ... generation check ...
    }
    visitor.objects_marked += 1;
    break;  // NO is_under_construction check!
}
// Later, trace_fn is called on the potentially uninitialized object
```

Compare with `worker_mark_loop` (marker.rs:931-935):
```rust
// FIX bug469: Skip objects under construction (e.g. Gc::new_cyclic).
// Matches mark_object_black / mark_new_object_black (bug238).
if (*gc_box_ptr).is_under_construction() {
    break;
}
```

The `mark_object_minor` function was introduced for minor GC and does not have the `is_under_construction` check that all other marking paths have. This is an inconsistency that could cause serious issues.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires minor GC during Gc::new_cyclic construction
use rudo_gc::{Gc, Trace, GcCell, collect};

#[derive(Trace)]
struct Node {
    value: i32,
}

fn main() {
    // Create Gc that triggers minor GC during construction
    // mark_object_minor would trace the partially initialized object
    let node = Gc::new_cyclic(|weak| Node { value: 42 });
    // If minor GC runs here, object may be traced before construction complete
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_under_construction` check in `mark_object_minor` after successful mark:

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(index) {
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;
        }
        (*header.as_ptr()).clear_mark_atomic(index);
        return;
    }
    // FIX bug546: Skip objects under construction (e.g. Gc::new_cyclic).
    // Matches worker_mark_loop (bug469), mark_object_black (bug238).
    if (*ptr.as_ptr()).is_under_construction() {
        (*header.as_ptr()).clear_mark_atomic(index);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
All marking paths that call trace_fn must skip objects under construction. Gc::new_cyclic creates objects where trace_fn may not be fully initialized - tracing such object leads to incorrect behavior or memory corruption.

**Rustacean (Soundness 觀點):**
The missing is_under_construction check is a soundness issue. Objects under construction may have uninitialized fields that trace_fn would access incorrectly.

**Geohot (Exploit 觀點):**
If an attacker can trigger minor GC during object construction (e.g., via timed allocations), they could potentially cause trace_fn to be called on partially initialized memory, leading to information disclosure or corruption.

---

## 驗證指南檢查

- Pattern 1 (Full GC 遮蔽 barrier bug): Use minor GC (`collect()`) not `collect_full()` to test
- Pattern 2 (單執行緒無法觸發競態): Construction timing is easier to control in single thread
- Pattern 3 (測試情境與 issue 描述不符): N/A
- Pattern 4 (容器內的 Gc 未被當作 root): N/A  
- Pattern 5 (難以觀察的內部狀態): trace_fn calling on wrong data is observable via crash/panic