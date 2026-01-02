//! `BiBOP` (Big Bag of Pages) memory management.
//!
//! This module implements the core memory layout using page-aligned segments
//! with size-class based allocation for O(1) allocation performance.
//!
//! # `BiBOP` Memory Layout
//!
//! Memory is divided into 4KB pages. Each page contains objects of a single
//! size class. This allows O(1) lookup of object metadata from its address.

use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;

// ============================================================================
// Constants
// ============================================================================

/// Size of each memory page (4KB aligned).
pub const PAGE_SIZE: usize = 4096;

/// Mask for extracting page address from a pointer.
pub const PAGE_MASK: usize = !(PAGE_SIZE - 1);

/// Magic number for validating GC pages ("RUDG" in ASCII).
pub const MAGIC_GC_PAGE: u32 = 0x5255_4447;

/// Size classes for object allocation.
/// Objects are routed to the smallest size class that fits them.
#[allow(dead_code)]
pub const SIZE_CLASSES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Objects larger than this go to the Large Object Space.
pub const MAX_SMALL_OBJECT_SIZE: usize = 2048;

// ============================================================================
// PageHeader - Metadata at the start of each page
// ============================================================================

/// Metadata stored at the beginning of each page.
///
/// This header enables O(1) lookup of object information from any pointer
/// within the page using simple alignment operations.
#[repr(C)]
pub struct PageHeader {
    /// Magic number to validate this is a GC page.
    pub magic: u32,
    /// Size of each object slot in bytes (u32 to support multi-page large objects).
    pub block_size: u32,
    /// Maximum number of objects in this page.
    pub obj_count: u16,
    /// Offset from the start of the page to the first object.
    pub header_size: u16,
    /// Generation index (for future generational GC).
    pub generation: u8,
    /// Bitflags (`is_large_object`, `is_dirty`, etc.).
    pub flags: u8,
    /// Padding for alignment.
    _padding: [u8; 2],
    /// Bitmap of marked objects (one bit per slot).
    /// Size depends on `obj_count`, but we reserve space for max possible.
    pub mark_bitmap: [u64; 4], // 256 bits = enough for smallest size class (16 bytes)
    /// Bitmap of dirty objects (one bit per slot).
    /// Used for generational GC to track old objects that point to young objects.
    pub dirty_bitmap: [u64; 4],
    /// Bitmap of allocated objects (one bit per slot).
    /// Used to distinguish between newly unreachable and already free slots.
    pub allocated_bitmap: [u64; 4],
    /// Index of first free slot in free list.
    pub free_list_head: Option<u16>,
}

impl PageHeader {
    /// Calculate the header size, rounded up to block alignment.
    #[must_use]
    pub const fn header_size(block_size: usize) -> usize {
        let base = std::mem::size_of::<Self>();
        // For small objects, block_size is a power-of-two size class (16, 32, ..., 2048).
        // For large objects, block_size is the actual size (which might not be a power-of-two).
        if block_size > 0 && block_size.is_power_of_two() && block_size <= MAX_SMALL_OBJECT_SIZE {
            (base + block_size - 1) & !(block_size - 1)
        } else {
            // For large objects, align to at least 16 bytes (standard alignment for GcBox header).
            // Note: alloc_large will handle stricter alignment if needed.
            (base + 15) & !15
        }
    }

    /// Calculate maximum objects per page for a given block size.
    #[must_use]
    pub const fn max_objects(block_size: usize) -> usize {
        (PAGE_SIZE - Self::header_size(block_size)) / block_size
    }

