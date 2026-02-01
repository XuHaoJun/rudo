//! Local handle storage for per-thread handle management.
//!
//! This module implements the core data structures for handle-based GC root tracking:
//! - `HandleSlot`: Individual handle storage (single word)
//! - `HandleBlock`: Fixed-size array of slots (256 slots)
//! - `HandleScopeData`: Runtime state for scope management
//! - `LocalHandles`: Per-thread handle storage manager

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::ptr_cast_constness)]
#![allow(clippy::manual_assert)]
#![allow(clippy::use_self)]
#![allow(clippy::non_send_fields_in_send_ty)]

use std::ptr::NonNull;

use crate::ptr::GcBox;

pub const HANDLE_BLOCK_SIZE: usize = 256;

#[repr(C)]
pub struct HandleSlot {
    gc_box_ptr: *const GcBox<()>,
}

impl HandleSlot {
    #[inline]
    pub const fn new(gc_box_ptr: *const GcBox<()>) -> Self {
        Self { gc_box_ptr }
    }

    #[inline]
    pub const fn null() -> Self {
        Self {
            gc_box_ptr: std::ptr::null(),
        }
    }

    #[inline]
    pub const fn as_ptr(&self) -> *const GcBox<()> {
        self.gc_box_ptr
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.gc_box_ptr.is_null()
    }

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

pub struct HandleBlock {
    pub(crate) slots: [HandleSlot; HANDLE_BLOCK_SIZE],
    next: Option<NonNull<HandleBlock>>,
}

impl HandleBlock {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            slots: std::array::from_fn(|_| HandleSlot::null()),
            next: None,
        })
    }

    #[inline]
    pub fn slots_ptr(&mut self) -> *mut HandleSlot {
        self.slots.as_mut_ptr()
    }

    #[inline]
    pub fn slots_end(&mut self) -> *mut HandleSlot {
        unsafe { self.slots.as_mut_ptr().add(HANDLE_BLOCK_SIZE) }
    }

    #[inline]
    pub fn next(&self) -> Option<NonNull<HandleBlock>> {
        self.next
    }

    #[inline]
    pub fn set_next(&mut self, next: Option<NonNull<HandleBlock>>) {
        self.next = next;
    }
}

impl Default for HandleBlock {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| HandleSlot::null()),
            next: None,
        }
    }
}

#[derive(Debug)]
pub struct HandleScopeData {
    pub(crate) next: *mut HandleSlot,
    pub(crate) limit: *mut HandleSlot,
    pub(crate) level: u32,
    #[cfg(debug_assertions)]
    pub(crate) sealed_level: u32,
}

impl HandleScopeData {
    pub const fn new() -> Self {
        Self {
            next: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            level: 0,
            #[cfg(debug_assertions)]
            sealed_level: 0,
        }
    }

    #[inline]
    pub const fn is_active(&self) -> bool {
        self.level > 0
    }

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

pub struct LocalHandles {
    blocks: Option<NonNull<HandleBlock>>,
    current_block: Option<NonNull<HandleBlock>>,
    pub(crate) scope_data: HandleScopeData,
}

impl LocalHandles {
    pub fn new() -> Self {
        Self {
            blocks: None,
            current_block: None,
            scope_data: HandleScopeData::new(),
        }
    }

    #[inline]
    pub fn scope_data_mut(&mut self) -> &mut HandleScopeData {
        &mut self.scope_data
    }

    #[inline]
    pub fn scope_data(&self) -> &HandleScopeData {
        &self.scope_data
    }

    pub fn add_block(&mut self) -> (*mut HandleSlot, *mut HandleSlot) {
        let new_block = HandleBlock::new();
        let new_block_ptr = NonNull::from(Box::leak(new_block));

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
            let start = (*block).slots.as_mut_ptr();
            let end = start.add(HANDLE_BLOCK_SIZE);
            (start, end)
        }
    }

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

    pub fn iterate<F>(&self, mut visitor: F)
    where
        F: FnMut(*const GcBox<()>),
    {
        let mut block_opt = self.blocks;
        while let Some(block_ptr) = block_opt {
            let block = unsafe { block_ptr.as_ref() };

            let slots_start = block.slots.as_ptr();
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
