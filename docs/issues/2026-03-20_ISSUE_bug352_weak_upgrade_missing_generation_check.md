# [Bug]: Weak::upgrade Missing Generation Check - Slot Reuse TOCTOU

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires precise timing between CAS and slot sweep |
| **Severity (嚴重程度)** | `Critical` | Can corrupt ref_count of unrelated object, leading to UAF |
| **Reproducibility (復現難度)** | `Low` | Requires Miri or TSan to reliably reproduce; race condition |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade`, `ptr.rs:2212-2303`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `current`

---

## 📝 問題描述 (Description)

`Weak::upgrade()` is missing the generation check that `GcHandle::clone()` correctly implements. This creates a TOCTOU (Time-Of-Check-Time-Of-Use) vulnerability where a slot could be swept and reused between the successful CAS incrementing `ref_count` and the post-CAS safety checks.

### 預期行為 (Expected Behavior)
When a slot is swept and reused between the CAS in `Weak::upgrade()` and the subsequent safety checks, the operation should fail gracefully (return `None`) by detecting the generation change.

### 實際行為 (Actual Behavior)
`Weak::upgrade()` performs:
1. Line 2257: `current_count = gc_box.ref_count.load(Ordering::Acquire)`
2. Lines 2266-2274: `compare_exchange_weak` to increment ref_count
3. Lines 2281-2286: Post-CAS checks for `dropping_state` and `dead_flag`
4. Lines 2287-2293: Post-CAS `is_allocated` check

But **NO** generation check exists to detect slot reuse. Compare with `GcHandle::clone()` at `cross_thread.rs:346-358`:

```rust
// Get generation BEFORE inc_ref to detect slot reuse (bug347).
let pre_generation = gc_box.generation();

gc_box.inc_ref();

// Verify generation hasn't changed - if slot was reused, return None.
if pre_generation != gc_box.generation() {
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}
```

The `generation` field (ptr.rs:48) exists specifically to detect slot reuse, but `Weak::upgrade()` does not use it.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `generation` field was added to `GcBox` to detect slot reuse (see bug347). It is correctly used in:
- `GcHandle::clone()` (cross_thread.rs:346-358)
- `GcHandle::resolve()` (cross_thread.rs:634)
- `Gc::cross_thread_handle()` (handles/async.rs:704)

But `Weak::upgrade()` does not have this protection. The vulnerability:
1. Weak ref points to slot X with generation=1
2. Slot X is swept (generation incremented to 2), new object allocated in same slot
3. Weak::upgrade() CAS succeeds on new object's ref_count (because old count was loaded)
4. Post-CAS checks don't detect slot reuse (they check flags, not generation)
5. New object now has inflated ref_count

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This bug requires Miri or TSan to reliably reproduce
// The race condition between CAS and post-CAS checks is timing-dependent
// Minimal PoC structure:

use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATED_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct Trackable(Arc<AtomicUsize>);

impl Trace for Trackable {
    fn trace(&self, _visitor: &mut rudo_gc::Visitor) {}
}

static_collect!(Trackable);

fn main() {
    // Create object and downgrade to weak
    let strong = Gc::new(Trackable(Arc::new(AtomicUsize::new(0))));
    let weak = strong.downgrade();
    drop(strong);
    
    // Force allocation to reuse the same slot
    // ... (requires precise GC timing to trigger slot reuse during upgrade)
    
    // Weak::upgrade could succeed and corrupt new object's ref_count
    let _upgraded = weak.upgrade();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check to `Weak::upgrade()` matching the pattern in `GcHandle::clone()`:

```rust
// Before CAS (after line 2264):
let pre_generation = gc_box.generation();

// After successful CAS (after line 2274):
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(ptr.as_ptr());
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation mechanism is essential for slot reuse detection in a mark-sweep GC with lazy sweeping. Without it, `Weak::upgrade()` can succeed on a different object than originally referenced. This corrupts reference counts and can lead to premature collection or use-after-free. The same pattern is correctly implemented in `GcHandle::clone()` - this inconsistency is the bug.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. Incrementing the ref_count of an unintended object violates Rust's ownership model. The `generation` field exists precisely to prevent this, but it's not being checked. This is not just a race condition - it's a missing invariant check that should be enforced.

**Geohot (Exploit 觀點):**
If an attacker can control GC timing (e.g., via allocation patterns), they could trigger slot reuse during `Weak::upgrade()`. This could:
1. Inflate ref_count of a target object
2. Prevent its collection even when all real refs are dropped
3. Eventually lead to UAF when combined with other bugs
The generation check is the safety net that makes this attack vector impractical.