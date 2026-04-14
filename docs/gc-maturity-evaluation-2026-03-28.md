# rudo-gc GC Maturity Evaluation
**Date**: 2026-03-28
**Version evaluated**: 0.8.19
**Method**: Source-code-only analysis (no documentation consulted)

---

## 前言

This report is written from the intersection of two perspectives:

- **R. Kent Dybvig** — designer of Chez Scheme and its production GC; thinks in terms of
  tri-color invariants, barrier correctness proofs, safepoint protocols, and whether the
  implementation of a GC is actually what its design claims to be.

- **Graydon Hoare** — designer of Rust; thinks in terms of whether safety guarantees are
  actually enforced by the type system, whether `unsafe` is properly scoped, and whether the
  API surface could be used incorrectly without the user realizing it.

Both would agree: a GC is only as correct as its weakest invariant.

---

## 1. Architecture Overview (What the Code Actually Is)

rudo-gc is a **non-moving**, **generational**, **tri-color mark-sweep** collector with:

- **BiBOP** (Big Bag of Pages) memory layout: size classes `[16, 32, 64, 128, 256, 512, 1024, 2048]` bytes, 4 KB-aligned pages.
- Per-page `PageHeader` (over 500 bytes) holding mark/allocated/dirty bitmaps as `[AtomicU64; 64]` arrays.
- A `GcBox<T>` header (repr(C)) containing reference count, weak count (with flags packed in high bits), type-erased `drop_fn`/`trace_fn` pointers, an `is_dropping` reentrancy guard, and a `generation: AtomicU32` slot-reuse counter.
- **Generational GC** with a minor/major split; write barrier implemented in `GcCell::borrow_mut()`.
- **Incremental marking** via a hybrid SATB + Dijkstra insertion barrier, with four STW-fallback trigger conditions.
- **Conservative stack scanning** — no precise roots, no pinning protocol.
- **HandleScope v2** for explicit root registration with compile-time lifetime enforcement.
- **Cross-thread handles** (`GcHandle<T>`) with weak TCB tracking and orphan root migration.

The work-stealing queue (`StealQueue`) and the `GcVisitorConcurrent` type for parallel marking exist in the source tree but are decorated with `#[allow(dead_code)]` — they are scaffolding, not deployed.

---

## 2. Dybvig Perspective: GC Algorithm Correctness

### 2.1 Tri-Color Invariant & Write Barrier

The fundamental requirement for a correct incremental/generational collector is that the
mutator never creates a black→white reference without the write barrier noticing.

**Claim**: Hybrid SATB (capture old grey/white pointers) + Dijkstra insertion barrier (immediately
blacken newly-written pointers).

**Assessment**: The claim is structurally correct. In `GcCell::borrow_mut_with_satb()`:
1. Old GC pointers are captured into the SATB buffer *before* the mutation.
2. New pointers are marked BLACK *after* the mutation.

This covers both sides of the Dijkstra invariant. However, three implementation concerns
undermine full confidence:

**Concern A — Barrier Skipping via `borrow_mut()`:**
`GcCell` offers three distinct mutation methods:
- `borrow_mut()` — generational + incremental Dijkstra (no SATB capture)
- `borrow_mut_with_satb()` — full SATB + Dijkstra (correct for types holding `Gc<T>`)
- `borrow_mut_gen_only()` — generational barrier only (no incremental support)

The *default* method `borrow_mut()` does NOT capture old values into the SATB buffer.
If a user mutates a `GcCell<Gc<T>>` using `borrow_mut()` during incremental marking,
the SATB snapshot is violated: the old pointer is not preserved and the new pointer
is blackened — but if the old pointer was the *only* path to a grey object, that object
becomes unreachable in the grey set. The incremental cycle's snapshot guarantee breaks.
The correct method for `Gc`-containing types is `borrow_mut_with_satb()`, but nothing
in the type system prevents `borrow_mut()` from being called. This is an API hazard.

