//! Spawn wrapper for automatic root tracking.
//!
//! This module provides the [`spawn`][crate::tokio::spawn] function for spawning
//! async tasks with automatic GC root tracking.

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::task::JoinHandle;

use super::guard::GcRootGuard;
use super::GcRootSet;

/// Future wrapper that holds a root guard for automatic tracking in spawned tasks.
///
/// See [`GcRootScope`][super::GcRootScope] for more details.
pub struct GcRootScope<F> {
    future: F,
    _guard: GcRootGuard,
}

impl<F> GcRootScope<F> {
    /// Creates a new scope wrapping the given future with an automatic root guard.
    ///
    /// # Safety
    ///
    /// The guard must be created with a valid pointer to the `GcBox` being tracked.
    pub unsafe fn new(future: F, guard: GcRootGuard) -> Self {
        Self {
            future,
            _guard: guard,
        }
    }
}

impl<F: std::future::Future> std::future::Future for GcRootScope<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We poll the wrapped future with the same waker.
        // The future is pinned for its lifetime, which is valid.
        let this = unsafe { self.get_unchecked_mut() };
        let future = unsafe { Pin::new_unchecked(&mut this.future) };
        future.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::internal_ptr;
    use crate::Gc;

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    fn register_test_root(ptr: std::ptr::NonNull<u8>) {
        GcRootSet::global().register(ptr.as_ptr() as usize);
    }

    #[tokio::test]
    async fn test_spawn_basic() {
        let gc = Gc::new(TestData { value: 42 });
        let ptr = internal_ptr(&gc);

        register_test_root(ptr);

        let _guard = unsafe { GcRootGuard::new(ptr) };

        let handle = crate::tokio::spawn(async move { gc.value });

        let result = handle.await.unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_multiple_spawns() {
        let gc = Gc::new(TestData { value: 42 });
        let ptr = internal_ptr(&gc);
        register_test_root(ptr);
        let _guard = unsafe { GcRootGuard::new(ptr) };

        let handles: Vec<_> = (0..5)
            .map(|i| {
                let gc = gc.clone();
                crate::tokio::spawn(async move { gc.value + i })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.await.unwrap()).collect();

        assert_eq!(results, vec![42, 43, 44, 45, 46]);
    }

    #[test]
    #[cfg(miri)]
    fn test_gcrootscope_unsafe_new_validates() {
        use std::ptr::NonNull;

        let ptr = NonNull::dangling();
        let guard = unsafe { GcRootGuard::new(ptr) };
        let future = async { 42 };

        let scope = unsafe { GcRootScope::new(future, guard) };
        assert!(GcRootSet::global().is_registered(ptr.as_ptr() as usize));
    }
}
