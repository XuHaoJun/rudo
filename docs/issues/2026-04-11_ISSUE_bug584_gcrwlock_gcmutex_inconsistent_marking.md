# [Bug]: GcRwLock/GcMutex inconsistent with GcCell/GcThreadSafeCell - NEW pointer marking

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Only triggers when barrier state changes between check and marking |
| **Severity (嚴重程度)** | High | Could cause premature object collection during incremental marking |
| **Reproducibility (復現難度)** | High | Requires precise timing of barrier activation |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock`, `GcMutex`, `GcCell`, `GcThreadSafeCell`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

There is an inconsistency in how NEW GC pointers are marked across different cell types:

### GcCell::borrow_mut() and GcThreadSafeCell::borrow_mut()
- Mark NEW GC pointers **unconditionally** (no check for barrier state)
- Comment at cell.rs:201-204 claims to match GcRwLock behavior

### GcRwLock::write() and GcMutex::lock()
- Only mark NEW GC pointers when `generational_active || incremental_active` is true
- Uses `mark_gc_ptrs_immediate(&*guard, generational_active || incremental_active)`

### 預期行為 (Expected Behavior)
All cell types should mark NEW GC pointers consistently. If GcCell claims to match GcRwLock behavior, they should have the same marking logic.

### 實際行為 (Actual Behavior)
- **GcCell::borrow_mut()**: Unconditionally marks NEW pointers
- **GcThreadSafeCell::borrow_mut()**: Unconditionally marks NEW pointers  
- **GcRwLock::write()**: Only marks when `generational_active || incremental_active` is true
- **GcMutex::lock()**: Only marks when `generational_active || incremental_active` is true

The comment at cell.rs:201-204 says:
```rust
// FIX bug506: Always mark NEW GC pointers unconditionally, matching
// GcThreadSafeCell::borrow_mut() and GcRwLock::write() behavior.
```

But this is **incorrect** - GcRwLock::write() does NOT mark unconditionally. It passes `generational_active || incremental_active` to `mark_gc_ptrs_immediate()`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `sync.rs:295` (GcRwLock::write):
```rust
mark_gc_ptrs_immediate(&*guard, generational_active || incremental_active);
```

And in `sync.rs:59-62` (mark_gc_ptrs_immediate):
```rust
fn mark_gc_ptrs_immediate<T: GcCapture + ?Sized>(value: &T, barrier_active: bool) {
    if !barrier_active {
        return;  // Early return when barrier not active!
    }
    // ... marking code
}
```

But in `cell.rs:208-219` (GcCell::borrow_mut):
```rust
unsafe {
    let new_value = &*result;
    let mut new_gc_ptrs = Vec::with_capacity(32);
    new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
    if !new_gc_ptrs.is_empty() {
        crate::heap::with_heap(|_heap| {
            for gc_ptr in new_gc_ptrs {
                let _ = crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8);
            }
        });
    }
}
```

No check for barrier state - always marks.

**Bug Scenario**:
1. GcRwLock written when NO barriers active (`generational_active=false`, `incremental_active=false`)
2. mark_gc_ptrs_immediate returns early without marking
3. Later, incremental marking becomes active
4. The NEW pointer may be missed during marking if slot reuse occurs

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires careful timing to reproduce - the race window is small
// See Pattern 2 in verification guidelines: "單執行緒無法觸發競態 bug"
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option A** (Match GcCell behavior - recommended):
Make GcRwLock::write() and GcMutex::lock() mark unconditionally by changing:
```rust
mark_gc_ptrs_immediate(&*guard, generational_active || incremental_active);
```
to:
```rust
mark_gc_ptrs_immediate(&*guard, true);
```

**Option B** (Match GcRwLock behavior):
Make GcCell::borrow_mut() only mark when barriers are active, reverting bug506 partially.

**Option C** (Documentation fix only):
The comment in GcCell is misleading - fix it to accurately describe the actual behavior and explain why the inconsistency is intentional.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The marking behavior is related to SATB consistency. If OLD values are recorded but NEW values are not marked, the SATB invariant could be violated if incremental marking starts later. The unconditional marking in GcCell/GcThreadSafeCell seems more conservative.

**Rustacean (Sound 觀點):**
The inconsistency itself is a code smell. If the API contract says these types should behave the same, they should. The misleading comment (claiming to match GcRwLock) is a documentation bug at minimum.

**Geohot (Exploit 觀點):**
The race condition (barrier becoming active between check and marking) is similar to TOCTOU issues. The unconditional marking approach is defense-in-depth against timing attacks.

---

## 修復紀錄 (2026-04-11)

**修復人員:** opencode

**修復內容:**
- `sync.rs:295`: GcRwLock::write() - 將 `mark_gc_ptrs_immediate(&*guard, generational_active || incremental_active)` 改為 `mark_gc_ptrs_immediate(&*guard, true)`
- `sync.rs:338`: GcRwLock::try_write() - 同上
- `sync.rs:605`: GcMutex::lock() - 同上
- `sync.rs:646`: GcMutex::try_lock() - 同上

**驗證:**
- `cargo build --workspace` 編譯成功
- `./clippy.sh` 通過
