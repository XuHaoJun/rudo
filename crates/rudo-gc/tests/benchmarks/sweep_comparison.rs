//! Benchmarks for lazy sweep vs eager sweep performance.
//!
//! These benchmarks compare pause times and throughput between lazy and eager sweep
//! garbage collection strategies.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use rudo_gc::{collect, trace::Trace, Gc};

#[derive(Trace)]
struct BenchNode {
    value: i64,
    next: Option<Gc<BenchNode>>,
}

fn bench_sweep_lazy_pause_time(c: &mut Criterion) {
    c.bench_function("lazy_sweep_pause_time", |b| {
        b.iter(|| {
            let mut nodes = Vec::new();
            for i in 0..100 {
                let node = Gc::new(BenchNode {
                    value: i,
                    next: nodes.last().cloned(),
                });
                nodes.push(node);
            }
            black_box(&nodes);
            drop(nodes);
            collect();
        });
    });
}

fn bench_sweep_lazy_throughput(c: &mut Criterion) {
    c.bench_function("lazy_sweep_throughput", |b| {
        b.iter(|| {
            for i in 0..1000 {
                let _gc = Gc::new(i);
            }
            black_box(collect());
        });
    });
}

fn bench_sweep_all_dead_optimization(c: &mut Criterion) {
    c.bench_function("lazy_sweep_all_dead_optimization", |b| {
        b.iter(|| {
            let mut nodes = Vec::new();
            for i in 0..200 {
                let node = Gc::new(BenchNode {
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

fn bench_sweep_incremental_allocation(c: &mut Criterion) {
    c.bench_function("lazy_sweep_incremental_allocation", |b| {
        b.iter(|| {
            for _ in 0..100 {
                let mut nodes = Vec::new();
                for i in 0..10 {
                    let node = Gc::new(BenchNode {
                        value: i,
                        next: nodes.last().cloned(),
                    });
                    nodes.push(node);
                }
                black_box(&nodes);
                nodes.clear();
            }
        });
    });
}

criterion_group!(
    name = lazy_sweep_benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_millis(100))
        .measurement_time(std::time::Duration::from_millis(500));
    targets =
        bench_sweep_lazy_pause_time,
        bench_sweep_lazy_throughput,
        bench_sweep_all_dead_optimization,
        bench_sweep_incremental_allocation,
);

criterion_main!(lazy_sweep_benches);
