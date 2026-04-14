# [Bug]: try_pop_from_page Orphaned Slot Causes UB or Memory Leak

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | Race condition between concurrent sweep/allocate operations |
| **Severity (嚴重程度)** | Critical | Can cause UB (use-after-free) or memory leak |
| **Reproducibility (復現難度)** | Very Low | Requires precise thread interleaving of concurrent operations |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::try_pop_from_page`, `heap.rs:2338-2344`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0 (current)

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `try_pop_from_page` detects a corrupt slot state (slot is both in free list AND marked as allocated), it should clean up ALL state associated with that slot to prevent further issues.

### 實際行為 (Actual Behavior)
When corrupt state is detected:
1. The free list head is cleared via CAS
2. The function returns `None`
3. **The allocated_bitmap is NOT cleared**
4. The slot becomes "orphaned" - permanently marked as allocated but not on free list

This orphan slot causes problems during subsequent sweep:
- If `mark=0` (unmarked): sweep sees `is_allocated=true && mark=false` → treats as dead → calls `drop_fn` on garbage data → **UB**
- If `mark=1` (marked): sweep skips it → **permanent memory leak**

---

## 🔬 根本原因分析 (Root Cause Analysis)

Location: `crates/rudo-gc/src/heap.rs:2338-2344`

```rust
if unsafe { (*header).is_allocated(idx as usize) } {
    // Slot is allocated but in free list - corrupt. Pop it and give up on this page.
    // Do NOT read next_head from slot memory (it contains user data, not a list ptr).
    // Clear the free list head to avoid leaving corrupt state; sweep will rebuild.
    // SAFETY: Caller guarantees header is valid.
    let _ = unsafe { (*header).compare_exchange_free_list(Some(idx), None) };
    return None;  // <-- BUG: Does NOT clear allocated_bitmap
}
```

The comment says "sweep will rebuild" but this is incorrect. Sweep cannot safely reclaim orphaned slots:
- Sweep's reclaim logic (`lazy_sweep_page`, line 2640) only processes slots with `is_allocated && !is_marked`
- An orphaned slot has `is_allocated=true`, but its contents are garbage (not a valid GcBox)
- If mark=0: calling `drop_fn` on garbage is **undefined behavior**
- If mark=1: slot is skipped and **never reclaimed** (memory leak)

The root cause is a TOCTOU-style race between:
1. Sweep adding a slot to free list
2. Concurrent allocation marking slot as allocated

When this race occurs, the slot enters a corrupt state where it's simultaneously on the free list and marked as allocated.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This is a theoretical race condition - requires precise thread interleaving
// not practical to reproduce in single-threaded test

// Thread 1: Sweep is reclaiming a dead slot, about to add it to free list
// Thread 2: Concurrent allocation marks the same slot as allocated
// Result: Slot in both states - corrupt

// PoC would require:
// 1. Miri or ThreadSanitizer to detect the UB
// 2. Heavy concurrent stress testing to trigger the race
```

**Note**: Per Pattern 2 in verification guidelines, this race condition cannot be reliably reproduced in single-threaded testing. Mark as "Not Reproduced (requires TSan)".

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

At `heap.rs:2343`, after clearing the free list head, also clear the allocated bitmap:

```rust
if unsafe { (*header).is_allocated(idx as usize) } {
    let _ = unsafe { (*header).compare_exchange_free_list(Some(idx), None) };
    unsafe { (*header).clear_allocated(idx as usize) };  // <-- ADD THIS
    return None;
}
```

This ensures the slot transitions to a consistent "unallocated" state rather than being orphaned.

Alternative: Document that this is a rare fatal corruption and the appropriate response is to abort the process, since continuing with corrupted heap state is dangerous.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The corrupt state can only arise from a race between sweep adding to free list and allocation marking as allocated. This is a classic TOCTOU bug. The comment "sweep will rebuild" is misleading - sweep rebuilds the free list from properly allocated-but-dead slots, not from orphaned slots. The correct recovery is to clear all state and treat the slot as unallocated.

**Rustacean (Soundness 觀點):**
Calling `drop_fn` on a slot containing garbage (not a valid GcBox) is undefined behavior. The `drop_fn` function pointer is stored in the GcBox and expects a valid object. Reading garbage through this function pointer could read arbitrary memory, violate type safety, or cause segfaults. This is a soundness issue.

**Geohot (Exploit 觀點):**
If an attacker can influence the timing of concurrent threads, they might be able to trigger this race repeatedly. The orphaned slot with garbage data could potentially be reallocated and written with controlled data, then when the orphan is eventually swept (mark=0 path), the `drop_fn` would be called on attacker-controlled data, potentially enabling code execution via vtable or similar mechanisms.