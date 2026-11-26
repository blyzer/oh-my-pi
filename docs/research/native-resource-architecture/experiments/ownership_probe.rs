//! Research-only synthetic ownership control; not an OMP2 IR or production allocator.
//! Build with `rustc -O --edition=2024 ownership_probe.rs -o /tmp/omp-ownership-probe`.
use std::{alloc::{GlobalAlloc, Layout, System}, hint::black_box, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::Instant};

struct CountingSystem;
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
pub fn snapshot() -> (u64, u64) { (CALLS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed)) }
pub fn live() -> u64 { LIVE.load(Ordering::Relaxed) }
fn allocated(size: usize) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(size as u64, Ordering::Relaxed);
    let live = LIVE.fetch_add(size as u64, Ordering::Relaxed) + size as u64;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
// SAFETY: forwards unchanged layouts and pointers to System; counters never allocate.
unsafe impl GlobalAlloc for CountingSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies the GlobalAlloc layout contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() { allocated(layout.size()); }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as alloc_zeroed.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() { allocated(layout.size()); }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: forwards the original allocation and layout.
        unsafe { System.dealloc(ptr, layout) };
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: preserves the caller's realloc contract.
        let new = unsafe { System.realloc(ptr, layout, size) };
        if !new.is_null() {
            LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
            allocated(size);
        }
        new
    }
}
#[global_allocator]
static ALLOCATOR: CountingSystem = CountingSystem;

#[derive(Clone)]
struct Symbol { id: u32, name: String, path: String, docs: String, callers: Vec<u32> }
fn graph(nodes: usize) -> Vec<Symbol> {
    (0..nodes).map(|i| Symbol { id: i as u32, name: format!("symbol_{i:08}"),
        path: format!("src/module_{:04}/function.rs", i % 1000),
        docs: "typed symbol metadata; ownership probe, not a code intelligence result; ".repeat(2),
        callers: vec![(i / 2) as u32, i.saturating_sub(1) as u32, (i % 31) as u32] }).collect()
}
fn check(graph: &[Symbol]) -> usize { let s = &graph[graph.len()/2]; graph.len() + s.id as usize + s.name.len() + s.path.len() + s.docs.len() + s.callers.len() }
fn memory() -> String {
    let raw = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    let value = |name: &str| -> u64 { raw.lines().find(|line| line.starts_with(name)).and_then(|line| line.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024 };
    format!("{{\"rss_bytes\":{},\"pss_bytes\":{},\"uss_bytes\":{},\"requested_live_bytes\":{},\"requested_peak_bytes\":{}}}", value("Rss:"), value("Pss:"), value("Private_Clean:") + value("Private_Dirty:"), LIVE.load(Ordering::Relaxed), PEAK.load(Ordering::Relaxed))
}
fn main() {
    let nodes: usize = std::env::args().nth(1).expect("nodes").parse().expect("integer");
    assert!((1..=100_000).contains(&nodes), "bounded research run");
    let samples = 30;
    let source = Arc::new(graph(nodes));
    let expected = check(&source);
    println!("{{\"kind\":\"baseline\",\"nodes\":{nodes},\"memory\":{}}}", memory());
    for method in ["deep_clone", "arc_clone"] {
        for sample in 0..samples {
            let before_calls = CALLS.load(Ordering::Relaxed);
            let before_bytes = BYTES.load(Ordering::Relaxed);
            let start = Instant::now();
            let result = if method == "deep_clone" {
                let copy = black_box(source.as_ref().clone());
                black_box(check(&copy))
            } else {
                let copy = black_box(Arc::clone(&source));
                black_box(check(&copy))
            };
            let elapsed = start.elapsed().as_nanos();
            let calls = CALLS.load(Ordering::Relaxed) - before_calls;
            let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
            assert_eq!(result, expected);
            println!("{{\"kind\":\"sample\",\"nodes\":{nodes},\"method\":\"{method}\",\"sample\":{sample},\"elapsed_ns\":{elapsed},\"allocation_calls\":{calls},\"requested_bytes\":{bytes}}}");
        }
        println!("{{\"kind\":\"settled\",\"nodes\":{nodes},\"method\":\"{method}\",\"memory\":{}}}", memory());
    }
}