    /// Check if an object at the given index is marked.
    #[must_use]
    pub const fn is_marked(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.mark_bitmap[word] & (1 << bit)) != 0
    }

    /// Set the mark bit for an object at the given index.
    pub const fn set_mark(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word] |= 1 << bit;
    }

    /// Clear the mark bit for an object at the given index.
    #[allow(dead_code)]
    pub const fn clear_mark(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.mark_bitmap[word] &= !(1 << bit);
    }

    /// Clear all mark bits.
    pub const fn clear_all_marks(&mut self) {
        self.mark_bitmap = [0; 4];
    }

    /// Check if an object at the given index is dirty.
    #[must_use]
    pub const fn is_dirty(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.dirty_bitmap[word] & (1 << bit)) != 0
    }

    /// Set the dirty bit for an object at the given index.
    pub const fn set_dirty(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word] |= 1 << bit;
    }

    /// Clear the dirty bit for an object at the given index.
    #[allow(dead_code)]
    pub const fn clear_dirty(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.dirty_bitmap[word] &= !(1 << bit);
    }

    /// Clear all dirty bits.
    pub const fn clear_all_dirty(&mut self) {
        self.dirty_bitmap = [0; 4];
    }

    /// Check if an object at the given index is allocated.
    #[must_use]
    pub const fn is_allocated(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.allocated_bitmap[word] & (1 << bit)) != 0
    }

    /// Set the allocated bit for an object at the given index.
    pub const fn set_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word] |= 1 << bit;
    }

    /// Clear the allocated bit for an object at the given index.
    pub const fn clear_allocated(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.allocated_bitmap[word] &= !(1 << bit);
    }

    /// Clear all allocated bits.
    pub const fn clear_all_allocated(&mut self) {
        self.allocated_bitmap = [0; 4];
    }
}

// ============================================================================
// Segment - Size-class based memory pool
// ============================================================================

/// A segment manages pages of a specific size class.
///
/// Each segment contains multiple pages, all with the same block size.
/// Allocation uses bump-pointer allocation with free-list fallback.
pub struct Segment<const BLOCK_SIZE: usize> {
    /// All pages in this segment.
    pages: Vec<NonNull<PageHeader>>,
    /// Page currently being allocated from.
    current_page: Option<NonNull<PageHeader>>,
    /// Bump pointer for fast allocation.
    bump_ptr: *mut u8,
    /// End of allocatable region in current page.
    bump_end: *const u8,
}

