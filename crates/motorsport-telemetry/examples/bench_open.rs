use aim_telemetry::AimFile;
use cosworth_telemetry::CosworthFile;
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::TelemetrySource;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn median(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn main() {
    let pds = fixture("synthetic_cosworth.pds");
    let mp4 = fixture("synthetic_aimd.mp4");

    for _ in 0..200 {
        black_box(CosworthFile::open(&pds).unwrap());
        black_box(AimFile::open(&mp4).unwrap());
    }

    const BATCHES: usize = 9;
    const ITERATIONS: u64 = 2_000;
    let mut pds_times = Vec::with_capacity(BATCHES);
    let mut mp4_times = Vec::with_capacity(BATCHES);
    for _ in 0..BATCHES {
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(CosworthFile::open(&pds).unwrap());
        }
        pds_times.push(start.elapsed().as_nanos() as u64 / ITERATIONS);

        let start = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(AimFile::open(&mp4).unwrap());
        }
        mp4_times.push(start.elapsed().as_nanos() as u64 / ITERATIONS);
    }

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    black_box(CosworthFile::open(&pds).unwrap());
    let pds_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let pds_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    black_box(AimFile::open(&mp4).unwrap());
    let mp4_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let mp4_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

    let pds_ns = median(pds_times);
    let mp4_ns = median(mp4_times);
    println!("METRIC combined_load_ns={}", pds_ns + mp4_ns);
    println!("METRIC pds_load_ns={pds_ns}");
    println!("METRIC mp4_load_ns={mp4_ns}");
    println!("METRIC pds_allocations={pds_allocations}");
    println!("METRIC pds_allocated_bytes={pds_bytes}");
    println!("METRIC mp4_allocations={mp4_allocations}");
    println!("METRIC mp4_allocated_bytes={mp4_bytes}");

    if let Some(path) = std::env::args_os().nth(1) {
        let path = PathBuf::from(path);
        let mut open_times = Vec::with_capacity(BATCHES);
        for _ in 0..BATCHES {
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                black_box(MotecFile::open(&path).unwrap());
            }
            open_times.push(start.elapsed().as_nanos() as u64 / ITERATIONS);
        }

        let file = MotecFile::open(&path).unwrap();
        let sample_count = file
            .channels()
            .iter()
            .map(|channel| channel.sample_count)
            .sum::<u64>();
        let start = Instant::now();
        let mut checksum = 0.0;
        for (channel_index, channel) in file.channels().iter().enumerate() {
            for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
                for local_index in 0..chunk.sample_count {
                    checksum += file.decode(channel_index, chunk_index, local_index);
                }
            }
        }
        black_box(checksum);
        let decode_ns = start.elapsed().as_nanos() as u64;
        println!("METRIC motec_load_ns={}", median(open_times));
        println!("METRIC motec_sample_count={sample_count}");
        println!("METRIC motec_decode_all_ns={decode_ns}");
    }
}