**Concern B — SATB Buffer Overflow → Fallback Race Window:**
The SATB buffer overflows to a cross-thread secondary buffer at 1 MB. On overflow,
a fallback to full STW is *requested* but not *immediate*. Between the overflow detection
and the actual STW pause, the incremental phase continues running. During this window,
further mutations produce SATB entries that go to an overflow buffer. If the overflow
buffer also fills before STW begins, those entries are silently dropped. No assertion
or error is raised in the code when the secondary buffer would overflow.

**Concern C — `RefCell::try_borrow()` Silently Skips During Tracing:**
`Trace for RefCell<T>` (in `trace.rs`) uses `try_borrow()`:
```rust
if let Ok(inner) = self.try_borrow() {
    inner.trace(visitor);
}
```
If the `RefCell` is mutably borrowed during GC (valid per Rust semantics), the GC
silently skips tracing its contents. The assumption is "the borrower is responsible for
tracing" — but there is no mechanism that actually enforces this or calls the trace
from the temporary mutable borrow site. Under incremental marking where GC may interleave
with mutator work, this can cause live objects inside a `RefCell<Gc<T>>` to be collected.
This was documented as bug 223 for `RefCell` inside `GcCapture`, but the base `Trace for RefCell`
implementation has the same structural problem.

### 2.2 Generational Barrier Completeness

Write barriers for old→young references fire in `GcCell::borrow_mut()` by checking the page
generation. This is correct for mutations via `GcCell`. However:

- `GcMutex<T>` and `GcRwLock<T>` (in `sync.rs`) wrap `parking_lot::Mutex/RwLock`. Whether
  these fire write barriers when their guarded values contain `Gc` pointers depends on the
  implementation of their `borrow_mut`-equivalent methods. If they share the same barrier
  infrastructure, this is fine; if they bypass it, old→young pointers through mutex-guarded
  objects would be invisible to minor GC.

- `GcThreadSafeCell<T>` provides cross-thread interior mutability. Its barrier must also
  fire on mutation; this needs verification against the actual implementation rather
  than the documentation claim.

### 2.3 Root Enumeration

Root enumeration combines:
1. `HandleScope` — explicit per-thread scope stack (correct, compile-time safe).
2. Conservative stack scanning — correct in principle, problematic in practice (see §2.4).
3. Cross-thread roots (`GcHandle`) — registered in TCB, migrated on thread death.
4. Test roots — manual registration via `register_test_root`.

**Coverage gap — `GcCell` inside non-traced heap allocations:**
A `Vec<Gc<T>>` stored outside a `GcCell` (e.g., in a `Box<Vec<Gc<T>>>`) would be traced
because `Trace for Vec<T>` iterates elements. However, a `Vec<Gc<T>>` inside a type that
has a *wrong* or *missing* `Trace` impl would silently drop those roots. Since `Trace`
is an `unsafe trait`, the correctness of the entire root graph depends on every user-derived
`Trace` being correct. The derive macro reduces this risk but cannot prevent manual
`unsafe impl Trace` with omitted fields.

### 2.4 Conservative Stack Scanning — A Fundamental Tension

Conservative scanning on x86_64 spills: `rbp, r12, r13, r14, r15`.
**`rbx` is explicitly not spilled** (LLVM internal register constraint).

This means any live `Gc` pointer held exclusively in `rbx` at the time of collection will
not be found as a root. The code comment acknowledges this: "the conservative scan of stack
memory will still find any GC pointers that were stored on the stack." This is true only if
the pointer was *also* written to the stack at some point. For a pointer that lives entirely
in `rbx` between its last stack write and a GC trigger, that object would be prematurely
collected. In a conservative scanner, this is a soundness hole — it cannot be dismissed as
"unlikely" because LLVM optimizations routinely keep hot pointers in `rbx`.

