# API Contracts: Rust Dumpster GC

**Feature Branch**: `001-rust-dumpster-gc`  
**Date**: 2026-01-02

## Overview

This document defines the public API contracts (traits, types, and functions) for the `rudo-gc` crate.

---

## Public Traits

### Trace

The core trait that all garbage-collected types must implement.

```rust
/// A type that can be traced by the garbage collector.
///
/// # Safety
///
/// Implementations MUST correctly report all `Gc<T>` fields by calling
/// `visitor.visit()` on each one. Failure to do so will result in
/// use-after-free or memory leaks.
///
/// Prefer using `#[derive(Trace)]` instead of manual implementation.
pub unsafe trait Trace {
    /// Visit all `Gc` pointers contained within this value.
    ///
    /// The visitor will be called with each `Gc<T>` field. The implementation
    /// must visit ALL Gc fields, including those inside nested structs, enums,
    /// and collections.
    fn trace(&self, visitor: &mut impl Visitor);
}
```

### Visitor

The trait for GC operations that traverse the object graph.

```rust
/// A visitor that is called during garbage collection to traverse the object graph.
///
/// Users generally do not need to implement this trait. It is used internally
/// by the garbage collector.
pub trait Visitor {
    /// Visit a garbage-collected pointer.
    ///
    /// Called by `Trace::trace()` for each `Gc` field in an object.
    fn visit<T: Trace + ?Sized>(&mut self, gc: &Gc<T>);
}
```

---

## Public Types

### Gc<T>

The primary garbage-collected pointer type.

```rust
/// A garbage-collected pointer to a value of type `T`.
///
/// `Gc<T>` provides shared ownership of a value, similar to `Rc<T>`, but with
/// automatic cycle detection and collection.
///
/// # Thread Safety
///
/// `Gc<T>` is `!Send` and `!Sync`. It can only be used within a single thread.
/// For thread-safe garbage collection, use `sync::Gc<T>` (future work).
///
/// # Panics
///
/// Dereferencing a "dead" `Gc` (one whose value has been collected during
/// a Drop implementation) will panic. Use `Gc::try_deref()` for fallible access.
pub struct Gc<T: Trace + ?Sized> {
    // ... internal fields ...
}

impl<T: Trace + ?Sized> Gc<T> {
    /// Create a new garbage-collected value.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::Gc;
    ///
    /// let x = Gc::new(42);
    /// assert_eq!(*x, 42);
    /// ```
    pub fn new(value: T) -> Gc<T>
    where
        T: Sized;

    /// Create a self-referential garbage-collected value.
    ///
    /// The closure receives a "dead" `Gc` that will be rehydrated after
    /// construction completes.
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, Trace};
    ///
    /// #[derive(Trace)]
    /// struct Node {
    ///     self_ref: Gc<Node>,
    /// }
    ///
    /// let node = Gc::new_cyclic(|this| Node { self_ref: this });
    /// ```
    pub fn new_cyclic<F: FnOnce(Gc<T>) -> T>(data_fn: F) -> Gc<T>
    where
        T: Sized;

    /// Attempt to dereference this Gc.
    ///
    /// Returns `None` if this Gc is "dead" (only possible during Drop of cycles).
    pub fn try_deref(gc: &Gc<T>) -> Option<&T>;

    /// Attempt to clone this Gc.
    ///
    /// Returns `None` if this Gc is "dead".
    pub fn try_clone(gc: &Gc<T>) -> Option<Gc<T>>;

    /// Returns a raw pointer to the data.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn as_ptr(gc: &Gc<T>) -> *const T;

    /// Check if two Gcs point to the same allocation.
    pub fn ptr_eq(this: &Gc<T>, other: &Gc<T>) -> bool;

    /// Get the current reference count.
    ///
    /// # Panics
    ///
    /// Panics if the Gc is dead.
    pub fn ref_count(gc: &Gc<T>) -> NonZeroUsize;

    /// Check if this Gc is "dead" (refers to a collected value).
    pub fn is_dead(gc: &Gc<T>) -> bool;
}

impl<T: Trace + ?Sized> Deref for Gc<T> {
    type Target = T;
    fn deref(&self) -> &T;
}

impl<T: Trace + ?Sized> Clone for Gc<T> {
    fn clone(&self) -> Gc<T>;
}

impl<T: Trace + ?Sized> Drop for Gc<T> {
    fn drop(&mut self);
}

// Gc is NOT Send or Sync
impl<T: Trace + ?Sized> !Send for Gc<T> {}
impl<T: Trace + ?Sized> !Sync for Gc<T> {}
```

### CollectInfo

Information provided to collection condition functions.

```rust
/// Statistics about the current heap state, used to determine when to collect.
pub struct CollectInfo {
    // ... internal fields ...
}

