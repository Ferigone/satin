//! Exclusive work and thread-cycle counters for the diagnostic Windows binary.
//! No JS data is captured. The outer classic script owns any synchronous reentry.
use std::{cell::RefCell, time::Instant};
use windows_sys::Win32::System::{
    Threading::GetCurrentThread, WindowsProgramming::QueryThreadCycleTime,
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Area {
    Other,
    Compile,
    RegexpCompile,
    RegexpMatch,
    Property,
    Host,
}

const COUNT: usize = 6;

/// Entries are ordered as in `Area`. A missing OS clock leaves that metric absent.
#[derive(Default)]
pub struct Measurement {
    pub wall_us: u64,
    pub cpu_us: Option<u64>,
    pub cycles: Option<[u64; COUNT]>,
    pub work: [u64; COUNT],
}

struct Active {
    area: Area,
    last: Option<u64>,
    sample: Measurement,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

fn cycles() -> Option<u64> {
    let mut value = 0;
    // SAFETY: the pseudo-handle is for this thread and value is writable.
    (unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut value) } != 0).then_some(value)
}

impl Active {
    fn transition(&mut self, area: Area) {
        let now = cycles();
        if let (Some(delta), Some(totals)) = (
            self.last.zip(now).and_then(|(a, b)| b.checked_sub(a)),
            &mut self.sample.cycles,
        ) {
            totals[self.area as usize] += delta;
        } else {
            self.sample.cycles = None;
        }
        self.last = now;
        self.area = area;
    }
}

pub(crate) struct Scope(Option<Area>);

pub(crate) fn enter(area: Area) -> Scope {
    Scope(ACTIVE.with_borrow_mut(|state| {
        let active = state.as_mut()?;
        let previous = active.area;
        if previous == area {
            return None;
        }
        active.transition(area);
        Some(previous)
    }))
}

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(area) = self.0 {
            ACTIVE.with_borrow_mut(|state| {
                if let Some(active) = state {
                    active.transition(area);
                }
            });
        }
    }
}

pub(crate) fn charge(units: u64) {
    ACTIVE.with_borrow_mut(|state| {
        if let Some(active) = state {
            active.sample.work[active.area as usize] += units;
        }
    });
}

pub(crate) fn host<F>() -> Scope {
    if std::any::type_name::<F>().starts_with("rust_browser_engine::") {
        enter(Area::Host)
    } else {
        Scope(None)
    }
}

pub fn measure<T>(run: impl FnOnce() -> T) -> (T, Option<Measurement>) {
    if ACTIVE.with_borrow(Option::is_some) {
        return (run(), None);
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACTIVE.with_borrow_mut(Option::take);
        }
    }
    let wall = Instant::now();
    let cpu = super::task_deadline::thread_cpu_time();
    let last = cycles();
    ACTIVE.replace(Some(Active {
        area: Area::Other,
        last,
        sample: Measurement {
            cycles: last.map(|_| [0; COUNT]),
            ..Default::default()
        },
    }));
    let _reset = Reset;
    let result = run();
    let mut active = ACTIVE.take().expect("outer profile");
    active.transition(Area::Other);
    active.sample.cpu_us = cpu
        .zip(super::task_deadline::thread_cpu_time())
        .and_then(|(a, b)| b.checked_sub(a))
        .map(|t| t.as_micros() as u64);
    active.sample.wall_us = wall.elapsed().as_micros() as u64;
    (result, Some(active.sample))
}
