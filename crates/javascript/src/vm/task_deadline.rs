//! Cooperative time limits are checked on the VM thread at interpreter and host checkpoints.
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

pub(super) enum TaskDeadline {
    Wall(Instant),
    Thread {
        cpu_deadline: Duration,
        cpu_error: &'static str,
        wall_deadline: Instant,
        next_check: Cell<Instant>,
    },
}

impl TaskDeadline {
    pub(super) fn for_thread(cpu_budget: Duration, wall_deadline: Instant) -> Self {
        let now = Instant::now();
        let check = now
            .checked_add(cpu_budget)
            .unwrap_or(wall_deadline)
            .min(wall_deadline);
        match thread_cpu_time().and_then(|start| start.checked_add(cpu_budget)) {
            Some(cpu_deadline) => Self::Thread {
                cpu_deadline,
                cpu_error: "Task CPU budget exceeded",
                wall_deadline,
                next_check: Cell::new(check),
            },
            None => Self::Wall(check),
        }
    }

    pub(super) fn for_child(&self) -> Option<Self> {
        let Self::Thread {
            cpu_deadline,
            wall_deadline,
            ..
        } = self
        else {
            return None;
        };
        let now = Instant::now();
        let remaining = cpu_deadline.saturating_sub(thread_cpu_time()?);
        let reserved = remaining / 4;
        Some(Self::Thread {
            cpu_deadline: *cpu_deadline - reserved,
            cpu_error: "Child script CPU budget exceeded",
            wall_deadline: *wall_deadline,
            next_check: Cell::new(
                now.checked_add(remaining - reserved)
                    .unwrap_or(*wall_deadline)
                    .min(*wall_deadline),
            ),
        })
    }

    pub(super) fn exceeded(&self) -> Option<&'static str> {
        let now = Instant::now();
        match self {
            Self::Wall(deadline) => (now >= *deadline).then_some("Task deadline exceeded"),
            Self::Thread {
                cpu_deadline,
                cpu_error,
                wall_deadline,
                next_check,
            } => {
                if now < next_check.get() {
                    return None;
                }
                if now >= *wall_deadline {
                    return Some("Task deadline exceeded");
                }
                let Some(cpu) = thread_cpu_time() else {
                    return Some("Task deadline exceeded");
                };
                if cpu >= *cpu_deadline {
                    return Some(cpu_error);
                }
                // This thread cannot consume its remaining CPU allowance in less
                // elapsed time. Avoid an OS CPU-clock query on ordinary VM checks.
                next_check.set(
                    now.checked_add(*cpu_deadline - cpu)
                        .unwrap_or(*wall_deadline)
                        .min(*wall_deadline),
                );
                None
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn thread_cpu_time() -> Option<Duration> {
    let mut ts = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime initializes this timespec on success.
    let ts = unsafe {
        if libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, ts.as_mut_ptr()) != 0 {
            return None;
        }
        ts.assume_init()
    };
    Some(Duration::new(
        ts.tv_sec.try_into().ok()?,
        ts.tv_nsec.try_into().ok()?,
    ))
}

#[cfg(windows)]
pub(super) fn thread_cpu_time() -> Option<Duration> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentThread, GetThreadTimes},
    };
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut times = [zero; 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the current-thread pseudo-handle is valid during this call and
    // all four output pointers refer to distinct writable FILETIMEs.
    if unsafe { GetThreadTimes(GetCurrentThread(), created, exited, kernel, user) } == 0 {
        return None;
    }
    let ticks = |t: &FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    let total = ticks(kernel).checked_add(ticks(user))?;
    Some(Duration::new(
        total / 10_000_000,
        (total % 10_000_000) as u32 * 100,
    ))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn thread_cpu_time() -> Option<Duration> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, Value, Vm};

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn busy_callback_exhausts_cpu_even_with_fuel_and_wall_time_left() {
        let mut vm = Vm::new();
        let wall_deadline = Instant::now() + Duration::from_secs(15);
        let end = vm
            .with_task_cpu_budget(u64::MAX, Duration::from_millis(20), wall_deadline, |vm| {
                vm.eval("while (true) {}")
            })
            .unwrap_err();
        assert_eq!(end.kind, ErrorKind::ExecutionLimit);
        assert_eq!(end.message, "Task CPU budget exceeded");
        assert!(Instant::now() < wall_deadline);
        assert_eq!(vm.eval("1").unwrap(), Value::Number(1.));
    }

    #[test]
    fn emergency_wall_deadline_bounds_a_thread_with_cpu_time_remaining() {
        let mut vm = Vm::new();
        let error = vm
            .with_task_cpu_budget(u64::MAX, Duration::from_secs(2), Instant::now(), |vm| {
                vm.charge_work(1)
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ExecutionLimit);
        assert_eq!(error.message, "Task deadline exceeded");
    }
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn an_exhausted_outer_cpu_budget_is_not_a_recoverable_child_error() {
        let mut vm = Vm::new();
        let wall_deadline = Instant::now() + Duration::from_secs(15);
        vm.with_task_cpu_budget(u64::MAX, Duration::ZERO, wall_deadline, |vm| {
            let (result, recoverable) = vm.eval_embedded_script("0", "child");
            assert_eq!(result.unwrap_err().kind, ErrorKind::ExecutionLimit);
            assert!(!recoverable);
            assert_eq!(
                vm.charge_work(0).unwrap_err().message,
                "Task CPU budget exceeded"
            );
            Ok(())
        })
        .unwrap();
    }
}