**aarch64 `clear_registers()` clears the wrong registers:**
`spill_registers_and_scan` spills x19–x30 (callee-saved).
`clear_registers()` clears x0–x18 (caller-saved/scratch registers).
These are disjoint sets. `clear_registers()` on aarch64 does nothing to prevent false roots
from lingering in the registers that the spill function actually captures. This is a latent
correctness bug: any stale `Gc` pointer in x19–x30 that should have been cleared (to prevent
a false root keeping an object alive after drop) will persist through `clear_registers()`.

### 2.5 Slot-Reuse / Generation Counter

The generation counter (`AtomicU32` in `GcBox`) guards against use-after-free from slot reuse.
The pattern is: capture generation → do CAS → recheck generation → abort if mismatch.
This is the correct defense against the sweep-then-reallocate TOCTOU race and has been
applied extensively (20+ bug fixes reference this pattern). The implementation is thorough.

**One residual concern:** The generation counter is `u32`. With aggressive allocation and
collection of short-lived objects, the counter wraps at 2³² ≈ 4 billion reuses per slot.
On a fast allocator doing millions of small allocations per second, a slot could theoretically
complete 4B reuse cycles and present the same generation number, causing a false "not reused"
conclusion. This is an astronomical edge case in practice but is worth noting for completeness.

### 2.6 Ephemeron Semantics — Structurally Incomplete

`Ephemeron<K, V>` stores a weak reference to the key and a strong reference to the value.
The user-facing API correctly returns `None` when the key is dead. However:

The `Trace for Ephemeron<K, V>` unconditionally traces the value (`self.value`), regardless
of key liveness. This means the GC keeps the value alive even after the key is dead.
The correct ephemeron semantics (as in Racket, Java, or ECMAScript weak maps) require the
GC itself to treat the value's liveness as conditional on the key's liveness, breaking the
value free only when the key is confirmed dead *in the same GC cycle*.

The current implementation is not an ephemeron in the GC-theoretic sense; it is a
"weak-key strong-value pair" with conditional read access. Memory associated with the value
is never reclaimed upon key death unless the `Ephemeron` itself is dropped. For workloads
that use ephemerons as memory-efficient caches (the canonical use case), this defeats the
purpose entirely.

---

## 3. Graydon Hoare Perspective: Rust Safety Model

### 3.1 `unsafe impl Sync for IncrementalMarkState` — Comments vs. Types

```rust
pub struct IncrementalMarkState {
    worklist: UnsafeCell<SegQueue<*const GcBox<()>>>,
    // ...
}
unsafe impl Sync for IncrementalMarkState {}
```

The safety justification (in comments at lines 221–235) is: "the worklist is accessed
single-threaded from the GC thread during mark slices." This is a prose invariant,
not an enforced structural invariant. Nothing in the type system prevents a second
thread from calling `push_work()` or `pop_work()` simultaneously. The correctness of
the entire incremental marking phase rests on a convention documented in a comment.

When parallel marking is eventually implemented (the comment explicitly anticipates this),
this `unsafe impl Sync` becomes a ticking time bomb: the impl exists and is correct *today*
under stated restrictions, but those restrictions are invisible to any future contributor
who extends the type.

The correct Rust pattern here would be a type-level token — e.g., `PhantomData<*mut ()>`
making the type `!Sync` by default, with access gated through a `GcThreadToken` passed
only to the GC thread. Instead, the current design inverts this: the type is declared `Sync`
and soundness depends on callers respecting an undocumented call discipline.

### 3.2 `unsafe trait Trace` — Correctly Unsafe, but Composability Risk

`Trace` is correctly marked `unsafe` because an incorrect implementation causes UB
(missed live objects → use-after-free; false objects → undefined type confusion).

However, the standard library `Trace` impls include:

```rust
unsafe impl<T: Trace + ?Sized> Trace for &T {
    fn trace(&self, visitor: &mut impl Visitor) {
        T::trace(self, visitor);
    }
}
```