impl<const BLOCK_SIZE: usize> Segment<BLOCK_SIZE> {
    /// Create a new empty segment.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pages: Vec::new(),
            current_page: None,
            bump_ptr: std::ptr::null_mut(),
            bump_end: std::ptr::null(),
        }
    }

    /// Allocate a new page for this segment.
    fn allocate_page(&mut self) -> NonNull<PageHeader> {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).expect("Invalid page layout");

        // SAFETY: Layout is valid and non-zero sized
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }

        // SAFETY: ptr is page-aligned, which is more strict than PageHeader's alignment.
        // PageHeader contains u64, so it needs 8-byte alignment. PAGE_SIZE is 4096.
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<PageHeader>();
        let obj_count = PageHeader::max_objects(BLOCK_SIZE);

        // SAFETY: We just allocated this memory
        unsafe {
            let h_size = PageHeader::header_size(BLOCK_SIZE);
            header.write(PageHeader {
                magic: MAGIC_GC_PAGE,
                #[allow(clippy::cast_possible_truncation)]
                block_size: BLOCK_SIZE as u32,
                #[allow(clippy::cast_possible_truncation)]
                obj_count: obj_count as u16,
                #[allow(clippy::cast_possible_truncation)]
                header_size: h_size as u16,
                generation: 0,
                flags: 0,
                _padding: [0; 2],
                mark_bitmap: [0; 4],
                dirty_bitmap: [0; 4],
                allocated_bitmap: [0; 4],
                free_list_head: None,
            });
        }

        // SAFETY: We checked for null above
        let page_ptr = unsafe { NonNull::new_unchecked(header) };
        self.pages.push(page_ptr);

        // Update range will be done by GlobalHeap

        // Initialize all slots with no-op drop to avoid crashes during sweep
        // if they are swept before being allocated.
        let header_size = PageHeader::header_size(BLOCK_SIZE);
        unsafe {
            for i in 0..obj_count {
                let obj_ptr = ptr.add(header_size + (i * BLOCK_SIZE));
                // We only need to set drop_fn. We can use GcBox<()> for this.
                // SAFETY: We just allocated this page and it's aligned.
                #[allow(clippy::cast_ptr_alignment)]
                let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
                std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
                    .write(crate::ptr::GcBox::<()>::no_op_drop);
                std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
                    .write(crate::ptr::GcBox::<()>::no_op_trace);
            }
        }

        // Set up bump allocation for this page
        self.current_page = Some(page_ptr);
        self.bump_ptr = unsafe { ptr.add(header_size) };
        self.bump_end = unsafe { ptr.add(PAGE_SIZE) };

        page_ptr
    }

    /// Allocate space for an object.
    ///
    /// Returns a pointer to uninitialized memory of size `BLOCK_SIZE`.
    pub fn allocate(&mut self) -> NonNull<u8> {
        // 1. Try free list of current page
        if let Some(current) = self.current_page {
            unsafe {
                let header = current.as_ptr();
                if let Some(idx) = (*header).free_list_head {
                    let header_size = PageHeader::header_size(BLOCK_SIZE);
                    let ptr = current
                        .as_ptr()
                        .cast::<u8>()
                        .add(header_size + (idx as usize * BLOCK_SIZE));

                    // Read next index from the memory itself
                    // SAFETY: The memory at ptr was previously an object or a free slot
                    #[allow(clippy::cast_ptr_alignment)]
                    let next_idx = *(ptr.cast::<Option<u16>>());
                    (*header).free_list_head = next_idx;
                    (*header).set_allocated(idx as usize);

                    return NonNull::new_unchecked(ptr);
                }
            }
        }

        // 2. Fast path: bump allocation
        if self.bump_ptr < self.bump_end.cast_mut() {
            let ptr = self.bump_ptr;
            // Record allocation in bitmap
            if let Some(mut current) = self.current_page {
                unsafe {
                    let header = current.as_mut();
                    let header_size = PageHeader::header_size(BLOCK_SIZE);
                    let offset = ptr as usize - (current.as_ptr() as usize + header_size);
                    let idx = offset / BLOCK_SIZE;
                    header.set_allocated(idx);
                }
            }
            self.bump_ptr = unsafe { self.bump_ptr.add(BLOCK_SIZE) };
            // SAFETY: bump_ptr is always valid when less than bump_end
            return unsafe { NonNull::new_unchecked(ptr) };
        }

        // 3. Slow path: Search other pages for free slots
        for &page in &self.pages {
            unsafe {
                let header = page.as_ptr();
                if let Some(idx) = (*header).free_list_head {
                    // Found a page with a free slot! Make it current.
                    self.current_page = Some(page);
                    // Disable bump allocation for this page as it's potentially fragmented
                    self.bump_ptr = self.bump_end.cast_mut();

                    let header_size = PageHeader::header_size(BLOCK_SIZE);
                    let ptr = page
                        .as_ptr()
                        .cast::<u8>()
                        .add(header_size + (idx as usize * BLOCK_SIZE));

                    #[allow(clippy::cast_ptr_alignment)]
                    let next_idx = *(ptr.cast::<Option<u16>>());
                    (*header).free_list_head = next_idx;

                    return NonNull::new_unchecked(ptr);
                }
            }
        }

        // 4. Ultra-slow path: need a new page
        self.allocate_page();
        self.allocate()
    }

    /// Get all pages in this segment.
    #[must_use]
    pub fn pages(&self) -> &[NonNull<PageHeader>] {
        &self.pages
    }

    /// Get all pages mutably.
    #[allow(dead_code)]
    #[must_use]
    pub const fn pages_mut(&mut self) -> &mut Vec<NonNull<PageHeader>> {
        &mut self.pages
    }
}

impl<const BLOCK_SIZE: usize> Default for Segment<BLOCK_SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const BLOCK_SIZE: usize> Drop for Segment<BLOCK_SIZE> {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        for page in &self.pages {
            // SAFETY: Pages were allocated with this layout
            unsafe {
                dealloc(page.as_ptr().cast(), layout);
            }
        }
    }
}

// ============================================================================
// SizeClass trait - Compile-time size class routing
// ============================================================================

/// Trait for computing size class at compile time.
#[allow(dead_code)]
pub trait SizeClass {
    /// The size of the type.
    const SIZE: usize;
    /// The size class for this type (smallest class that fits).
    const CLASS: usize;
    /// Index into the segments array.
    const CLASS_INDEX: usize;
}

impl<T> SizeClass for T {
    const SIZE: usize = std::mem::size_of::<T>();
    const CLASS: usize = compute_size_class(std::mem::size_of::<T>());
    const CLASS_INDEX: usize = compute_class_index(std::mem::size_of::<T>());
}

