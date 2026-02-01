//! Async handle support for GC references across await points.
//!
//! This module provides `AsyncHandleScope` and `AsyncHandle<T>` for safe
//! async/await GC reference management.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::ref_as_ptr)]
#![allow(clippy::borrow_as_ptr)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_cast_constness)]
#![allow(clippy::non_send_fields_in_send_ty)]
#![allow(clippy::elidable_lifetime_names)]
#![allow(clippy::manual_assert)]
#![allow(clippy::explicit_auto_deref)]
#![allow(clippy::as_ptr_cast_mut)]
#![allow(clippy::non_canonical_clone_impl)]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::heap::ThreadControlBlock;
use crate::ptr::GcBox;
use crate::trace::Trace;
use crate::Gc;

use super::local_handles::{HandleBlock, HandleSlot, HANDLE_BLOCK_SIZE};

static ASYNC_SCOPE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct AsyncScopeEntry {
    pub id: u64,
    pub block_ptr: *const HandleBlock,
    pub used: *const AtomicUsize,
}

unsafe impl Send for AsyncScopeEntry {}
unsafe impl Sync for AsyncScopeEntry {}

pub struct AsyncHandleScope {
    id: u64,
    tcb: Arc<ThreadControlBlock>,
    block: Box<HandleBlock>,
    used: AtomicUsize,
    dropped: AtomicBool,
}

impl AsyncHandleScope {
    #[allow(clippy::unused_self)]
    pub fn new(_tcb: &ThreadControlBlock) -> Self {
        let id = ASYNC_SCOPE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let block = HandleBlock::new();

        let tcb_arc = crate::heap::current_thread_control_block()
            .expect("AsyncHandleScope::new called outside GC thread");

        let scope = Self {
            id,
            tcb: tcb_arc,
            block,
            used: AtomicUsize::new(0),
            dropped: AtomicBool::new(false),
        };

        scope.tcb.register_async_scope(
            scope.id,
            scope.block.as_ref() as *const HandleBlock,
            &scope.used as *const AtomicUsize,
        );

        scope
    }

    pub fn handle<T: Trace + 'static>(&self, gc: &Gc<T>) -> AsyncHandle<T> {
        let idx = self.used.fetch_add(1, Ordering::Relaxed);
        if idx >= HANDLE_BLOCK_SIZE {
            panic!("AsyncHandleScope: exceeded maximum handle count ({HANDLE_BLOCK_SIZE})");
        }

        let slot_ptr = unsafe {
            let slots_ptr = self.block.slots.as_ptr() as *mut HandleSlot;
            slots_ptr.add(idx)
        };

        let gc_box_ptr = Gc::internal_ptr(gc) as *const GcBox<()>;
        unsafe {
            (*slot_ptr).set(gc_box_ptr);
        }

        AsyncHandle {
            slot: slot_ptr,
            scope_id: self.id,
            _marker: PhantomData,
        }
    }

    pub fn with_guard<F, R>(&self, f: F) -> R
    where
        F: FnOnce(AsyncHandleGuard<'_>) -> R,
    {
        let guard = AsyncHandleGuard {
            scope: self,
            _marker: PhantomData,
        };
        f(guard)
    }

    pub fn iterate<F>(&self, mut visitor: F)
    where
        F: FnMut(*const GcBox<()>),
    {
        let used = self.used.load(Ordering::Acquire);
        for i in 0..used {
            let slot = unsafe { &*self.block.slots.as_ptr().add(i) };
            if !slot.is_null() {
                visitor(slot.as_ptr());
            }
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for AsyncHandleScope {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
        self.tcb.unregister_async_scope(self.id);
    }
}

unsafe impl Send for AsyncHandleScope {}
unsafe impl Sync for AsyncHandleScope {}

pub struct AsyncHandleGuard<'scope> {
    scope: &'scope AsyncHandleScope,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> AsyncHandleGuard<'scope> {
    pub fn get<'a, T: Trace + 'static>(&'a self, handle: &'a AsyncHandle<T>) -> &'a T {
        #[cfg(debug_assertions)]
        {
            if handle.scope_id != self.scope.id {
                panic!("AsyncHandle accessed from wrong scope");
            }
        }

        unsafe { handle.get() }
    }
}

pub struct AsyncHandle<T: Trace + 'static> {
    slot: *const HandleSlot,
    scope_id: u64,
    _marker: PhantomData<*const T>,
}

impl<T: Trace + 'static> AsyncHandle<T> {
    /// # Safety
    ///
    /// The parent `AsyncHandleScope` must still be alive.
    pub unsafe fn get(&self) -> &T {
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const u8;
        let gc: Gc<T> = unsafe { Gc::<T>::from_raw(gc_box_ptr) };
        let value_ref: &T = &*gc;
        let result = unsafe { &*(value_ref as *const T) };
        std::mem::forget(gc);
        result
    }

    pub fn to_gc(&self) -> Gc<T> {
        unsafe {
            let slot = &*self.slot;
            let ptr = slot.as_ptr() as *const u8;
            let gc: Gc<T> = Gc::from_raw(ptr);
            let gc_clone = gc.clone();
            std::mem::forget(gc);
            gc_clone
        }
    }
}

impl<T: Trace + 'static> Clone for AsyncHandle<T> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot,
            scope_id: self.scope_id,
            _marker: PhantomData,
        }
    }
}

impl<T: Trace + 'static> Copy for AsyncHandle<T> {}

unsafe impl<T: Trace + 'static> Send for AsyncHandle<T> {}
unsafe impl<T: Trace + 'static> Sync for AsyncHandle<T> {}

#[macro_export]
macro_rules! spawn_with_gc {
    ($gc:expr => |$handle:ident| $body:expr) => {{
        let __gc = $gc;
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::handles::AsyncHandleScope::new(&__tcb);
            let $handle = __scope.handle(&__gc);
            let __result = { $body };
            drop(__scope);
            __result
        })
    }};

    ($($gc:ident),+ => |$($handle:ident),+| $body:expr) => {{
        $(let $gc = $gc;)+
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::handles::AsyncHandleScope::new(&__tcb);
            $(let $handle = __scope.handle(&$gc);)+
            let __result = { $body };
            drop(__scope);
            __result
        })
    }};
}