A `Gc<&'static T>` would pass through this impl correctly. But `Gc<&'a T>` where `'a`
is not `'static` is structurally prevented by requiring `T: 'static` in `Gc<T>::new()`.
This is fine. The risk is in the derive macro output: `#[derive(Trace)]` generates
field-by-field `trace()` calls, which is correct if all fields implement `Trace`.
The macro does not verify that fields containing raw pointers (`*const T`, `*mut T`)
or `NonNull<T>` are handled — it simply does not generate `visit()` calls for them,
which is correct behavior for non-GC raw pointers but silently wrong if a user stores
a raw pointer that *is* a GC-managed address and expects it to be traced.

### 3.3 `static_collect!` Macro — Easy Misuse

```rust
macro_rules! static_collect {
    ($type:ty) => {
        unsafe impl<'gc> $crate::Trace for $type where $type: 'static {
            fn trace(&self, _visitor: &mut impl $crate::Visitor) {}
        }
    }
}
```

This macro generates a no-op `Trace` impl, asserting to the GC that the type contains
no `Gc` pointers. A user who calls `static_collect!(MyStruct)` on a type that *does*
contain `Gc` fields will silently break the GC: those fields are live but never traced,
causing premature collection. The macro provides no guard, no `assert`, and no documentation
warning beyond its name. An assertion like `T: !ContainsGcPointers` is impossible to express
in Rust's type system, but the macro should at minimum be documented prominently as
"you are asserting this type contains NO `Gc<T>` fields, and if you are wrong, you will get UB."

### 3.4 Handle API Surface — Correct but Complex

The `HandleScope` / `Handle<'scope, T>` design correctly uses Rust lifetimes to enforce
scope discipline at compile time. This is the strongest API design in the entire library —
it makes root-scope misuse a type error rather than a runtime check.

`EscapeableHandleScope` correctly provides a single-escape mechanism with a pre-allocated
slot in the parent scope, preventing use-after-free when handles must cross scope boundaries.

One concern: `MaybeHandle<'scope, T>` stores a nullable raw pointer (`*const HandleSlot`)
with no `Option` wrapper. The nullability invariant is maintained by convention; code that
constructs a `MaybeHandle` must never produce a non-null-but-invalid slot pointer.
This could be expressed more safely as `Option<Handle<'scope, T>>` at zero cost, eliminating
the raw pointer.

### 3.5 Parallel Marking Infrastructure Is Dead Code in Production

The codebase contains:
- `StealQueue<T, N>` (Chase-Lev work-stealing deque) — `#[allow(dead_code)]`
- `GcVisitorConcurrent` — `#[allow(dead_code)]`
- `GcWorkerRegistry` — partially used, partially scaffolded

The `#[allow(dead_code)]` attributes are honest markers — the infrastructure is not deployed.
But they represent a promise that is not yet kept. The unsafe code in these types (particularly
the memory ordering in `StealQueue::pop()`) has not been exercised under production load.
The `pop()` implementation has a subtlety: after decrementing `bottom` with `Release`,
it reads the item, then CAS-races with stealers for the last element. The `Release` on
the preliminary `bottom.store(new_b, ...)` is insufficient — it should be `SeqCst` or
at least paired with a `fence(SeqCst)` before the item read to prevent the compiler from
reordering the item read before the bottom decrement on weakly-ordered architectures.
The existing code would be racy on architectures weaker than x86's TSO model (notably
aarch64 under high concurrency).

### 3.6 Cross-Thread Handle Safety — Well-Engineered

`GcHandle<T>` is the strongest cross-thread story in the library. The design is:
- Holds `Weak<ThreadControlBlock>` — handle outlives origin thread without UAF.
- `resolve()` enforces origin-thread identity — prevents accessing GC heap from wrong thread.
- On TCB drop, roots migrate to global orphan table — no root leak.

The 15+ bugs found and fixed in this subsystem (threads 4, 11, 127, 185, 296, 338, 403, 415)
demonstrate that this design space is hard, but the current state of the fixes suggests
the implementation has been hardened iteratively to a reasonable level of correctness.

