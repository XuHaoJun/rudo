# Code Review: `rudo-gc` Crate

**Date:** 2026-01-03  
**Reviewer:** AI Assistant (based on John McCarthy's GC principles and Chez Scheme analysis)  
**Scope:** `crates/rudo-gc` implementation review with comparison to Chez Scheme GC

---

## Executive Summary

The `rudo-gc` crate implements a garbage-collected smart pointer (`Gc<T>`) for Rust, inspired by Chez Scheme's BiBOP (Big Bag of Pages) memory layout and Mark-Sweep algorithm. The implementation successfully adapts several key principles from Chez Scheme while respecting Rust's ownership model and address stability requirements.

**Overall Assessment: Solid Foundation with Room for Optimization**

The crate demonstrates good engineering decisions:
- ✅ BiBOP memory layout for O(1) pointer-to-page lookup
- ✅ Non-moving GC preserving Rust's `&T` address stability
- ✅ Generational collection with write barriers
- ✅ Conservative stack scanning for root discovery
- ✅ Large Object Space (LOS) for objects > 2KB

---

## 1. Architecture Comparison: `rudo-gc` vs Chez Scheme

### 1.1 Memory Layout (BiBOP)

| Aspect | Chez Scheme | `rudo-gc` | Assessment |
|--------|-------------|-----------|------------|
| Page Size | Variable (segments) | Fixed 4KB | ✅ Good for Rust's alignment needs |
| Size Classes | Type-based (pairs, vectors, etc.) | Size-based (16, 32, 64, ..., 2048) | ✅ Appropriate for Rust's diverse types |
| Page Header | Segment metadata | `PageHeader` struct (80 bytes) | ✅ Includes mark/dirty bitmaps |
| Large Objects | Separate handling | Dedicated `large_objects` vector | ✅ Consistent with Chez |

**Code Reference (`heap.rs:45-66`):**
```rust
#[repr(C)]
pub struct PageHeader {
    pub magic: u32,           // Validation
    pub block_size: u16,      // Size class
    pub obj_count: u16,       // Max objects
    pub generation: u8,       // For generational GC
    pub flags: u8,            // is_large_object, etc.
    pub mark_bitmap: [u64; 4],  // 256 bits
    pub dirty_bitmap: [u64; 4], // Write barrier support
    pub free_list_head: Option<u16>,
}
```

**Comparison to Chez Scheme (`gc.c:73-106`):**
Chez Scheme uses `marked_mask` and `use_marks` flags per segment, similar to `rudo-gc`'s `mark_bitmap`. However, Chez has a more complex segment structure supporting both copying and marking modes.

### 1.2 Collection Algorithms

| Aspect | Chez Scheme | `rudo-gc` | Assessment |
|--------|-------------|-----------|------------|
| Young Gen | Copying (Semi-space) | Mark-Sweep with promotion | ⚠️ Simplified but functional |
| Old Gen | Mark-Sweep OR Copying | Mark-Sweep only | ✅ Correct for Rust's non-moving requirement |
| Parallelism | Full parallel sweeping | Single-threaded | ⚠️ Future enhancement opportunity |

**Key Insight:** Chez Scheme can switch between copying and marking modes (`use_marks` flag). `rudo-gc` correctly chose Mark-Sweep only, as Rust's `&T` references require address stability.

### 1.3 Write Barriers

Both systems use **Dirty Cards/Bitmaps** for generational GC:

**`rudo-gc` (`cell.rs:77-95`):**
```rust
fn write_barrier(&self) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();
    unsafe {
        let header = ptr_to_page_header(ptr);
        if (*header).generation > 0 {
            if let Some(index) = ptr_to_object_index(ptr) {
                (*header).set_dirty(index);
            }
        }
    }
}
```

**Chez Scheme (`gc.c:76-78`):**
> "Uses **Dirty Cards**. When a mutation occurs (e.g., `set-car!`), the system marks the corresponding segment's 'card' as dirty."

**Assessment:** ✅ The implementation is semantically equivalent. `GcCell<T>` properly triggers write barriers on mutation.

---

## 2. Implementation Review

### 2.1 Strengths

#### A. Conservative Stack Scanning (`stack.rs`)

The implementation correctly spills registers and scans the stack for potential pointers:

```rust
#[cfg(all(target_arch = "x86_64", not(miri)))]
unsafe {
    std::arch::asm!(
        "mov {0}, rbx",
        "mov {1}, rbp",
        // ... callee-saved registers
    );
}
```

This is essential since Rust/LLVM doesn't provide stack maps. The BiBOP layout enables O(1) validation of potential pointers:

```rust
pub unsafe fn find_gc_box_from_ptr(heap: &GlobalHeap, ptr: usize) -> Option<NonNull<GcBox<()>>> {
    if !heap.is_in_range(ptr) { return None; }  // Fast range check
    let page_addr = ptr & PAGE_MASK;             // O(1) page lookup
    let header_ptr = page_addr as *mut PageHeader;
    if (*header_ptr).magic != MAGIC_GC_PAGE { return None; }
    // ... interior pointer handling
}
```

#### B. Generational Collection

The implementation properly distinguishes Minor and Major collections:

```rust
fn collect() {
    if total_size > MAJOR_THRESHOLD {
        collect_major(heap);
    } else if young_size > MINOR_THRESHOLD {
        collect_minor(heap);
    } else {
        collect_minor(heap);  // Default to minor for low latency
    }
}
```

**Minor Collection Flow:**
1. `mark_minor_roots()` - Stack + Dirty old objects
2. `sweep_young_pages()` - Reclaim unmarked young objects
3. `promote_young_pages()` - Survivors become old

#### C. Type-Erased Tracing

The `trace_fn` stored in `GcBox` enables polymorphic tracing without dynamic dispatch overhead during marking:

```rust
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: Cell<NonZeroUsize>,
    pub(crate) drop_fn: unsafe fn(*mut u8),
    pub(crate) trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    value: T,
}
```

This mirrors Chez Scheme's approach where object type information is encoded in the segment/page structure.

### 2.2 Areas for Improvement

#### A. Bump Allocation Optimization

**Current (`heap.rs:237-296`):** The segment uses bump pointer with free-list fallback.

**Suggestion:** Consider Thread-Local Allocation Buffers (TLAB) as discussed in the Gemini analysis document. This would reduce contention in future multi-threaded extensions:

```rust
// Proposed enhancement
thread_local! {
    static TLAB: Cell<Option<NonNull<PageHeader>>> = Cell::new(None);
}
```

#### B. Parallel Marking (Future)

**Chez Scheme (`gc.c:122-194`):**
> "Parallel mode runs `sweep_generation` concurrently in multiple sweeper threads... uses Work Stealing Deque."

The current single-threaded implementation is appropriate for v0.1, but the architecture should plan for:
- Atomic mark bitmap operations (already using `[u64; 4]`, easy to make atomic)
- Work-stealing queue for parallel tracing

#### C. Interior Pointer Handling

**Current Limitation (`heap.rs:807-810`):**
```rust
// Large object handling: only accept the exact start for now
if header.flags & 0x01 != 0 && offset != 0 {
    return None;
}
```

For large objects, interior pointers are rejected. This is conservative but may cause false negatives in stack scanning.

**Recommendation:** Implement a look-aside table for large objects as described in the Gemini document's "McCarthy's analysis" section.

---

## 3. Comparison with Chez Scheme GC Internals

### 3.1 Generation Management

**Chez Scheme:**
```c
// gc.c:312-320
/* max_cg: maximum copied generation
 * min_tg: minimum target generation
 * max_tg: maximum target generation
 * Objects in generation g are collected into generation
 * MIN(max_tg, MAX(min_tg, g+1)).
 */
```

**`rudo-gc`:**
- Simpler two-generation model (Young=0, Old>0)
- Promotion on survival during Minor GC

**Assessment:** The simplified model is appropriate for a Rust library. Chez Scheme's multi-generational approach is designed for long-running Scheme processes with complex tenuring requirements.

### 3.2 Object Relocation

**Chez Scheme (`gc.c:84-93`):**
> "If an object is copied, then its first word is set to `forward_marker` and its second word is set to the new address."

**`rudo-gc`:** Does NOT relocate objects. This is the correct design choice for Rust compatibility but means:
- No compaction (potential fragmentation in BiBOP mitigates this)
- No semi-space copying for young gen

### 3.3 Ephemerons and Guardians

**Chez Scheme (`gc.c:110-120`):**
Supports complex finalization ordering via ephemerons and guardians.

**`rudo-gc`:**
Uses Rust's standard `Drop` trait. Consider adding:
- `Weak<T>` equivalent for GC
- Finalization ordering guarantees

---

## 4. Code Quality Assessment

### 4.1 Safety

| Aspect | Status | Notes |
|--------|--------|-------|
| `unsafe` blocks | ⚠️ Many | Inherent to GC implementation |
| Bounds checking | ✅ | Consistent index validation |
| Magic number validation | ✅ | `MAGIC_GC_PAGE = 0x5255_4447` |
| Miri compatibility | ✅ | `#[cfg(miri)]` fallbacks in `stack.rs` |

### 4.2 Documentation

- ✅ Comprehensive module-level docs
- ✅ Function-level safety requirements
- ⚠️ Missing inline comments in complex GC logic

### 4.3 Testing

The test coverage includes:
- Basic allocation/deallocation
- Minor/Major collection
- Write barrier verification
- Cycle detection

**Suggestion:** Add stress tests for:
- Large object allocation/collection
- Deep object graphs
- Interior pointer scanning validation

---

## 5. Recommendations

### Short-term (v0.1.x)
1. **Add `Weak<T>` support** - Essential for observer patterns
2. **Improve interior pointer handling** for large objects
3. **Add collection metrics** - Expose timing and byte counts

### Medium-term (v0.2.x)
4. **TLAB implementation** - Thread-local bump allocation
5. **Incremental marking** - Reduce worst-case pause times
6. **`#[derive(Trace)]` macro** - Already planned per `lib.rs:84-85`

### Long-term (v1.0)
7. **Parallel sweeping** - Following Chez Scheme's parallel model
8. **Segment reclamation** - Return empty pages to OS
9. **Cross-thread `Gc<T>`** - Similar to V8's shared heap

---

## 6. Conclusion

The `rudo-gc` crate successfully adapts Chez Scheme's BiBOP principles to Rust's unique constraints. The non-moving Mark-Sweep approach respects Rust's address stability guarantees while providing cycle detection—something `Rc<T>` cannot offer.

**Key Achievements:**
- Elegant BiBOP integration with Rust's type system
- Correct generational collection with write barriers
- Conservative stack scanning without compiler modifications

**Main Trade-offs (vs Chez Scheme):**
- No object relocation → Potential fragmentation (mitigated by BiBOP)
- Single-threaded → Future scalability concern
- Two generations → Less tuning flexibility

The implementation demonstrates a deep understanding of both GC theory and Rust's memory model. It is a solid foundation for production use, with clear paths for future optimization.

---

*"Both are worthy successors to the `reclaim()` function I wrote on an IBM 704."*
— John McCarthy (from the Gemini analysis document)
