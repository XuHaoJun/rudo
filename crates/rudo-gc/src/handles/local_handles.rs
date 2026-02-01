//! Local handle storage for per-thread handle management.
//!
//! This module implements the core data structures for handle-based GC root tracking:
//! - [`HandleSlot`]: Individual handle storage (single word)
//! - [`HandleBlock`]: Fixed-size array of slots (256 slots)
//! - [`HandleScopeData`]: Runtime state for scope management
//! - [`LocalHandles`]: Per-thread handle storage manager

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::ptr_cast_constness)]
#![allow(clippy::manual_assert)]
#![allow(clippy::use_self)]
#![allow(clippy::non_send_fields_in_send_ty)]

use std::cell::UnsafeCell;
use std::ptr::NonNull;

use crate::ptr::GcBox;

/// The number of handle slots per block.
///
/// Each handle block contains this many slots for storing GC references.
pub const HANDLE_BLOCK_SIZE: usize = 256;

/// A single slot for storing a GC handle.
///
/// Each slot holds a pointer to a `GcBox`. Handles are allocated by
/// claiming a slot and storing the `GcBox` pointer in it.
#[repr(C)]
pub struct HandleSlot {
    gc_box_ptr: *const GcBox<()>,
}

impl HandleSlot {
    /// Creates a new slot with the given `GcBox` pointer.
    #[inline]
    pub const fn new(gc_box_ptr: *const GcBox<()>) -> Self {
        Self { gc_box_ptr }
    }

    /// Creates a null (empty) slot.
    #[inline]
    pub const fn null() -> Self {
        Self {
            gc_box_ptr: std::ptr::null(),
        }
    }

    /// Returns the `GcBox` pointer stored in this slot.
    #[inline]
    pub const fn as_ptr(&self) -> *const GcBox<()> {
        self.gc_box_ptr
    }

    /// Returns `true` if this slot is null (empty).
    #[inline]
    pub fn is_null(&self) -> bool {
        self.gc_box_ptr.is_null()
    }

    /// Sets the `GcBox` pointer for this slot.
    #[inline]
    pub fn set(&mut self, ptr: *const GcBox<()>) {
        self.gc_box_ptr = ptr;
    }
}

impl Default for HandleSlot {
    fn default() -> Self {
        Self::null()
    }
}

/// A block of handle slots.
///
/// Handle blocks are linked together to form a growing pool of slots.
/// Each block contains [`HANDLE_BLOCK_SIZE`] slots plus a pointer to the next block.
pub struct HandleBlock {
    pub(crate) slots: UnsafeCell<[HandleSlot; HANDLE_BLOCK_SIZE]>,
    next: Option<NonNull<HandleBlock>>,
}

impl HandleBlock {
    /// Creates a new handle block with all slots initialized to null.
    pub fn new() -> Box<Self> {
        Box::new(Self {
            slots: UnsafeCell::new(std::array::from_fn(|_| HandleSlot::null())),
            next: None,
        })
    }

    /// Returns a pointer to the start of the slots array.
    #[inline]
    pub fn slots_ptr(&mut self) -> *mut HandleSlot {
        self.slots.get() as *mut HandleSlot
    }

    /// Returns a pointer past the end of the slots array.
    #[inline]
    pub fn slots_end(&mut self) -> *mut HandleSlot {
        unsafe {
            let ptr = self.slots.get() as *mut HandleSlot;
            ptr.add(HANDLE_BLOCK_SIZE)
        }
    }

    /// Returns the next block in the chain, if any.
    #[inline]
    pub fn next(&self) -> Option<NonNull<HandleBlock>> {
        self.next
    }

    /// Sets the next block in the chain.
    #[inline]
    pub fn set_next(&mut self, next: Option<NonNull<HandleBlock>>) {
        self.next = next;
    }
}

impl Default for HandleBlock {
    fn default() -> Self {
        Self {
            slots: UnsafeCell::new(std::array::from_fn(|_| HandleSlot::null())),
            next: None,
        }
    }
}

/// Tracks the current state of handle scope nesting.
///
/// This structure maintains the allocation pointer, limit, and nesting level
/// for the current handle scope.
///
/// # Note on `sealed_level` Field
///
/// The `sealed_level` field is `#[cfg(debug_assertions)]` only.
/// This is INTENTIONAL - in release builds, `SealedHandleScope` is a ZST
/// (zero-sized type) and uses the type system for sealing instead of
/// runtime checks.
///
/// In release builds, `is_sealed()` always returns `false` because:
/// - `SealedHandleScope` cannot be constructed incorrectly
/// - The type system enforces correct nesting at compile time
#[derive(Debug)]
pub struct HandleScopeData {
    pub(crate) next: *mut HandleSlot,
    pub(crate) limit: *mut HandleSlot,
    pub(crate) level: u32,
    #[cfg(debug_assertions)]
    pub(crate) sealed_level: u32,
}

