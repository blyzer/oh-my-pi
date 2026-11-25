//! Research-only probe linking the actual pinned omp-core rlib built by its tests.
//! The graph is synthetic, not an existing OMP2 IR. Mutable graph containers remain owned.
#[path = "ownership_probe.rs"]
mod ownership;
use std::{hint::black_box, time::Instant};
use omp_core::Str;
#[derive(Clone)]
struct Symbol { id: u32, name: Str, path: Str, docs: Str, callers: Vec<u32> }
fn main() {
    let n: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    assert!((1..=100_000).contains(&n));
    let source: Vec<Symbol> = (0..n).map(|i| Symbol {
        id: i as u32, name: Str::new(format!("symbol_{i:08}")),
        path: Str::new(format!("src/module_{:04}/function.rs", i%1000)),
        docs: Str::new("typed symbol metadata; ownership probe, not a code intelligence result; ".repeat(2)),
        callers: vec![(i/2) as u32, i.saturating_sub(1) as u32, (i%31) as u32],
    }).collect();
    for sample in 0..30 {
        let before = ownership::snapshot();
        let start = Instant::now();
        let copy = black_box(source.clone());
        let symbol = black_box(&copy[n/2]);
        assert_eq!(symbol.id as usize, n/2);
        black_box(symbol.name.len()+symbol.path.len()+symbol.docs.len()+symbol.callers.len());
        drop(copy);
        let elapsed_ns = start.elapsed().as_nanos();
        let after = ownership::snapshot();
        println!("{{\"kind\":\"sample\",\"nodes\":{n},\"method\":\"omp_core_str_graph_clone\",\"sample\":{sample},\"elapsed_ns\":{elapsed_ns},\"allocation_calls\":{},\"requested_bytes\":{}}}", after.0-before.0, after.1-before.1);
    }
}