/// Compute the size class for a given size.
#[allow(dead_code)]
const fn compute_size_class(size: usize) -> usize {
    if size <= 16 {
        16
    } else if size <= 32 {
        32
    } else if size <= 64 {
        64
    } else if size <= 128 {
        128
    } else if size <= 256 {
        256
    } else if size <= 512 {
        512
    } else if size <= 1024 {
        1024
    } else {
        2048
    }
}

/// Compute the index into the segments array.
const fn compute_class_index(size: usize) -> usize {
    if size <= 16 {
        0
    } else if size <= 32 {
        1
    } else if size <= 64 {
        2
    } else if size <= 128 {
        3
    } else if size <= 256 {
        4
    } else if size <= 512 {
        5
    } else if size <= 1024 {
        6
    } else {
        7
    }
}

// ============================================================================
// GlobalHeap - Central memory manager
// ============================================================================

/// Central memory manager coordinating all segments.
pub struct GlobalHeap {
    /// One segment per size class.
    segment_16: Segment<16>,
    segment_32: Segment<32>,
    segment_64: Segment<64>,
    segment_128: Segment<128>,
    segment_256: Segment<256>,
    segment_512: Segment<512>,
    segment_1024: Segment<1024>,
    segment_2048: Segment<2048>,
    /// Pages for objects larger than 2KB.
    large_objects: Vec<NonNull<PageHeader>>,
    /// Map from page address to its corresponding large object head, size, and `header_size`.
    /// This enables interior pointer support for multi-page large objects.
    pub large_object_map: HashMap<usize, (usize, usize, usize)>,
    /// Total bytes allocated in young generation.
    young_allocated: usize,
    /// Total bytes allocated in old generation.
    old_allocated: usize,
    /// Minimum address managed by this heap.
    min_addr: usize,
    /// Maximum address managed by this heap.
    max_addr: usize,
}

