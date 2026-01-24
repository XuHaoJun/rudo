//! A wrapper for closures that explicitly captures and traces dependencies.

use crate::trace::{Trace, Visitor};

/// A wrapper for a closure that captures and traces dependencies.
///
/// This is useful when a closure captures `Gc<T>` pointers and is stored
/// in a way that doesn't automatically trace those pointers (e.g., inside
/// a `Box<dyn Fn()>`).
pub struct TraceClosure<C, D> {
    closure: C,
    deps: D,
}

impl<C, D> TraceClosure<C, D> {
    /// Create a new `TraceClosure` with the given closure and dependencies.
    pub const fn new(deps: D, closure: C) -> Self {
        Self { closure, deps }
    }
}

impl<C: Fn(), D> TraceClosure<C, D> {
    /// Call the inner closure.
    pub fn call(&self) {
        (self.closure)();
    }
}

unsafe impl<C, D: Trace> Trace for TraceClosure<C, D> {
    fn trace(&self, visitor: &mut impl Visitor) {
        self.deps.trace(visitor);
    }
}
