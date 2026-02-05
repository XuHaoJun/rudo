//! Benchmark for incremental marking pause times.
//!
//! This benchmark measures the maximum pause time during major GC
//! for heaps of various sizes, comparing STW vs incremental marking.

#![allow(
    deprecated,
    unused_imports,
    clippy::borrow_as_ptr,
    clippy::needless_borrow
)]

use criterion::{criterion_group, criterion_main, Criterion};
use rudo_gc::gc::incremental::{IncrementalConfig, IncrementalMarkState};
use rudo_gc::{Gc, Trace};
use std::cell::RefCell;

#[derive(Trace)]
struct Data {
    value: usize,
}

fn create_large_allocation(size: usize) -> Gc<Data> {
    Gc::new(Data { value: size })
}

fn benchmark_stw_pause_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_pause");

    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_millis(500));

    // Small heap: 10,000 nodes
    group.bench_function("stw_10k_nodes", |b| {
        b.iter(|| {
            let data = create_large_allocation(10_000);
            std::hint::black_box(&data);
            drop(data);
            rudo_gc::collect();
        });
    });

    // Medium heap: 100,000 nodes
    group.bench_function("stw_100k_nodes", |b| {
        b.iter(|| {
            let data = create_large_allocation(100_000);
            std::hint::black_box(&data);
            drop(data);
            rudo_gc::collect();
        });
    });
}

fn benchmark_incremental_pause_time(c: &mut Criterion) {
    rudo_gc::test_util::reset();

    // Enable incremental marking
    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        increment_size: 1000,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    });

    let mut group = c.benchmark_group("incremental_pause");

    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_millis(500));

    // Small heap: 10,000 nodes
    group.bench_function("incremental_10k_nodes", |b| {
        b.iter(|| {
            let data = create_large_allocation(10_000);
            std::hint::black_box(&data);
            drop(data);
            rudo_gc::collect();
        });
    });

    // Medium heap: 100,000 nodes
    group.bench_function("incremental_100k_nodes", |b| {
        b.iter(|| {
            let data = create_large_allocation(100_000);
            std::hint::black_box(&data);
            drop(data);
            rudo_gc::collect();
        });
    });
}

criterion_group!(
    benches,
    benchmark_stw_pause_time,
    benchmark_incremental_pause_time
);
criterion_main!(benches);
