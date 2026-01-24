# rudo-gc Closure Tracing Technical Plan
## 閉包捕獲 Gc<T> 的正確追蹤策略

**Author:** R. Kent Dybvig (Parallel Universe Consulting)  
**Date:** 2026-01-24  
**Status:** Technical Proposal  
**Context:** [MVP-Review-3](/docs/mvp-review-3.md) P0 Technical Blocker

---

## 🔴 Problem Statement

### The Fatal Trace Hole

```rust
// effect.rs - Current Implementation
unsafe impl Trace for Effect {
    fn trace(&self, visitor: &mut impl rudo_gc::Visitor) {
        // Closure is not GC-managed, so we don't trace it <- CRITICAL BUG!
        self.owner.trace(visitor);
    }
}

pub struct Effect {
    closure: Box<dyn Fn() + 'static>,  // ← Opaque, untraced
    // ...
}
```

**Failure Mode:**
```rust
let shared = Gc::new(MyData::new());  // ref_count = 1
let captured = Gc::clone(&shared);    // ref_count = 2

let effect = create_effect(move || {
    captured.get();  // captured lives inside closure
});

drop(shared);  // ref_count = 1 (captured in closure)
collect();     // GC cannot see `captured` → marks it dead → BOOM! 💥
```

### Why This Is Fundamentally Hard

1. **Type Erasure**: `Box<dyn Fn()>` erases all type information about captured values
2. **No Reflection**: Rust has no runtime mechanism to enumerate closure captures
3. **Lifetime Constraints**: `'static` requirement on closures prevents compile-time assistance
4. **Opaque Memory Layout**: Closure struct layout is compiler-generated and unknowable

---

## 📐 Solution Design Space

### Option 1: Conservative Stack Scanning (Current Approach - **Insufficient**)

`rudo-gc` already implements conservative stack scanning via `spill_registers_and_scan()`:

```rust
// stack.rs
pub unsafe fn spill_registers_and_scan<F>(mut scan_fn: F)
where F: FnMut(usize, usize, bool)
{
    // Spill callee-saved registers to stack
    // Scan stack for potential pointers
}
```

**Why It Fails for Closures:**
- ✅ Works for stack-allocated `Gc<T>` locals
- ❌ Closures stored in `Box<dyn Fn()>` live on the **heap**, not the stack
- ❌ Conservative scanning only covers stack memory and registers
- ❌ No way to know which heap regions contain captured `Gc<T>` pointers

### Option 2: Explicit Capture Registration (cppgc Style)

Force users to explicitly register captured `Gc<T>` values:

```rust
// Proposed API
fn create_effect_with_deps<F, D>(deps: D, closure: F) -> Gc<Effect>
where
    F: Fn() + 'static,
    D: TraceDeps + 'static,
{
    // Store deps alongside closure, trace both
}

// Usage
let shared = Gc::new(MyData::new());
let _effect = create_effect_with_deps(
    deps![shared.clone()],  // Explicit registration
    move || { shared.get(); }
);
```

**Evaluation:**
- ✅ Precise, no false negatives
- ✅ Zero runtime overhead for scanning
- ❌ Ergonomic nightmare - duplicates every capture
- ❌ Easy to forget, leading to subtle bugs
- ❌ Violates DX goals ("feels like JavaScript")

### Option 3: TraceClosure Wrapper Type

Provide a wrapper that makes closures traceable:

```rust
// Proposed TraceClosure<C, D>
pub struct TraceClosure<C, D>
where
    C: Fn() + 'static,
    D: Trace + Clone + 'static,
{
    closure: C,
    deps: D,
}

impl<C, D> Trace for TraceClosure<C, D> 
where C: Fn() + 'static, D: Trace + Clone + 'static 
{
    fn trace(&self, visitor: &mut impl Visitor) {
        self.deps.trace(visitor);
    }
}
```

**Evaluation:**
- ✅ Type-safe dependency tracking
- ✅ Can be used ergonomically with tuples: `TraceClosure::new((a, b), move || ...)`
- ❌ Still requires explicit dep declaration
- ❌ Closure cannot move out of deps (needs Clone)

### Option 4: Heap Conservative Scanning (V8/Oilpan Style) ⭐ RECOMMENDED

