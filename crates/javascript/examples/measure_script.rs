use rust_browser_javascript::{Vm, VmLimits};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Measured;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(bytes: usize) {
    let current = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(current, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Measured {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() { add(layout.size()); }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            if new_size >= layout.size() {
                add(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Measured = Measured;

fn main() {
    let mut vm = Vm::with_limits(VmLimits {
        max_source_bytes: 12 * 1024 * 1024,
        max_heap_bytes: 512 * 1024 * 1024,
        ..VmLimits::default()
    });
    let source = std::fs::read_to_string(std::env::args().nth(1).expect("source path")).unwrap();
    let start = LIVE.load(Ordering::Relaxed);
    PEAK.store(start, Ordering::Relaxed);
    let timer = Instant::now();
    let result = vm.check_syntax(&source);
    let peak = PEAK.load(Ordering::Relaxed);
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let rss = status.lines().find(|s| s.starts_with("VmHWM:")).unwrap();
    println!("source_bytes={} start_alloc={} peak_alloc={} {} elapsed={:.2}s result={result:?}",
        source.len(), start, peak, rss, timer.elapsed().as_secs_f64());
    result.expect("complete public application should pass compilation");
}