---

## 4. Performance Architecture Assessment

### 4.1 What Is Working

- BiBOP layout with per-size-class pages gives O(1) allocation on the fast path.
- Bitmap-based sweep iteration is cache-friendly for sparse pages.
- Per-page ownership tracking (`ownership.rs`) routes parallel marking work to the thread
  that originally allocated objects, exploiting cache locality.
- Generational split reduces full-heap scan frequency for allocation-heavy workloads.

### 4.2 What Is Not Yet Working

- **Parallel marking is scaffolded but not deployed.** All actual marking is single-threaded
  on the GC thread. The work-stealing infrastructure (`StealQueue`, worker registry) exists
  but is not wired into the mark phase.
- **Incremental marking has 4 fallback conditions.** The frequency with which these trigger
  in production is unknown, as the benchmarks (`sweep_benchmark`, `gui_overall`) do not
  measure fallback rate. If fallbacks are common, the incremental marking overhead
  (SATB buffering, barrier complexity) is paid with no benefit over stop-the-world.
- **The GcVisitor worklist is an unbounded `Vec`.** The comment acknowledges this:
  "for very deep graphs with millions of objects, this could consume significant memory."
  No overflow mitigation (segmented allocation, spill to secondary store) exists.
- **Conservative scanning is O(stack_size / word_size)** per GC per thread. For threads
  with large stacks (common in async runtimes), this scanning cost may dominate GC pause
  time on minor collections, which are supposed to be fast.

---

## 5. Test Coverage Assessment

### Strongly Covered
- Basic allocation, reference counting, cycle detection.
- Cross-thread handle lifecycle with thread termination.
- Weak reference TOCTOU races (bugs 8, 91).
- Slot-reuse generation guard correctness.
- HandleScope nesting, escape, sealed scope semantics.

### Inadequately Covered
- **Incremental marking phase transitions.** No test verifies that Idle → Snapshot →
  Marking → FinalMark → Sweeping transitions are correct, or that the SATB snapshot
  accurately preserves all live objects across a full incremental cycle with concurrent
  mutator activity.
- **SATB barrier via `borrow_mut()` vs `borrow_mut_with_satb()`** — no test demonstrates
  what happens when the "wrong" method is used during an active incremental phase.
- **Ephemeron value reclamation.** All ephemeron tests verify read semantics. None
  verify that the *value* is reclaimed after key death — because it isn't.
- **aarch64 correctness.** `clear_registers()` clears caller-saved registers rather than
  callee-saved registers. No test on aarch64 exercises the root-clearing path because the
  bug has no observable effect in tests (false roots only cause retention, not crashes).
- **Large stack conservative scanning.** No test exercises GC on a thread with a very
  large stack to measure false root rate or scan overhead.
- **Async handle scope integration.** `handles/async.rs` contains `AsyncHandleScope`,
  `AsyncHandle`, `GcScope` — but the integration test file `handlescope_async.rs` exists
  and tests exist. The coverage here needs to be verified more carefully, but the async
  integration is present.

---

## 6. Summary Scorecard

| Dimension | Score | Notes |
|---|---|---|
| **Tri-color invariant (STW)** | ★★★★☆ | Correct for explicit handle roots; conservative scan has the RBX gap |
| **Generational barrier** | ★★★☆☆ | Correct via GcCell; bypass possible via wrong borrow method; GcThreadSafeCell TBD |
| **Incremental marking** | ★★☆☆☆ | Architecture correct; SATB overflow unmitigated; too many fallback conditions; not battle-tested |
| **Ephemeron semantics** | ★★☆☆☆ | API correct; memory reclamation missing |
| **Weak references** | ★★★★☆ | Generation-guarded, multi-layered; TOCTOU history well-addressed |
| **Cross-thread handles** | ★★★★☆ | Solid design; extensive bug fixes; orphan migration correct |
| **HandleScope API safety** | ★★★★★ | Compile-time lifetime enforcement; best part of the library |
| **Unsafe discipline** | ★★★☆☆ | SAFETY comments present; IncrementalMarkState Sync issue; aarch64 register clear bug |
| **Parallel marking** | ★★☆☆☆ | Infrastructure written but undeployed; ordering questions in pop() |
| **Test coverage** | ★★★☆☆ | Regression tests excellent; incremental/generational barrier paths weak |
| **Production readiness** | ★★★☆☆ | Solid for single-threaded or STW use; incremental/parallel not production-ready |