impl CollectInfo {
    /// Number of Gc pointers dropped since last collection.
    pub fn n_gcs_dropped_since_last_collect(&self) -> usize;

    /// Number of Gc pointers currently existing.
    pub fn n_gcs_existing(&self) -> usize;
}
```

---

## Public Functions

### collect

```rust
/// Force an immediate garbage collection.
///
/// This function runs the mark-sweep collector synchronously, freeing all
/// unreachable allocations. Normally, collection is triggered automatically
/// based on the collect condition, but this function can be used to ensure
/// deterministic cleanup.
///
/// # Examples
///
/// ```
/// use rudo_gc::{Gc, collect};
/// use std::sync::Mutex;
///
/// static GUARD: Mutex<()> = Mutex::new(());
///
/// let gc = Gc::new(GUARD.lock().unwrap());
/// drop(gc);
/// collect(); // Ensure the guard is dropped before re-locking
/// let _ = GUARD.lock().unwrap();
/// ```
pub fn collect();
```

### set_collect_condition

```rust
/// Set the function that determines when automatic collection occurs.
///
/// The function is called periodically and should return `true` when
/// a collection should be triggered.
///
/// # Examples
///
/// ```
/// use rudo_gc::{set_collect_condition, CollectInfo};
///
/// // Never collect automatically
/// set_collect_condition(|_| false);
///
/// // Always collect on drop
/// set_collect_condition(|_| true);
/// ```
pub fn set_collect_condition(f: fn(&CollectInfo) -> bool);
```

### default_collect_condition

```rust
/// The default collection condition.
///
/// Returns `true` when `n_gcs_dropped > n_gcs_existing`, ensuring
/// amortized O(1) collection overhead.
pub fn default_collect_condition(info: &CollectInfo) -> bool;
```

---

## Derive Macro

### #[derive(Trace)]

```rust
/// Derive the `Trace` trait for a struct or enum.
///
/// All fields that contain `Gc<T>` (directly or indirectly) will be
/// automatically traced.
///
/// # Examples
///
/// ```
/// use rudo_gc::{Gc, Trace};
///
/// #[derive(Trace)]
/// struct Node {
///     value: i32,           // Primitive, no tracing needed
///     left: Option<Gc<Node>>,  // Traced
///     right: Option<Gc<Node>>, // Traced
/// }
/// ```
///
/// # Supported Types
///
/// - Structs with named fields
/// - Structs with tuple fields
/// - Enums with any variant type
/// - Generic types (with `T: Trace` bounds added automatically)
#[proc_macro_derive(Trace)]
pub fn derive_trace(input: TokenStream) -> TokenStream;
```

---

## Blanket Implementations

The following types have `Trace` implemented automatically:

```rust
// Primitives (no-op trace)
unsafe impl Trace for i8 { ... }
unsafe impl Trace for i16 { ... }
unsafe impl Trace for i32 { ... }
unsafe impl Trace for i64 { ... }
unsafe impl Trace for i128 { ... }
unsafe impl Trace for isize { ... }
unsafe impl Trace for u8 { ... }
unsafe impl Trace for u16 { ... }
unsafe impl Trace for u32 { ... }
unsafe impl Trace for u64 { ... }
unsafe impl Trace for u128 { ... }
unsafe impl Trace for usize { ... }
unsafe impl Trace for f32 { ... }
unsafe impl Trace for f64 { ... }
unsafe impl Trace for bool { ... }
unsafe impl Trace for char { ... }
unsafe impl Trace for () { ... }
unsafe impl Trace for String { ... }
unsafe impl<T: Trace> Trace for &T { ... }
unsafe impl<T: Trace + ?Sized> Trace for Box<T> { ... }
unsafe impl<T: Trace> Trace for Vec<T> { ... }
unsafe impl<T: Trace> Trace for Option<T> { ... }
unsafe impl<T: Trace, E: Trace> Trace for Result<T, E> { ... }
unsafe impl<T: Trace> Trace for RefCell<T> { ... }
unsafe impl<T: Trace> Trace for Cell<T> where T: Copy { ... }
// ... and more standard library types
```

---

## Error Handling

| Situation | Behavior |
|-----------|----------|
| Deref dead Gc | Panic with descriptive message |
| Clone dead Gc | Panic with descriptive message |
| Incorrect Trace impl | Undefined behavior (use-after-free or leak) |
| Out of memory | Calls `handle_alloc_error()` (abort or panic) |
| Collection during Drop | Safe; dead Gcs are detectable via `is_dead()` |