impl GlobalHeap {
    /// Create a new empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            segment_16: Segment::new(),
            segment_32: Segment::new(),
            segment_64: Segment::new(),
            segment_128: Segment::new(),
            segment_256: Segment::new(),
            segment_512: Segment::new(),
            segment_1024: Segment::new(),
            segment_2048: Segment::new(),
            large_objects: Vec::new(),
            large_object_map: HashMap::new(),
            young_allocated: 0,
            old_allocated: 0,
            min_addr: usize::MAX,
            max_addr: 0,
        }
    }

    /// Update the address range of the heap.
    const fn update_range(&mut self, addr: usize, size: usize) {
        if addr < self.min_addr {
            self.min_addr = addr;
        }
        if addr + size > self.max_addr {
            self.max_addr = addr + size;
        }
    }

    /// Check if an address is within the heap's range.
    #[must_use]
    pub const fn is_in_range(&self, addr: usize) -> bool {
        addr >= self.min_addr && addr < self.max_addr
    }

    /// Allocate space for a value of type T.
    ///
    /// Returns a pointer to uninitialized memory.
    ///
    /// # Panics
    ///
    /// Panics if the type's alignment exceeds the size class alignment.
    /// This should be extremely rare in practice since size classes are
    /// powers of two starting at 16.
    pub fn alloc<T>(&mut self) -> NonNull<u8> {
        let size = std::mem::size_of::<T>();
        let align = std::mem::align_of::<T>();
        // All new allocations start in young generation
        self.young_allocated += size;

        if size > MAX_SMALL_OBJECT_SIZE {
            return self.alloc_large(size, align);
        }

        // Validate alignment - size class must satisfy alignment requirement
        let size_class = compute_size_class(size);
        assert!(
            size_class >= align,
            "Type alignment ({align}) exceeds size class ({size_class}). \
             Consider using a larger wrapper type."
        );

        let ptr = match compute_class_index(size) {
            0 => self.segment_16.allocate(),
            1 => self.segment_32.allocate(),
            2 => self.segment_64.allocate(),
            3 => self.segment_128.allocate(),
            4 => self.segment_256.allocate(),
            5 => self.segment_512.allocate(),
            6 => self.segment_1024.allocate(),
            _ => self.segment_2048.allocate(),
        };

        // Update heap range for conservative scanning
        self.update_range(ptr.as_ptr() as usize & PAGE_MASK, PAGE_SIZE);

        ptr
    }

    /// Allocate a large object (> 2KB).
    ///
    /// # Panics
    ///
    /// Panics if the alignment requirement exceeds `PAGE_SIZE`.
    fn alloc_large(&mut self, size: usize, align: usize) -> NonNull<u8> {
        // Validate alignment - page alignment (4096) should satisfy most types
        assert!(
            PAGE_SIZE >= align,
            "Type alignment ({align}) exceeds page size ({PAGE_SIZE}). \
             Such extreme alignment requirements are not supported."
        );

        // For large objects, allocate dedicated pages
        // The header must be followed by padding to satisfy the object's alignment.
        let base_h_size = PageHeader::header_size(size);
        let h_size = (base_h_size + align - 1) & !(align - 1);
        let total_size = h_size + size;
        let pages_needed = total_size.div_ceil(PAGE_SIZE);
        let alloc_size = pages_needed * PAGE_SIZE;

        let layout =
            Layout::from_size_align(alloc_size, PAGE_SIZE).expect("Invalid large object layout");

        // SAFETY: Layout is valid
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }

        // Initialize header for large object
        // SAFETY: ptr is page-aligned, which is more strict than PageHeader's alignment.
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<PageHeader>();
        // SAFETY: We just allocated this memory
        unsafe {
            header.write(PageHeader {
                magic: MAGIC_GC_PAGE,
                #[allow(clippy::cast_possible_truncation)]
                block_size: size as u32, // Store actual size for large objects (now u32)
                obj_count: 1,
                #[allow(clippy::cast_possible_truncation)]
                header_size: h_size as u16,
                generation: 0,
                flags: 0x01, // Mark as large object
                _padding: [0; 2],
                mark_bitmap: [0; 4],
                dirty_bitmap: [0; 4],
                allocated_bitmap: [0; 4],
                free_list_head: None,
            });
            // Mark the single object as allocated
            (*header).set_allocated(0);
        }

        let page_ptr = unsafe { NonNull::new_unchecked(header) };
        self.large_objects.push(page_ptr);

        // Register all pages of this large object in the map for interior pointer support.
        // This allows find_gc_box_from_ptr to find the head GcBox from any interior pointer.
        let header_addr = header as usize;
        for p in 0..pages_needed {
            let page_addr = header_addr + (p * PAGE_SIZE);
            self.large_object_map
                .insert(page_addr, (header_addr, size, h_size));
        }

        // Update heap range for conservative scanning
        self.update_range(header_addr, alloc_size);

        let gc_box_ptr = unsafe { ptr.add(h_size) };
        unsafe { NonNull::new_unchecked(gc_box_ptr) }
    }

    /// Get total bytes allocated.
    #[must_use]
    pub const fn total_allocated(&self) -> usize {
        self.young_allocated + self.old_allocated
    }

    /// Get bytes allocated in young generation.
    #[must_use]
    pub const fn young_allocated(&self) -> usize {
        self.young_allocated
    }

    /// Get bytes allocated in old generation.
    #[must_use]
    pub const fn old_allocated(&self) -> usize {
        self.old_allocated
    }

    /// Update allocation counters given a change in young/old bytes.
    /// This is used by the collector during promotion and sweeping.
    pub const fn update_allocated_bytes(&mut self, young: usize, old: usize) {
        self.young_allocated = young;
        self.old_allocated = old;
    }

    /// Iterate over all pages in all segments.
    pub fn all_pages(&self) -> impl Iterator<Item = NonNull<PageHeader>> + '_ {
        self.segment_16
            .pages()
            .iter()
            .copied()
            .chain(self.segment_32.pages().iter().copied())
            .chain(self.segment_64.pages().iter().copied())
            .chain(self.segment_128.pages().iter().copied())
            .chain(self.segment_256.pages().iter().copied())
            .chain(self.segment_512.pages().iter().copied())
            .chain(self.segment_1024.pages().iter().copied())
            .chain(self.segment_2048.pages().iter().copied())
            .chain(self.large_objects.iter().copied())
    }

    /// Get large object pages.
    #[must_use]
    pub fn large_object_pages(&self) -> &[NonNull<PageHeader>] {
        &self.large_objects
    }

    /// Get mutable access to large object pages (for sweep phase).
    #[allow(dead_code)]
    pub const fn large_object_pages_mut(&mut self) -> &mut Vec<NonNull<PageHeader>> {
        &mut self.large_objects
    }

    /// Get the size class index for a type.
    ///
    /// This is useful for debugging and verifying `BiBOP` routing.
    ///
    /// # Returns
    ///
    /// - `Some(index)` - Size class index (0-7) for small objects
    /// - `None` - Type is a large object (> 2KB)
    #[must_use]
    #[allow(dead_code)]
    pub const fn size_class_for<T>() -> Option<usize> {
        let size = std::mem::size_of::<T>();
        if size > MAX_SMALL_OBJECT_SIZE {
            None
        } else {
            Some(compute_class_index(size))
        }
    }

    /// Get the segment index and size class name for debugging.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rudo_gc::heap::GlobalHeap;
    ///
    /// let (class, name) = GlobalHeap::debug_size_class::<u64>();
    /// assert_eq!(name, "16-byte");
    /// ```
    #[must_use]
    #[allow(dead_code)]
    pub const fn debug_size_class<T>() -> (usize, &'static str) {
        let size = std::mem::size_of::<T>();
        let class = compute_size_class(size);
        let name = match class {
            16 => "16-byte",
            32 => "32-byte",
            64 => "64-byte",
            128 => "128-byte",
            256 => "256-byte",
            512 => "512-byte",
            1024 => "1024-byte",
            2048 => "2048-byte",
            _ => "large-object",
        };
        (class, name)
    }
}

