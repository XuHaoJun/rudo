//! Benchmark: Lazy Sweep vs Eager Sweep Pause Time Comparison
//!
//! Measures GC pause time, throughput and latency distribution
//! to quantify the improvement from lazy sweep.

use criterion::{criterion_group, criterion_main, Criterion};
use rudo_gc::{collect, Gc, Trace};
use std::hint::black_box;
use std::time::{Duration, Instant};

#[derive(Trace)]
struct Node {
    value: i64,
    next: Option<Gc<Node>>,
}

fn bench_pause_time_100(c: &mut Criterion) {
    c.bench_function("pause_time_100_objects", |b| {
        b.iter(|| {
            let mut nodes = Vec::new();
            for i in 0..100 {
                let node = Gc::new(Node {
                    value: i,
                    next: None,
                });
                nodes.push(node);
            }
            black_box(&nodes);
            drop(nodes);
            collect();
        });
    });
}

fn bench_pause_time_1000(c: &mut Criterion) {
    c.bench_function("pause_time_1000_objects", |b| {
        b.iter(|| {
            let mut nodes = Vec::new();
            for i in 0..1000 {
                let node = Gc::new(Node {
                    value: i,
                    next: None,
                });
                nodes.push(node);
            }
            black_box(&nodes);
            drop(nodes);
            collect();
        });
    });
}

fn bench_pause_time_10000(c: &mut Criterion) {
    c.bench_function("pause_time_10000_objects", |b| {
        b.iter(|| {
            let mut nodes = Vec::new();
            for i in 0..10000 {
                let node = Gc::new(Node {
                    value: i,
                    next: None,
                });
                nodes.push(node);
            }
            black_box(&nodes);
            drop(nodes);
            collect();
        });
    });
}

fn bench_pause_time_100000(c: &mut Criterion) {
    c.bench_function("pause_time_100000_objects", |b| {
        b.iter(|| {
            let mut nodes = Vec::new();
            for i in 0..100000 {
                let node = Gc::new(Node {
                    value: i,
                    next: None,
                });
                nodes.push(node);
            }
            black_box(&nodes);
            drop(nodes);
            collect();
        });
    });
}

fn bench_throughput_alloc(c: &mut Criterion) {
    c.bench_function("throughput_alloc_10000", |b| {
        b.iter(|| {
            for i in 0..10000 {
                let _gc = Gc::new(i);
            }
            black_box(collect());
        });
    });
}

fn bench_throughput_alloc_dealloc(c: &mut Criterion) {
    c.bench_function("throughput_alloc_dealloc", |b| {
        b.iter(|| {
            let mut refs = Vec::new();
            for i in 0..1000 {
                refs.push(Gc::new(i));
                if i % 10 == 0 {
                    refs.remove(0);
                }
            }
            black_box(collect());
        });
    });
}

fn bench_latency_distribution(c: &mut Criterion) {
    c.bench_function("latency_distribution_5000", |b| {
        b.iter_custom(|iterations| {
            let mut latencies = Vec::new();
            let iter_count = iterations as usize;
            for _ in 0..iter_count {
                let mut nodes = Vec::new();
                for i in 0..5000 {
                    nodes.push(Gc::new(Node {
                        value: i,
                        next: None,
                    }));
                }
                let start = Instant::now();
                drop(nodes);
                collect();
                latencies.push(start.elapsed());
            }
            latencies.sort();
            let idx50 = (iter_count * 50) / 100;
            let idx95 = (iter_count * 95) / 100;
            let idx99 = (iter_count * 99) / 100;
            let p50 = latencies[idx50];
            let p95 = latencies[idx95];
            let p99 = latencies[idx99];
            Duration::from_nanos(((p50.as_nanos() + p95.as_nanos() + p99.as_nanos()) / 3) as u64)
        });
    });
}

fn bench_sustained_100_cycles(c: &mut Criterion) {
    c.bench_function("sustained_100_cycles", |b| {
        b.iter(|| {
            for _ in 0..100 {
                let mut nodes = Vec::new();
                for _ in 0..100 {
                    nodes.push(Gc::new(Node {
                        value: 0,
                        next: None,
                    }));
                }
                nodes.clear();
                collect();
            }
        });
    });
}

criterion_group!(
    name = sweep_comparison;
    config = Criterion::default()
        .sample_size(30)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
        .noise_threshold(0.05)
        .confidence_level(0.99);
    targets =
        bench_pause_time_100,
        bench_pause_time_1000,
        bench_pause_time_10000,
        bench_pause_time_100000,
        bench_throughput_alloc,
        bench_throughput_alloc_dealloc,
        bench_latency_distribution,
        bench_sustained_100_cycles,
);

criterion_main!(sweep_comparison);
