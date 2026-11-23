//! Callable timers for the worker event loop; all roots stay in its VM.
use super::scope;
use crate::script::{arg, fatal, webidl};
use rust_browser_javascript::{ErrorKind, JsError, RootId, Value, Vm};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_TIMERS: usize = 1024;
const MAX_TIMER_ARGUMENTS: usize = 128;

struct Timer {
    id: u32,
    deadline: Instant,
    nesting: u32,
    repeat: Option<Duration>,
    roots: Vec<RootId>,
}

struct Running {
    id: u32,
    nesting: u32,
    cancelled: bool,
}

#[derive(Default)]
struct Queue {
    next_id: u32,
    pending: Vec<Timer>,
    running: Option<Running>,
}

#[derive(Clone, Default)]
pub(super) struct Timers(Rc<RefCell<Queue>>);

impl Timers {
    pub(super) fn install(vm: &mut Vm, global: Value) -> Result<Self, JsError> {
        let timers = Self::default();
        for (name, repeat) in [("setTimeout", false), ("setInterval", true)] {
            let timers = timers.clone();
            webidl::operation(vm, global.clone(), name, 1, move |vm, receiver, args| {
                scope(vm, &receiver)?;
                webidl::required(args, 1, name)?;
                if !vm.is_callable(&args[0]) {
                    // This subset does not compile string handlers.
                    return Err(JsError::type_error(
                        "Worker timer requires a callable handler",
                    ));
                }
                let millis = long(vm, &arg(args, 1))?.max(0) as u64;
                let arguments = args.get(2..).unwrap_or_default();
                timers
                    .schedule(
                        vm,
                        &args[0],
                        arguments,
                        Duration::from_millis(millis),
                        repeat,
                    )
                    .map(Value::from)
            })?;
        }
        for name in ["clearTimeout", "clearInterval"] {
            let timers = timers.clone();
            webidl::operation(vm, global.clone(), name, 0, move |vm, receiver, args| {
                scope(vm, &receiver)?;
                let id = long(vm, &arg(args, 0))?;
                if id > 0 {
                    timers.clear(vm, id as u32);
                }
                Ok(Value::Undefined)
            })?;
        }
        Ok(timers)
    }

    fn schedule(
        &self,
        vm: &mut Vm,
        handler: &Value,
        arguments: &[Value],
        delay: Duration,
        repeat: bool,
    ) -> Result<u32, JsError> {
        let mut queue = self.0.borrow_mut();
        if queue.pending.len() + usize::from(queue.running.is_some()) >= MAX_TIMERS
            || arguments.len() > MAX_TIMER_ARGUMENTS
        {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Worker timer budget exhausted",
            ));
        }
        vm.charge_work(arguments.len() + 1)?;
        let parent = queue.running.as_ref().map_or(0, |timer| timer.nesting);
        let deadline = deadline(delay, parent)?;
        let id = queue.next_id + 1;
        if id > i32::MAX as u32 {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Worker timer identifiers exhausted",
            ));
        }
        queue.next_id = id;
        let roots = std::iter::once(handler)
            .chain(arguments)
            .map(|value| vm.root(value.clone()))
            .collect();
        queue.pending.push(Timer {
            id,
            deadline,
            nesting: parent.saturating_add(1),
            repeat: repeat.then_some(delay),
            roots,
        });
        Ok(id)
    }

    fn clear(&self, vm: &mut Vm, id: u32) {
        let mut queue = self.0.borrow_mut();
        if let Some(running) = &mut queue.running
            && running.id == id
        {
            running.cancelled = true;
        }
        if let Some(index) = queue.pending.iter().position(|timer| timer.id == id) {
            release(vm, queue.pending.swap_remove(index));
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.0
            .borrow()
            .pending
            .iter()
            .map(|timer| timer.deadline)
            .min()
    }

    pub(super) fn fire(&self, vm: &mut Vm, now: Instant) -> Result<(), JsError> {
        let mut timer = {
            let mut queue = self.0.borrow_mut();
            let Some((index, _)) = queue
                .pending
                .iter()
                .enumerate()
                .filter(|(_, timer)| timer.deadline <= now)
                .min_by_key(|(_, timer)| (timer.deadline, timer.id))
            else {
                return Ok(());
            };
            let timer = queue.pending.swap_remove(index);
            queue.running = Some(Running {
                id: timer.id,
                nesting: timer.nesting,
                cancelled: false,
            });
            timer
        };
        let handler = vm.root_value(timer.roots[0]).expect("rooted timer handler");
        let args: Vec<_> = timer.roots[1..]
            .iter()
            .map(|&root| vm.root_value(root).expect("rooted timer argument"))
            .collect();
        let result = vm.call(handler, vm.global(), &args).map(|_| ());
        let mut queue = self.0.borrow_mut();
        let running = queue.running.take().expect("running worker timer");
        // Reschedule before the checkpoint, so a microtask can clear this
        // interval, and an ordinary JS exception does not stop the interval.
        if !running.cancelled
            && !result.as_ref().is_err_and(fatal)
            && let Some(delay) = timer.repeat
            && let Ok(next) = deadline(delay, timer.nesting)
        {
            timer.deadline = next;
            timer.nesting = timer.nesting.saturating_add(1);
            queue.pending.push(timer);
        } else {
            release(vm, timer);
        }
        result
    }
}

fn deadline(requested: Duration, parent_nesting: u32) -> Result<Instant, JsError> {
    let delay = if parent_nesting > 5 {
        requested.max(Duration::from_millis(4))
    } else {
        requested
    };
    Instant::now()
        .checked_add(delay)
        .ok_or_else(|| JsError::type_error("Timer deadline exceeds the available clock range"))
}

fn release(vm: &mut Vm, timer: Timer) {
    for root in timer.roots {
        vm.unroot(root);
    }
}

fn long(vm: &mut Vm, value: &Value) -> Result<i32, JsError> {
    let number = vm.to_number(value)?;
    Ok(if number.is_finite() {
        number.trunc().rem_euclid(4_294_967_296.) as u32 as i32
    } else {
        0
    })
}