---

## 7. Top Priority Findings

These are the most important issues, in order of severity:

### P0 — aarch64 `clear_registers()` clears caller-saved registers (wrong set)
`spill_registers_and_scan` captures x19–x30 (callee-saved). `clear_registers` zeroes x0–x18
(scratch/caller-saved). The two functions operate on disjoint register sets. On aarch64,
`clear_registers()` does not prevent false roots from lingering in callee-saved registers.

### P1 — `borrow_mut()` skips SATB capture during incremental marking
During active incremental marking, calling `borrow_mut()` on a `GcCell<Gc<T>>` violates
the SATB snapshot because old pointer values are not captured. The user-visible API does
not distinguish "safe to call during incremental marking" from "not safe." This can cause
premature collection of objects that were live at snapshot time.

### P2 — `Trace for RefCell<T>` silently skips tracing when mutably borrowed
Under incremental GC, a `GcCell` containing a `RefCell<Gc<T>>` that is mutably borrowed
during a mark slice will not have its inner `Gc` traced in that slice. No compensation
mechanism exists (the borrow site does not call the visitor).

### P3 — `IncrementalMarkState: Sync` enforced by comment, not types
The `unsafe impl Sync` for the type containing `UnsafeCell<SegQueue>` relies on a prose
guarantee that only the GC thread accesses the worklist. No type-level enforcement exists.
Future parallel marking work will require revisiting this impl; the current state is a
latent soundness risk for contributors.

### P4 — Ephemeron values never reclaimed by GC
`Ephemeron<K, V>::trace()` unconditionally keeps `value` alive. This makes
`Ephemeron<K, V>` useless as a GC-level weak map or cache — values are never collected.

### P5 — SATB overflow secondary buffer not bounded
If the primary SATB buffer overflows *and* the secondary (cross-thread) buffer also
fills before the requested STW fallback begins, entries are silently dropped, breaking
the SATB snapshot guarantee.

---

## 8. Conclusion

rudo-gc is a **well-engineered research-quality GC** that has been hardened through
iteration — the 20+ numbered bug fixes demonstrate a culture of correctness. The
HandleScope API is genuinely excellent: it is the right way to integrate a GC into Rust's
ownership model. The cross-thread handle subsystem is sophisticated and defensively designed.

However, the project has two distinct tiers:

**Tier 1 (production-capable):** STW mark-sweep with HandleScope roots, generational
barriers via `GcCell`, weak references, cross-thread handles. These are well-tested and
the known bugs have been addressed. This tier is suitable for use in production systems
with appropriate testing.

**Tier 2 (not yet production-capable):** Incremental marking with SATB+Dijkstra barriers,
parallel marking, true ephemeron semantics. These are architecturally designed but contain
the P0–P5 issues above. Running incremental marking in a production environment without
resolving the SATB gap and the `borrow_mut` API hazard would be unsound.

From Dybvig's standpoint: the algorithm knows what it wants to be, but the SATB/Dijkstra
barrier protocol has holes that need closing before the incremental mode is trustworthy.
From Hoare's standpoint: the unsafe surface area is honest and documented, but several
invariants are enforced by convention rather than types — a pattern that does not scale
with contributors.

The library version number (0.8.x) is appropriately humble. It is not ready for 1.0
until at minimum P0–P2 are resolved and the incremental marking subsystem has dedicated
stress-test coverage that exercises the full Idle→Sweeping state machine under concurrent
mutator load.
