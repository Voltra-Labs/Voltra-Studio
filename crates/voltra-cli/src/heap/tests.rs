//! The counts, and that the binary's allocator keeps them.

use super::{Counters, HeapStats, Resident, parse_status, stats};

#[test]
fn allocations_reallocations_and_frees_are_counted_in_bytes_and_blocks() {
    let counters = Counters::new();
    counters.allocated(100);
    counters.allocated(50);
    counters.reallocated(100, 300);
    counters.reallocated(50, 20);
    assert_eq!(
        counters.snapshot(),
        HeapStats {
            live_bytes: 320,
            live_blocks: 2,
            peak_bytes: 350,
            allocations: 4,
        }
    );
    counters.freed(300);
    counters.freed(20);
    let after = counters.snapshot();
    assert_eq!((after.live_bytes, after.live_blocks), (0, 0));
    assert_eq!(after.peak_bytes, 350, "the peak stays");
}

#[test]
fn the_binary_counts_through_its_global_allocator() {
    // Other tests allocate at the same time, so only what they cannot undo is
    // checked: the allocation count only grows, and the peak is at least a
    // block this large.
    let before = stats();
    let block = std::hint::black_box(vec![1u8; 8 << 20]);
    let during = stats();
    assert!(during.allocations > before.allocations);
    assert!(during.peak_bytes >= 8 << 20);
    drop(block);
}

#[test]
fn resident_memory_is_read_from_proc_status() {
    let status = "Name:\tvoltra\nVmPeak:\t  100 kB\nVmHWM:\t   35676 kB\nVmRSS:\t   35448 kB\n";
    assert_eq!(
        parse_status(status),
        Some(Resident {
            now_kib: 35_448,
            peak_kib: 35_676,
        })
    );
    assert_eq!(parse_status("Name:\tvoltra\n"), None);
    #[cfg(target_os = "linux")]
    {
        let mut buffer = String::with_capacity(super::STATUS_CAPACITY);
        let resident = super::resident(&mut buffer).unwrap();
        assert!(resident.now_kib > 0 && resident.peak_kib >= resident.now_kib);
        assert!(buffer.capacity() == super::STATUS_CAPACITY, "read in place");
    }
}
