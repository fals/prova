use calc::primes::count_primes;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn primes(c: &mut Criterion) {
    c.bench_function("count_primes", |b| {
        b.iter(|| assert_eq!(count_primes(black_box(500_000)), 41_538))
    });
}

criterion_group!(benches, primes);
criterion_main!(benches);
