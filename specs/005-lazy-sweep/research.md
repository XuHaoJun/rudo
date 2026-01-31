# Research Findings: Lazy Sweep Implementation

## Decision 1: Safepoint Trigger Frequency

**Question**: How often should lazy sweep work occur during check_safepoint()?

### Findings

**Go GC Approach**: Go's concurrent sweep uses a "fraction" based approach where a small percentage of allocations trigger sweep work. The goal is to ensure sweep keeps pace with allocation.

**Java G1 Approach**: G1 uses a "concurrent refinement" thread that processes remembered sets at a rate proportional to allocation.

**Current Proposal**: ~0.5% chance per allocation (4 pages per 800 allocations, or ~64 objects per 12800 allocations with 16 objects/page).

### Recommendation

**Decision**: Use adaptive frequency based on pending sweep count.

**Rationale**: Fixed percentage is simple but doesn't adapt to workload. If allocation rate is low but sweep work accumulates, heap can grow unbounded. Better to use a simple threshold:

- If `pending_sweep_pages > heap_page_count * 0.1` (10% of heap needs sweep), trigger sweep on every check
- Otherwise, use ~0.5% probability

This ensures sweep work is proportional to accumulated work, preventing heap growth while keeping per-allocation overhead low.

**Alternatives Considered**:
- Fixed 0.5%: Simple but can lead to heap growth under low-allocation workloads
- Every allocation: Maximum heap utilization but highest overhead (up to 16 objects per allocation)
- Per-page scanning before allocation: Complex, requires state tracking

---

## Decision 2: All-Dead Optimization Trigger

**Question**: What threshold of dead objects should trigger the "all-dead" flag?

### Findings

**Optimization Value**: The "all-dead" fast path avoids scanning individual objects when a page is entirely dead. This is valuable for pages with many short-lived objects.

**Trade-off**: Setting the threshold too low means missing opportunities; setting it too high means over-counting.

**Current Proposal**: Track dead_count, set all_dead flag when `dead_count == allocated_count`.

### Recommendation

**Decision**: Use two-level tracking:

1. **Quick path**: Track dead_count during mark phase (increment when object found dead)
2. **Set all_dead**: When `dead_count == allocated_count` after marking
3. **Clear all_dead**: When any new allocation occurs on the page

**Rationale**: This requires minimal overhead (just a counter increment during marking) and correctly identifies pages that are entirely dead. New allocations naturally clear the flag.

**For marking phase**: When iterating through objects, if an object is allocated but not marked, increment dead_count. At end of mark, if dead_count equals allocated count, set all_dead.

**Alternatives Considered**:
- Count all objects in page during marking: Too expensive, O(objects)
- Only set all_dead during sweep: Circular - we want to avoid sweeping
- Bitmap-based counting: More complex than simple counter

---

## Decision 3: Page Scanning Strategy

**Question**: What is the best approach to avoid O(N) scan when looking for pages needing sweep?

### Findings

**Current Proposal**: Simple iteration through all pages, checking needs_sweep flag.

**Problem**: With many pages, scanning all pages on every allocation that needs sweep is expensive.

**Options**:

1. **Separate list**: Maintain a list of pages needing sweep
2. **Cursor/iterator**: Remember last scanned position
3. **Per-size-class lists**: Separate sweep-pending lists per size class
4. **Bitmap**: Bitmap indicating which pages need sweep

### Recommendation

**Decision**: Use a **per-size-class doubly-linked list of sweep-pending pages**.

**Rationale**:
- Pages are already organized by size class (SIZE_CLASSES array)
- Adding pages to sweep-pending list is O(1) when marking pages
- Looking up pages needing sweep for a specific size class is O(1)
- Memory overhead: One extra pointer per page (already has free_list_head)

**Implementation**:
```text
PageHeader {
    // ... existing fields ...
    next_sweep_pending: Option<NonNull<PageHeader>>,  // Next page needing sweep in size class
    prev_sweep_pending: Option<NonNull<PageHeader>>,  // Previous page needing sweep
}
```

For MVP (Minimum Viable Product), the simple O(N) scan is acceptable. Add the list structure as an optimization when profiling shows it's needed.

**Alternatives Considered**:
- Separate list per size class: Most efficient but requires maintaining 4-8 lists
- Cursor: Simple but can lead to starvation of old pages
- Bitmap: More cache-friendly but requires separate bitmap structure

---

## Summary of Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Safepoint frequency | Adaptive (threshold + probability) | Balances heap growth prevention with allocation overhead |
| All-dead trigger | dead_count == allocated_count | Simple counter, correctly identifies entirely-dead pages |
| Page scanning | O(N) scan for MVP, lists for optimization | Simple to implement, optimize later based on profiling |

These decisions align with the rudo-gc constitution:
- **Memory Safety**: No new unsafe patterns introduced
- **Performance-First**: Optimizations can be added when needed
- **API Consistency**: Follows existing naming conventions
- **Testing**: Each decision has clear verification criteria