Extend conservative scanning to cover heap-allocated closures:

```rust
// Enhanced heap scanning
pub struct HeapScanner {
    gc_page_ranges: Vec<(usize, usize)>,  // Known GC page boundaries
}

impl HeapScanner {
    /// Scan a memory region conservatively for Gc pointers
    pub unsafe fn scan_region(&self, start: *const u8, len: usize, visitor: &mut impl Visitor) {
        let end = start.add(len);
        let mut current = start as usize;
        
        while current + size_of::<usize>() <= end as usize {
            let potential_ptr = *(current as *const usize);
            
            // Check if this looks like a pointer into our GC pages
            if self.is_gc_pointer(potential_ptr) {
                if let Some(gc_box) = find_gc_box_from_ptr(potential_ptr as *const u8) {
                    mark_object(gc_box, visitor);
                }
            }
            
            current += align_of::<usize>();
        }
    }
    
    fn is_gc_pointer(&self, addr: usize) -> bool {
        // Fast check: Address Space Coloring
        // GC pages are allocated at HEAP_HINT_ADDRESS (0x6000_0000_0000)
        if addr < HEAP_HINT_ADDRESS || addr >= HEAP_HINT_ADDRESS + HEAP_SIZE_LIMIT {
            return false;
        }
        
        // Refined check: actual page lookup
        self.gc_page_ranges.iter().any(|(start, end)| addr >= *start && addr < *end)
    }
}
```

**Key Insight:** `Box<dyn Fn()>` allocates closure data on the regular heap (via system allocator). We can:
1. Record the memory region used by each closure `Box`
2. Scan that region conservatively during GC

**Implementation for Effect:**

```rust
pub struct Effect {
    closure: Box<dyn Fn() + 'static>,
    closure_ptr: *const (),   // Start of closure's heap allocation
    closure_size: usize,      // Size of closure's captured environment
    // ... other fields
}

unsafe impl Trace for Effect {
    fn trace(&self, visitor: &mut impl rudo_gc::Visitor) {
        // Trace known Gc fields
        self.owner.trace(visitor);
        
        // Conservative scan the closure's captured environment
        if !self.closure_ptr.is_null() && self.closure_size > 0 {
            unsafe {
                HEAP_SCANNER.scan_region(
                    self.closure_ptr as *const u8,
                    self.closure_size,
                    visitor
                );
            }
        }
    }
}
```

**Evaluation:**
- ✅ Zero API changes for users
- ✅ Automatic - captures are traced without explicit registration
- ✅ Same technique used by V8/cppgc for tracing C++ closures
- ⚠️ May have false positives (integers that look like pointers)
- ⚠️ Requires tracking closure allocation metadata

### Option 5: Procedural Macro for Traceable Closures ⭐ ALTERNATIVE

Use a proc-macro to transform closures at compile time:

```rust
// Usage
let shared = Gc::new(MyData::new());
let effect = create_effect!(move || {
    shared.get();
});

// Expands to:
let shared = Gc::new(MyData::new());
let effect = {
    let __captures = (Gc::clone(&shared),);  // Detected captures
    Effect::new_with_captures(__captures, move || {
        shared.get();
    })
};
```

**Implementation Sketch:**

```rust
// Closure analysis (similar to rvue's ReactiveDetector)
struct ClosureCaptureAnalyzer {
    gc_captures: Vec<Ident>,
}

impl ClosureCaptureAnalyzer {
    fn analyze(closure: &ExprClosure) -> Vec<Ident> {
        // Walk the AST, find all identifiers that:
        // 1. Are used inside the closure
        // 2. Are not defined inside the closure
        // 3. Have Gc<_> type (heuristic: ends with .clone() before use, or explicit annotation)
    }
}
```

**Evaluation:**
- ✅ Compile-time safety - no runtime overhead
- ✅ Ergonomic - just use `create_effect!()` instead of `create_effect()`
- ✅ Precise - knows exactly which captures are `Gc<T>`
- ⚠️ Requires macro usage (not transparent)
- ⚠️ Complex macro implementation
- ❌ Cannot detect `Gc<T>` inside nested structs captured by closure

---

## 🏗️ Recommended Implementation: Hybrid Approach

