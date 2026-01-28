#![allow(unused)]

//! Mark phase optimizations for parallel garbage collection.
//!
//! This module contains implementations of Chez Scheme-inspired optimizations:
//! - Push-based work transfer to reduce steal contention
//! - Segment ownership for better load distribution
//! - Mark bitmap for memory-efficient marking
//! - Dynamic stack growth monitoring

pub mod bitmap;
pub mod ownership;

pub use bitmap::MarkBitmap;
pub use ownership::{get_owned_pages_tracker, OwnedPagesTracker};
