//! `HandleScope` v2 implementation for compile-time safe GC handles.
//!
//! This module provides lifetime-bound handles that prevent dangling references
//! at compile time, following V8's `HandleScope` design patterns.

#![allow(missing_docs)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::elidable_lifetime_names)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::ref_as_ptr)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::manual_assert)]

mod r#async;
mod local_handles;

pub use local_handles::{
    HandleBlock, HandleScopeData, HandleSlot, LocalHandles, HANDLE_BLOCK_SIZE,
};
pub use r#async::{AsyncHandle, AsyncHandleGuard, AsyncHandleScope, AsyncScopeEntry};

use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Deref;

use crate::heap::ThreadControlBlock;
use crate::ptr::GcBox;
use crate::trace::Trace;
use crate::Gc;

pub struct HandleScope<'env> {
    tcb: &'env ThreadControlBlock,
    prev_next: *mut HandleSlot,
    prev_limit: *mut HandleSlot,
    prev_level: u32,
    _marker: PhantomData<*mut ()>,
}

impl<'env> HandleScope<'env> {
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let local_handles = tcb.local_handles_ptr();

        // SAFETY: We have exclusive access via the borrow of tcb
        let (prev_next, prev_limit, prev_level) = unsafe {
            let handles = &mut *local_handles;
            let scope_data = handles.scope_data_mut();
            let prev_next = scope_data.next;
            let prev_limit = scope_data.limit;
            let prev_level = scope_data.level;
            scope_data.level = prev_level
                .checked_add(1)
                .expect("HandleScope level overflow");
            (prev_next, prev_limit, prev_level)
        };

        Self {
            tcb,
            prev_next,
            prev_limit,
            prev_level,
            _marker: PhantomData,
        }
    }

    pub fn handle<'scope, T: Trace>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T> {
        let local_handles = self.tcb.local_handles_ptr();

        // SAFETY: We have exclusive access via the borrow of tcb
        let slot = unsafe { (*local_handles).allocate() };

        let gc_box_ptr = Gc::internal_ptr(gc) as *const GcBox<()>;
        unsafe {
            (*slot).set(gc_box_ptr);
        }

        Handle {
            slot,
            _marker: PhantomData,
        }
    }

    pub fn level(&self) -> u32 {
        unsafe { (*self.tcb.local_handles_ptr()).scope_data().level }
    }
}

impl Drop for HandleScope<'_> {
    fn drop(&mut self) {
        let local_handles = self.tcb.local_handles_ptr();

        // SAFETY: We have exclusive access via the borrow of tcb
        unsafe {
            let handles = &mut *local_handles;
            let scope_data = handles.scope_data_mut();
            scope_data.next = self.prev_next;
            scope_data.limit = self.prev_limit;
            scope_data.level = self.prev_level;
        }
    }
}

pub struct Handle<'scope, T: Trace + 'static> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}

impl<'scope, T: Trace + 'static> Handle<'scope, T> {
    #[inline]
    pub fn get(&self) -> &T {
        unsafe {
            let slot = &*self.slot;
            let gc_box_ptr = slot.as_ptr() as *const u8;
            // SAFETY: The handle slot contains a valid GcBox<T> pointer.
            let gc = Gc::<T>::from_raw(gc_box_ptr);
            let value_ref: &T = &*gc;
            let result = &*(value_ref as *const T);
            std::mem::forget(gc);
            result
        }
    }

    pub fn to_gc(&self) -> Gc<T> {
        unsafe {
            let slot = &*self.slot;
            let ptr = slot.as_ptr() as *const u8;
            // SAFETY: The handle slot contains a valid GcBox pointer
            // This increments the reference count
            let gc: Gc<T> = Gc::from_raw(ptr);
            // Increment ref count since from_raw doesn't
            let gc_clone = gc.clone();
            std::mem::forget(gc);
            gc_clone
        }
    }

    pub unsafe fn as_ptr(&self) -> *const GcBox<T> {
        let slot = unsafe { &*self.slot };
        slot.as_ptr() as *const GcBox<T>
    }
}

