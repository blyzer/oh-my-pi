//! Research-only probe of actual pinned omp-journal APIs, not production envd changes.
#[path = "ownership_probe.rs"]
mod ownership;
use std::{hint::black_box, io::Read, time::Instant};
use omp_journal::blob::BlobStore;
fn main() {
    let mut args = std::env::args().skip(1);
    let size: u64 = args.next().unwrap().parse().unwrap();
    assert!((1_048_576..=52_428_800).contains(&size));
    let root = args.next().unwrap();
    let store = BlobStore::open(root).unwrap();
    let reference = store.put_reader(std::io::repeat(90).take(size)).unwrap();
    let offset = size / 2;
    let length = 4096;
    for method in ["get_then_slice", "open_range_then_read"] {
        for sample in 0..30 {
            let before = ownership::snapshot();
            let before_live = ownership::live();
            let start = Instant::now();
            let (retained_bytes, checksum) = if method == "get_then_slice" {
                let bytes = black_box(store.get(&reference).unwrap().slice(offset as usize..offset as usize+length));
                let retained = ownership::live()-before_live;
                let checksum = black_box(bytes.iter().map(|v| *v as u64).sum::<u64>());
                (retained, checksum)
            } else {
                let opened = store.open_range(reference.hash, offset, length as u64).unwrap();
                assert_eq!(opened.reference(), reference);
                assert_eq!(opened.offset(), offset);
                assert_eq!(opened.len(), length as u64);
                let mut bytes = vec![0; length];
                opened.into_file().take(length as u64).read_exact(&mut bytes).unwrap();
                let retained = ownership::live()-before_live;
                let checksum = black_box(bytes.iter().map(|v| *v as u64).sum::<u64>());
                (retained, checksum)
            };
            let elapsed_ns = start.elapsed().as_nanos();
            let after = ownership::snapshot();
            assert_eq!(checksum, (90*length) as u64);
            println!("{{\"kind\":\"sample\",\"blob_bytes\":{size},\"range_bytes\":{length},\"method\":\"{method}\",\"sample\":{sample},\"elapsed_ns\":{elapsed_ns},\"allocation_calls\":{},\"requested_bytes\":{},\"retained_requested_bytes_during_return\":{retained_bytes}}}", after.0-before.0, after.1-before.1);
        }
    }
}
