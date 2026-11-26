//! Offline allocation probe. Only this executable installs the measuring allocator.
use rust_browser_javascript::{Vm, VmLimits};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Default)]
struct Counters {
    enabled: bool,
    net: i64,
    peak: i64,
    requested: u64,
}

thread_local! {
    static COUNTERS: Cell<Counters> = const {
        Cell::new(Counters { enabled: false, net: 0, peak: 0, requested: 0 })
    };
}

fn record(added: usize, removed: usize) {
    let _ = COUNTERS.try_with(|counts| {
        let mut c = counts.get();
        if c.enabled {
            c.net = c.net.saturating_add(added as i64).saturating_sub(removed as i64);
            c.peak = c.peak.max(c.net);
            c.requested = c.requested.saturating_add(added as u64);
            counts.set(c);
        }
    });
}

struct Meter;

// SAFETY: Every request is passed unchanged to System. Measurement neither
// allocates nor accesses the returned storage, and never changes layout or ownership.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            record(layout.size(), 0);
        }
        result
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() {
            record(layout.size(), 0);
        }
        result
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        record(0, layout.size());
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(ptr, layout, size) };
        if !result.is_null() {
            record(size, layout.size());
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: Meter = Meter;

#[cfg(windows)]
fn process_cpu() -> Option<Duration> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    fn time(t: FILETIME) -> u64 {
        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
    }
    let [mut created, mut exited, mut kernel, mut user] =
        [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
    // SAFETY: The handle is the current process and outputs are distinct live variables.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| Duration::from_nanos((time(kernel) + time(user)).saturating_mul(100)))
}

#[cfg(target_os = "linux")]
fn process_cpu() -> Option<Duration> {
    let mut t = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime initializes t if it reports success.
    if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, t.as_mut_ptr()) } != 0 {
        return None;
    }
    let t = unsafe { t.assume_init() };
    Some(Duration::new(t.tv_sec.try_into().ok()?, t.tv_nsec.try_into().ok()?))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn process_cpu() -> Option<Duration> { None }

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 2, "usage: profile_compiler compile|eval INPUT.js");
    let mode = args[0].to_str().expect("mode must be UTF-8");
    assert!(matches!(mode, "compile" | "eval"));
    let path = std::path::Path::new(&args[1]);
    let bytes = std::fs::metadata(path).expect("fixture metadata").len();
    assert!(bytes <= 12 * 1024 * 1024, "fixture exceeds browser script cap");
    let source = std::fs::read_to_string(path).expect("read UTF-8 fixture");
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 512 * 1024 * 1024,
        max_source_bytes: 12 * 1024 * 1024,
        ..VmLimits::default()
    });
    vm.collect_garbage();
    let mut phases = [("", Duration::ZERO, Counters::default()); 4];
    let mut count = 0;
    let start = Instant::now();
    let cpu = process_cpu();
    COUNTERS.set(Counters { enabled: true, ..Default::default() });
    let mut retained = None;
    let result = if mode == "compile" {
        vm.qa_profile_compilation(&source, |name| {
            phases[count] = (name, start.elapsed(), COUNTERS.get());
            count += 1;
        }).map(|value| { retained = Some(value); })
    } else {
        vm.eval(&source).and_then(|value| {
            if value == rust_browser_javascript::Value::Bool(true) {
                Ok(())
            } else {
                Err(rust_browser_javascript::JsError::new(
                    rust_browser_javascript::ErrorKind::InternalError,
                    "fixture did not return true",
                ))
            }
        })
    };
    let totals = COUNTERS.replace(Counters::default());
    let elapsed = start.elapsed();
    let cpu = cpu.zip(process_cpu()).map(|(a, b)| b.saturating_sub(a).as_micros());
    let code = match result {
        Ok(()) => "ok".to_owned(),
        Err(error) => format!("{:?}", error.kind),
    };
    println!("total\t{code}\t{}\t{}\t{}\t{}\t{}",
        elapsed.as_micros(), cpu.map_or("-".into(), |n| n.to_string()),
        totals.peak, totals.requested, retained.map_or("-".into(), |n| n.to_string()));
    for (phase, elapsed, counters) in &phases[..count] {
        println!("phase\t{phase}\t{}\t{}\t{}", elapsed.as_micros(), counters.net, counters.peak);
    }
}