Combine **Option 4 (Heap Scanning)** with **Option 5 (Macro)** for defense in depth:

### Phase 1: Heap Conservative Scanning (Immediate Fix)

**Changes to `rudo-gc`:**

1. **Add closure metadata tracking:**

```rust
// ptr.rs - Extended GcBox
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: Cell<NonZeroUsize>,
    weak_count: Cell<usize>,
    pub(crate) drop_fn: unsafe fn(*mut u8),
    pub(crate) trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    
    // NEW: Optional heap regions to conservatively scan
    pub(crate) extra_scan_regions: Cell<Option<NonNull<ExtraScanRegions>>>,
    
    value: T,
}

pub struct ExtraScanRegions {
    regions: Vec<(*const u8, usize)>,  // (ptr, len) pairs
}
```

2. **Add heap region scanner:**

```rust
// heap.rs - Conservative heap scanning
pub unsafe fn scan_heap_region_conservatively(
    region_ptr: *const u8,
    region_len: usize,
    visitor: &mut GcVisitor,
) {
    let heap = HEAP.with(|h| &*h.tcb.heap.get());
    
    let mut current = region_ptr as usize;
    let end = current + region_len;
    
    while current + size_of::<usize>() <= end {
        let potential_ptr = *(current as *const usize);
        
        // Try to find a GcBox at this address
        if let Some(gc_box) = find_gc_box_from_ptr(heap, potential_ptr as *const u8) {
            crate::gc::mark_object(gc_box, visitor);
        }
        
        current += align_of::<usize>();
    }
}
```

3. **Extend trace_fn to scan extra regions:**

```rust
// trace.rs
impl Visitor for GcVisitor {
    fn visit<T: Trace + ?Sized>(&mut self, gc: &Gc<T>) {
        let ptr = gc.raw_ptr();
        if let Some(nn) = ptr.as_option() {
            let gc_box = unsafe { nn.as_ref() };
            
            // Mark the object
            // ... existing marking logic ...
            
            // Scan any registered extra regions
            if let Some(regions) = gc_box.extra_scan_regions.get() {
                unsafe {
                    for (region_ptr, region_len) in (*regions.as_ptr()).regions.iter() {
                        scan_heap_region_conservatively(*region_ptr, *region_len, self);
                    }
                }
            }
        }
    }
}
```

### Phase 2: API Extension for rvue Effect

**Changes to `rvue`:**

```rust
// effect.rs - Updated Effect with tracing support
pub struct Effect {
    closure: Box<dyn Fn() + 'static>,
    closure_layout: std::alloc::Layout,  // For conservative scanning
    is_dirty: AtomicBool,
    is_running: AtomicBool,
    owner: GcCell<Option<Gc<Component>>>,
    cleanups: GcCell<Vec<Box<dyn FnOnce() + 'static>>>,
}

impl Effect {
    pub fn new<F>(closure: F) -> Gc<Self>
    where
        F: Fn() + 'static,
    {
        // Capture closure layout before boxing
        let closure_layout = std::alloc::Layout::for_value(&closure);
        
        let owner = crate::runtime::current_owner();
        let effect = Gc::new(Self {
            closure: Box::new(closure),
            closure_layout,
            is_dirty: AtomicBool::new(true),
            is_running: AtomicBool::new(false),
            owner: GcCell::new(owner),
            cleanups: GcCell::new(Vec::new()),
        });
        
        // Register the closure's heap region for conservative scanning
        // SAFETY: Box<dyn Fn()> points to heap memory of size closure_layout.size()
        unsafe {
            let closure_ptr: *const () = std::mem::transmute(&*effect.closure);
            rudo_gc::register_scan_region(
                &effect,
                closure_ptr as *const u8,
                closure_layout.size()
            );
        }
        
        effect
    }
}

unsafe impl Trace for Effect {
    fn trace(&self, visitor: &mut impl rudo_gc::Visitor) {
        // Owner is traced normally
        self.owner.trace(visitor);
        
        // Closure is traced via registered scan regions (handled by GC core)
        // Cleanups are also closures - need same treatment
        // For now, cleanups run before GC can occur, so they're safe
    }
}
```

### Phase 3: Optional Macro Enhancement

For maximum safety, provide an explicit-deps variant:

```rust
// effect.rs
pub fn create_effect_with<D, F>(deps: D, closure: F) -> Gc<Effect>
where
    D: Trace + Clone + 'static,
    F: Fn() + 'static,
{
    let deps_cell = GcCell::new(deps);
    let effect = Effect::new(move || {
        let _ = &deps_cell;  // Ensure deps_cell is captured
        closure();
    });
    effect
}

// Usage (optional, for extra safety):
create_effect_with((signal1.clone(), signal2.clone()), move || {
    // Even if conservative scan misses something, deps are traced
});
```

---

## 📊 Implementation Complexity Analysis

| Phase | Effort | Risk | Benefit |
|-------|--------|------|---------|
| Phase 1a: Heap scanner in rudo-gc | Medium (3-5 days) | Low | Fixes core bug |
| Phase 1b: Scan region registration API | Low (1-2 days) | Low | Clean API |
| Phase 2: Effect integration | Low (1-2 days) | Low | Complete fix for rvue |
| Phase 3: Macro enhancement | High (5-7 days) | Medium | Belt-and-suspenders safety |

**Recommended Rollout:** Phase 1 + Phase 2 first (1 week), Phase 3 deferred.

---

## 🔬 Testing Strategy

### Unit Tests for rudo-gc

```rust
#[test]
fn test_heap_conservative_scan_finds_captured_gc() {
    let obj = Gc::new(TestObject::new());
    let obj_ptr = Gc::internal_ptr(&obj);
    
    // Create closure that captures obj
    let closure: Box<dyn Fn()> = Box::new({
        let captured = obj.clone();
        move || { captured.data(); }
    });
    
    // Drop original reference - only closure holds it
    drop(obj);
    
    // Force GC - should NOT collect obj because closure holds it
    collect_full();
    
    // Verify object is still alive
    assert!(!is_freed(obj_ptr));
}
```

### Integration Tests for rvue

```rust
#[test]
fn test_effect_preserves_captured_gc() {
    let signal = create_signal(0);
    let counter = Gc::new(GcCell::new(0));
    let counter_clone = counter.clone();
    
    let _effect = create_effect(move || {
        counter_clone.borrow_mut().set(signal.get());
    });
    
    // Drop our reference - effect should keep counter alive
    drop(counter);
    
    // Force GC
    collect_full();
    
    // Update signal - effect should run without crash
    signal.set(42);  // Would crash if counter was incorrectly collected
}
```

---

## 🚨 Known Limitations

1. **False Positives**: Conservative scanning may keep objects alive longer than necessary if random integer values happen to look like GC pointers. Mitigation: Address Space Coloring makes this extremely unlikely.

2. **Closure Size Calculation**: `Layout::for_value(&closure)` gives the size before boxing. The actual heap allocation may be larger due to alignment. Mitigation: Round up to next alignment boundary.

3. **Nested Closures**: Closures inside closures require recursive scanning. The current design handles this automatically since each closure's region is scanned conservatively.

4. **dyn Trait Fat Pointers**: `Box<dyn Fn()>` is a fat pointer (data ptr + vtable ptr). We scan the data region, not the vtable.

---

## 📚 References

- [cppgc Conservative Scanning](https://chromium.googlesource.com/v8/v8/+/main/include/cppgc/internal/gc-info.h)
- [Oilpan Design Doc](https://chromium.googlesource.com/chromium/src/+/master/third_party/blink/renderer/platform/heap/HeapGC.md)
- [Boehm GC Conservative Pointer Finding](https://www.hboehm.info/gc/gcdescr.html)
- [rudo-gc BiBOP Layout](../learn-projects/rudo/crates/rudo-gc/src/heap.rs)

---

## ✅ Action Items

1. **[P0]** Implement `scan_heap_region_conservatively()` in `rudo-gc/src/heap.rs`
2. **[P0]** Add `ExtraScanRegions` support to `GcBox`
3. **[P0]** Add `register_scan_region()` public API
4. **[P1]** Update `Effect::new()` to register closure memory region
5. **[P1]** Add test suite for closure capture scenarios
6. **[P2]** Document the limitation and recommended usage patterns
7. **[P2]** Consider adding `create_effect_with()` explicit-deps variant