impl Default for GlobalHeap {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Thread-local heap access
// ============================================================================

thread_local! {
    /// Thread-local heap instance.
    pub static HEAP: RefCell<GlobalHeap> = RefCell::new(GlobalHeap::new());
}

/// Execute a function with access to the thread-local heap.
pub fn with_heap<F, R>(f: F) -> R
where
    F: FnOnce(&mut GlobalHeap) -> R,
{
    HEAP.with(|heap| f(&mut heap.borrow_mut()))
}

// ============================================================================
// Pointer utilities for BiBOP
// ============================================================================

/// Get the page header for a pointer.
///
/// # Safety
///
/// The pointer must point to memory within a valid GC page.
#[allow(dead_code)]
#[must_use]
pub unsafe fn ptr_to_page_header(ptr: *const u8) -> *mut PageHeader {
    let addr = ptr as usize;
    let page_addr = addr & PAGE_MASK;

    // Provenance Rescue for Miri:
    // Pointers derived from stack references (like &self) often have provenance restricted
    // to just that field. To access the page header (which is outside that field),
    // we must use a pointer that originally had provenance for the entire page.
    // We get one from the global heap.
    #[cfg(miri)]
    {
        // Try to find the page in the thread-local heap.
        // If we can't find it (e.g. during early init), we fall back.
        let found = HEAP.with(|heap| {
            if let Ok(h) = heap.try_borrow() {
                // 1. Check small object segments
                for p in h.all_pages() {
                    if p.as_ptr() as usize == page_addr {
                        return Some(p.as_ptr());
                    }
                }
                // 2. Check large object map (handles multi-page objects)
                if let Some(&(head_addr, _, _)) = h.large_object_map.get(&page_addr) {
                    let head_ptr = head_addr as *mut PageHeader;
                    let offset = page_addr - head_addr;
                    // Derive from head_ptr to preserve provenance for the whole large object
                    #[allow(clippy::cast_ptr_alignment)]
                    return Some(unsafe { head_ptr.cast::<u8>().add(offset).cast::<PageHeader>() });
                }
                None
            } else {
                None
            }
        });
        if let Some(p) = found {
            return p;
        }
    }

    // Use the pointer itself to derive the header to preserve provenance (if broad enough)
    #[allow(clippy::cast_ptr_alignment)]
    ptr.wrapping_add(page_addr.wrapping_sub(addr))
        .cast_mut()
        .cast::<PageHeader>()
}

/// Validate that a pointer is within a GC-managed page.
///
/// # Safety
///
/// The pointer must be valid for reading.
#[allow(dead_code)]
#[must_use]
pub unsafe fn is_gc_pointer(ptr: *const u8) -> bool {
    // SAFETY: Caller guarantees ptr is valid
    unsafe {
        let header = ptr_to_page_header(ptr);
        if header.is_null() {
            return false;
        }
        (*header).magic == MAGIC_GC_PAGE
    }
}

/// Get the object index for a pointer within a page.
///
/// # Safety
///
/// The pointer must point to memory within a valid GC page.
#[allow(dead_code)]
#[must_use]
pub unsafe fn ptr_to_object_index(ptr: *const u8) -> Option<usize> {
    // SAFETY: Caller guarantees ptr is valid and within a GC page
    unsafe {
        let header = ptr_to_page_header(ptr);
        if (*header).magic != MAGIC_GC_PAGE {
            return None;
        }

        let block_size = (*header).block_size as usize;
        let header_size = PageHeader::header_size(block_size);
        let page_addr = header as usize;
        let ptr_addr = ptr as usize;

        if ptr_addr < page_addr + header_size {
            return None;
        }

        let offset = ptr_addr - (page_addr + header_size);
        let index = offset / block_size;

        if index >= (*header).obj_count as usize {
            return None;
        }

        Some(index)
    }
}

/// Try to find a valid GC object starting address from a potential interior pointer.
///
/// This is the core of conservative stack scanning. It takes a potential pointer
/// and, if it points into the GC heap, returns the address of the start of the
/// containing `GcBox`.
///
/// # Safety
///
/// The pointer must be safe to read if it is a valid pointer.
#[allow(dead_code)]
#[must_use]
pub unsafe fn find_gc_box_from_ptr(
    heap: &GlobalHeap,
    ptr: *const u8,
) -> Option<NonNull<crate::ptr::GcBox<()>>> {
    let addr = ptr as usize;
    // 1. Quick range check
    if !heap.is_in_range(addr) {
        return None;
    }

    // 2. Check if the pointer is aligned to something that could be a pointer
    unsafe {
        if addr % std::mem::align_of::<usize>() != 0 {
            return None;
        }

        // 3. Check large object map first (handles multi-page objects and avoids reading uninit tail pages)
        let page_addr = addr & crate::heap::PAGE_MASK;
        let (header_ptr_to_use, block_size_to_use, header_size_to_use, offset_to_use) =
            if let Some(&(head_addr, size, h_size)) = heap.large_object_map.get(&page_addr) {
                let h_ptr = head_addr as *mut PageHeader;

                // Recover provenance for Miri
                #[cfg(miri)]
                let h_ptr = heap
                    .large_objects
                    .iter()
                    .find(|p| p.as_ptr() as usize == head_addr)
                    .map_or(h_ptr, |p| p.as_ptr());

                if addr < head_addr + h_size {
                    return None;
                }
                (h_ptr, size, h_size, addr - (head_addr + h_size))
            } else {
                // Not in large object map, must be small object page with header
                let header_ptr = ptr_to_page_header(ptr);
                if (*header_ptr).magic == MAGIC_GC_PAGE {
                    let header = &*header_ptr;
                    let b_size = header.block_size as usize;
                    let h_size = PageHeader::header_size(b_size);

                    if addr < (header_ptr as usize) + h_size {
                        return None;
                    }
                    (
                        header_ptr,
                        b_size,
                        h_size,
                        addr - ((header_ptr as usize) + h_size),
                    )
                } else {
                    return None;
                }
            };

        let header = &*header_ptr_to_use;
        let index = offset_to_use / block_size_to_use;

        // 5. Index check
        if index >= header.obj_count as usize {
            return None;
        }

        // 6. Large object handling: with the map, we now support interior pointers!
        // For large objects, we ensure the pointer is within the allocated bounds.
        if header.flags & 0x01 != 0 {
            if offset_to_use >= block_size_to_use {
                return None;
            }
        } else if offset_to_use % block_size_to_use != 0 {
            // For small objects, we still require them to point to the start of an object
            // unless we want to support interior pointers for small objects too.
            // Currently, only large objects (which often contain large buffers)
            // really need interior pointer support for things like array slicing.
            return None;
        }

        // Bingo! We found a potential object.
        let obj_ptr = header_ptr_to_use
            .cast::<u8>()
            .wrapping_add(header_size_to_use)
            .wrapping_add(index * block_size_to_use);
        #[allow(clippy::cast_ptr_alignment)]
        Some(NonNull::new_unchecked(
            obj_ptr.cast::<crate::ptr::GcBox<()>>(),
        ))
    }
}