impl HandleScopeData {
    /// Creates a new scope data structure with level 0 (inactive).
    pub const fn new() -> Self {
        Self {
            next: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            level: 0,
            #[cfg(debug_assertions)]
            sealed_level: 0,
        }
    }

    /// Returns `true` if handles are being allocated (level > 0).
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.level > 0
    }

    /// Returns `true` if handle creation is sealed at the current level.
    ///
    /// In debug builds, this prevents handles from being created in sealed scopes.
    /// In release builds, this always returns `false` because the type system
    /// enforces correct sealing at compile time via `SealedHandleScope` as a ZST.
    #[cfg(debug_assertions)]
    #[inline]
    pub const fn is_sealed(&self) -> bool {
        self.level <= self.sealed_level && self.sealed_level > 0
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    pub const fn is_sealed(&self) -> bool {
        false
    }
}

impl Default for HandleScopeData {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-local handle storage.
///
/// `LocalHandles` manages a linked list of handle blocks and tracks
/// the current scope state. It provides allocation and iteration facilities.
pub struct LocalHandles {
    blocks: Option<NonNull<HandleBlock>>,
    current_block: Option<NonNull<HandleBlock>>,
    pub(crate) scope_data: HandleScopeData,
}

impl LocalHandles {
    /// Creates a new empty handle storage.
    pub fn new() -> Self {
        Self {
            blocks: None,
            current_block: None,
            scope_data: HandleScopeData::new(),
        }
    }

    /// Returns mutable access to the scope data.
    #[inline]
    pub fn scope_data_mut(&mut self) -> &mut HandleScopeData {
        &mut self.scope_data
    }

    /// Returns immutable access to the scope data.
    #[inline]
    pub fn scope_data(&self) -> &HandleScopeData {
        &self.scope_data
    }

    /// Adds a new block to the handle chain.
    ///
    /// # Returns
    ///
    /// A tuple of (next, limit) pointers for the new block
    pub fn add_block(&mut self) -> (*mut HandleSlot, *mut HandleSlot) {
        let new_block = HandleBlock::new();
        let new_block_ptr = unsafe { NonNull::new_unchecked(Box::into_raw(new_block)) };

        if let Some(mut current) = self.current_block {
            unsafe {
                current.as_mut().set_next(Some(new_block_ptr));
            }
        }

        if self.blocks.is_none() {
            self.blocks = Some(new_block_ptr);
        }

        self.current_block = Some(new_block_ptr);

        unsafe {
            let block = new_block_ptr.as_ptr();
            let start = (*block).slots.get() as *mut HandleSlot;
            let end = start.add(HANDLE_BLOCK_SIZE);
            (start, end)
        }
    }

    /// Allocates a new handle slot.
    ///
    /// # Returns
    ///
    /// A pointer to the allocated slot
    ///
    /// # Panics
    ///
    /// Panics in debug mode if the scope is sealed
    #[inline]
    pub fn allocate(&mut self) -> *mut HandleSlot {
        #[cfg(debug_assertions)]
        {
            if self.scope_data.is_sealed() {
                panic!("Cannot allocate handle in sealed scope");
            }
        }

        if self.scope_data.next >= self.scope_data.limit || self.scope_data.next.is_null() {
            let (start, end) = self.add_block();
            self.scope_data.next = start;
            self.scope_data.limit = end;
        }

        let slot = self.scope_data.next;
        unsafe {
            self.scope_data.next = self.scope_data.next.add(1);
        }
        slot
    }

    /// Iterates over all allocated handles, calling the visitor for each.
    ///
    /// Only visits slots that have been allocated (up to the current next pointer).
    ///
    /// # Arguments
    ///
    /// * `visitor` - A closure that receives a pointer to each `GcBox`
    pub fn iterate<F>(&self, mut visitor: F)
    where
        F: FnMut(*const GcBox<()>),
    {
        let mut block_opt = self.blocks;
        while let Some(block_ptr) = block_opt {
            let block = unsafe { block_ptr.as_ref() };

            let slots_start = block.slots.get() as *const HandleSlot;
            let slots_end = if Some(block_ptr) == self.current_block {
                self.scope_data.next as *const HandleSlot
            } else {
                unsafe { slots_start.add(HANDLE_BLOCK_SIZE) }
            };

            let mut slot_ptr = slots_start;
            while slot_ptr < slots_end {
                let slot = unsafe { &*slot_ptr };
                if !slot.is_null() {
                    visitor(slot.as_ptr());
                }
                slot_ptr = unsafe { slot_ptr.add(1) };
            }

            block_opt = block.next();
        }
    }
}

impl Default for LocalHandles {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LocalHandles {
    fn drop(&mut self) {
        let mut block_opt = self.blocks.take();
        while let Some(block_ptr) = block_opt {
            let block = unsafe { Box::from_raw(block_ptr.as_ptr()) };
            block_opt = block.next;
        }
    }
}

unsafe impl Send for LocalHandles {}