impl<T: Trace + 'static> Deref for Handle<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T: Trace + 'static> Clone for Handle<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace + 'static> Copy for Handle<'_, T> {}

impl<T: Trace + 'static + std::fmt::Debug> std::fmt::Debug for Handle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Handle").field(&self.get()).finish()
    }
}

impl<T: Trace + 'static + std::fmt::Display> std::fmt::Display for Handle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.get(), f)
    }
}

pub struct EscapeableHandleScope<'env> {
    inner: HandleScope<'env>,
    escaped: Cell<bool>,
    escape_slot: *mut HandleSlot,
    #[cfg(debug_assertions)]
    #[allow(dead_code)]
    parent_level: u32,
}

impl<'env> EscapeableHandleScope<'env> {
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let local_handles = tcb.local_handles_ptr();

        // Pre-allocate escape slot in parent scope
        let escape_slot = unsafe { (*local_handles).allocate() };

        #[cfg(debug_assertions)]
        let parent_level = unsafe { (*local_handles).scope_data().level };

        let inner = HandleScope::new(tcb);

        Self {
            inner,
            escaped: Cell::new(false),
            escape_slot,
            #[cfg(debug_assertions)]
            parent_level,
        }
    }

    pub fn handle<'scope, T: Trace + 'static>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T> {
        self.inner.handle(gc)
    }

    pub fn escape<'parent, T: Trace + 'static>(
        &self,
        parent: &'parent HandleScope<'_>,
        handle: Handle<'_, T>,
    ) -> Handle<'parent, T> {
        if self.escaped.get() {
            panic!("EscapeableHandleScope::escape() can only be called once");
        }

        #[cfg(debug_assertions)]
        {
            if parent.level() + 1 != self.inner.level() {
                panic!("escape() called with incorrect parent scope");
            }
        }

        self.escaped.set(true);

        unsafe {
            let src_slot = &*handle.slot;
            (*self.escape_slot).set(src_slot.as_ptr());
        }

        Handle {
            slot: self.escape_slot,
            _marker: PhantomData,
        }
    }
}

pub struct MaybeHandle<'scope, T: Trace + 'static> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}

impl<'scope, T: Trace + 'static> MaybeHandle<'scope, T> {
    pub const fn empty() -> Self {
        Self {
            slot: std::ptr::null(),
            _marker: PhantomData,
        }
    }

    pub fn from_handle(handle: Handle<'scope, T>) -> Self {
        Self {
            slot: handle.slot,
            _marker: PhantomData,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.slot.is_null()
    }

    pub fn to_handle(self) -> Option<Handle<'scope, T>> {
        if self.slot.is_null() {
            None
        } else {
            Some(Handle {
                slot: self.slot,
                _marker: PhantomData,
            })
        }
    }
}

impl<T: Trace + 'static> Clone for MaybeHandle<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace + 'static> Copy for MaybeHandle<'_, T> {}

impl<T: Trace + 'static> Default for MaybeHandle<'_, T> {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(debug_assertions)]
pub struct SealedHandleScope<'env> {
    tcb: &'env ThreadControlBlock,
    prev_sealed_level: u32,
}

#[cfg(not(debug_assertions))]
pub struct SealedHandleScope<'env>(PhantomData<&'env ()>);

impl<'env> SealedHandleScope<'env> {
    #[cfg(debug_assertions)]
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let local_handles = tcb.local_handles_ptr();

        let prev_sealed_level = unsafe {
            let handles = &mut *local_handles;
            let scope_data = handles.scope_data_mut();
            let prev = scope_data.sealed_level;
            scope_data.sealed_level = scope_data.level;
            prev
        };

        Self {
            tcb,
            prev_sealed_level,
        }
    }

    #[cfg(not(debug_assertions))]
    pub fn new(_tcb: &'env ThreadControlBlock) -> Self {
        Self(PhantomData)
    }
}

#[cfg(debug_assertions)]
impl Drop for SealedHandleScope<'_> {
    fn drop(&mut self) {
        let local_handles = self.tcb.local_handles_ptr();

        unsafe {
            let handles = &mut *local_handles;
            handles.scope_data_mut().sealed_level = self.prev_sealed_level;
        }
    }
}
