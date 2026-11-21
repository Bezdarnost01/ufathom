//! Timings of in-process reads: `cargo run --release -p ufathom-mem --example read_bench`.
//!
//! Compares the operating system's copy call (`SelfMemory::safe`), the default
//! reader (`SelfMemory::new`, which takes the guarded direct copy where the
//! platform has one) and a plain `ptr::read_volatile`, which is the floor a C++
//! mod pays and has no protection at all. Prints nanoseconds per operation.
//! Needs no game: it reads its own heap.

#![allow(
    clippy::print_stdout,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    reason = "a benchmark prints its numbers and indexes arrays it built itself"
)]

use std::hint::black_box;
use std::ptr;
use std::time::{Duration, Instant};

use ufathom_mem::{MemoryReader, MemoryReaderExt, SelfMemory};

/// Objects in the mixed pattern.
const OBJECTS: usize = 1_000;
/// Fields read per object in the mixed pattern.
const FIELDS: usize = 10;
/// Size of one synthetic object in bytes.
const OBJECT_BYTES: usize = 512;
/// How long each measurement runs at least.
const BUDGET: Duration = Duration::from_millis(300);

/// Runs `body` repeatedly for at least [`BUDGET`] and returns nanoseconds per
/// call of `body`, which itself performs `per_call` operations.
fn time(per_call: usize, mut body: impl FnMut()) -> f64 {
    for _ in 0..1_000 {
        body();
    }
    let mut calls = 0u64;
    let start = Instant::now();
    while start.elapsed() < BUDGET {
        for _ in 0..256 {
            body();
        }
        calls += 256;
    }
    start.elapsed().as_nanos() as f64 / (calls as f64 * per_call as f64)
}

fn main() {
    let safe = SelfMemory::safe();
    let fast = SelfMemory::new();

    // One heap block, so every read hits the same committed region.
    let block: Vec<u8> = (0..=255u8).cycle().take(64 * 1024).collect();
    let base = block.as_ptr() as u64;
    let mut buf8 = [0u8; 8];
    let mut buf256 = [0u8; 256];

    println!("{:<34}{:>12}{:>12}", "ns per read", "8 bytes", "256 bytes");
    let readers: [(&str, &dyn MemoryReader); 2] = [
        ("SelfMemory::safe (OS copy call)", &safe),
        ("SelfMemory::new (default)", &fast),
    ];
    for (name, reader) in readers {
        let small = time(1, || {
            black_box(reader.read_exact(black_box(base + 64), &mut buf8)).ok();
        });
        let large = time(1, || {
            black_box(reader.read_exact(black_box(base + 64), &mut buf256)).ok();
        });
        println!("{name:<34}{small:>12.1}{large:>12.1}");
    }
    let small = time(1, || {
        // SAFETY: `base + 64` is inside `block`, which is alive and 64 KiB long.
        black_box(unsafe { ptr::read_volatile(black_box(base + 64) as *const u64) });
    });
    let large = time(1, || {
        // SAFETY: as above; 256 bytes fit inside `block` from offset 64.
        black_box(unsafe { ptr::read_volatile(black_box(base + 64) as *const [u8; 256]) });
    });
    println!(
        "{:<34}{small:>12.1}{large:>12.1}",
        "ptr::read_volatile (unguarded)"
    );

    // Mixed: OBJECTS separate allocations, FIELDS scattered u32 reads in each.
    let objects: Vec<Vec<u8>> = (0..OBJECTS)
        .map(|n| vec![(n % 251) as u8; OBJECT_BYTES])
        .collect();
    let addrs: Vec<u64> = objects
        .iter()
        .map(|object| object.as_ptr() as u64)
        .collect();
    let offsets: [u64; FIELDS] = [0, 8, 24, 56, 88, 136, 200, 264, 352, 440];

    println!();
    println!(
        "{:<34}{:>14}{:>14}",
        "1,000 objects x 10 fields", "us per pass", "ns per field"
    );
    let readers: [(&str, &dyn MemoryReader); 2] = [
        ("SelfMemory::safe (OS copy call)", &safe),
        ("SelfMemory::new (default)", &fast),
    ];
    for (name, reader) in readers {
        let per_pass = time(1, || {
            let mut sum = 0u32;
            for addr in &addrs {
                for offset in offsets {
                    sum = sum.wrapping_add(reader.read_u32(addr + offset).unwrap_or(0));
                }
            }
            black_box(sum);
        });
        println!(
            "{name:<34}{:>14.1}{:>14.1}",
            per_pass / 1_000.0,
            per_pass / (OBJECTS * FIELDS) as f64
        );
    }
    let per_pass = time(1, || {
        let mut sum = 0u32;
        for addr in &addrs {
            for offset in offsets {
                // SAFETY: every address is inside a live `Vec` of OBJECT_BYTES bytes.
                sum =
                    sum.wrapping_add(unsafe { ptr::read_volatile((addr + offset) as *const u32) });
            }
        }
        black_box(sum);
    });
    println!(
        "{:<34}{:>14.1}{:>14.1}",
        "ptr::read_volatile (unguarded)",
        per_pass / 1_000.0,
        per_pass / (OBJECTS * FIELDS) as f64
    );

    // The same objects read whole: one 80-byte call each.
    println!();
    println!(
        "{:<34}{:>14}{:>14}",
        "1,000 objects x 80 bytes at once", "us per pass", "ns per object"
    );
    let readers: [(&str, &dyn MemoryReader); 2] = [
        ("SelfMemory::safe (OS copy call)", &safe),
        ("SelfMemory::new (default)", &fast),
    ];
    for (name, reader) in readers {
        let mut whole = [0u8; 80];
        let per_pass = time(1, || {
            for addr in &addrs {
                black_box(reader.read_exact(*addr, &mut whole)).ok();
            }
            black_box(whole[0]);
        });
        println!(
            "{name:<34}{:>14.1}{:>14.1}",
            per_pass / 1_000.0,
            per_pass / OBJECTS as f64
        );
    }

    // What a caller pays around the read: the runtime reads an object header
    // into a buffer per object. A heap buffer costs more than the fast read.
    println!();
    println!(
        "{:<34}{:>14}",
        "48-byte header read (default)", "ns per read"
    );
    let first = addrs.first().copied().unwrap_or(0);
    let heap = time(1, || {
        let mut bytes = vec![0u8; black_box(48)];
        black_box(fast.read_exact(first, &mut bytes)).ok();
        black_box(bytes.first().copied());
    });
    println!("{:<34}{heap:>14.1}", "into a fresh Vec");
    let stack = time(1, || {
        let mut bytes = [0u8; 256];
        let len = black_box(48);
        if let Some(part) = bytes.get_mut(..len) {
            black_box(fast.read_exact(first, part)).ok();
        }
        black_box(bytes.first().copied());
    });
    println!("{:<34}{stack:>14.1}", "into a stack array");
    drop(objects);
    drop(block);
}
