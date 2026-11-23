//! Window tasks and foreground rendering callbacks.
mod user_timing;

use super::{
    Shared, arg, diagnostic, drain_microtask_queue, event_target, fatal, getter, unsupported,
    webidl,
};
use rust_browser_javascript::{JsError, JsString, RootId, Value, Vm};
use std::{
    cell::Cell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_TIMERS: usize = 1024;
const MAX_FRAME_CALLBACKS: usize = 1024;
const FRAME_PERIOD: Duration = Duration::from_micros(16_667);

enum Handler {
    Callable(Value),
    Source(JsString),
}

struct Timer {
    id: u32,
    deadline: Instant,
    nesting: u32,
    interval: Option<Duration>,
    internal: bool,
    handler: Handler,
    args: Vec<Value>,
    roots: Vec<RootId>,
}

impl Timer {
    fn release(self, vm: &mut Vm) {
        for root in self.roots {
            vm.unroot(root);
        }
    }
}

struct ActiveTimer {
    id: u32,
    nesting: u32,
    cancelled: bool,
    internal: bool,
}

struct Frame {
    id: u32,
    callback: RootId,
}

#[derive(Clone, Copy)]
struct Clock {
    origin: Instant,
    epoch_ms: f64,
}

impl Clock {
    fn now(self, instant: Instant) -> f64 {
        // Use 100 microsecond resolution in ordinary browser realms.
        (instant.saturating_duration_since(self.origin).as_micros() / 100) as f64 / 10.
    }

    fn epoch_at(self, instant: Instant) -> f64 {
        (self.epoch_ms + self.now(instant)).floor().max(0.)
    }
}

struct NavigationTiming {
    clock: Clock,
    response_start: Cell<Option<Instant>>,
}

impl NavigationTiming {
    fn timestamp(&self, name: &str) -> f64 {
        let instant = match name {
            "navigationStart" => Some(self.clock.origin),
            "responseStart" => self.response_start.get(),
            _ => None,
        };
        instant.map_or(0., |instant| self.clock.epoch_at(instant))
    }
}

pub(super) struct Scheduler {
    clock: Clock,
    navigation_timing: Rc<NavigationTiming>,
    timers: Vec<Timer>,
    current: Option<ActiveTimer>,
    next_id: u32,
    foreground: bool,
    frames: VecDeque<Frame>,
    rendering: VecDeque<Frame>,
    frame_deadline: Option<Instant>,
    last_frame: Option<Instant>,
}

impl Scheduler {
    pub(super) fn new(started: Instant) -> Self {
        let wall = SystemTime::now().duration_since(UNIX_EPOCH);
        let wall_ms = wall.map_or_else(
            |error| -error.duration().as_secs_f64() * 1000.,
            |time| time.as_secs_f64() * 1000.,
        );
        let epoch_ms = wall_ms - started.elapsed().as_secs_f64() * 1000.;
        let clock = Clock {
            origin: started,
            epoch_ms,
        };
        Self {
            clock,
            navigation_timing: Rc::new(NavigationTiming {
                clock,
                response_start: Cell::new(None),
            }),
            timers: Vec::new(),
            current: None,
            next_id: 1,
            foreground: true,
            frames: VecDeque::new(),
            rendering: VecDeque::new(),
            frame_deadline: None,
            last_frame: None,
        }
    }

    pub(super) fn note_response_start(&self, instant: Instant) {
        self.navigation_timing.response_start.set(Some(instant));
    }

    fn id(&mut self) -> Result<u32, JsError> {
        let id = self.next_id;
        if id > i32::MAX as u32 {
            return Err(unsupported("Window callback identifiers exhausted"));
        }
        self.next_id += 1;
        Ok(id)
    }

    fn timer(
        &mut self,
        vm: &mut Vm,
        handler: Handler,
        args: Vec<Value>,
        requested: Duration,
        kind: TimerKind,
    ) -> Result<u32, JsError> {
        if self.timers.len() + usize::from(self.current.is_some()) >= MAX_TIMERS {
            return Err(unsupported("Timer budget exceeded"));
        }
        let parent = self.current.as_ref().map_or(0, |timer| timer.nesting);
        let internal = matches!(kind, TimerKind::Internal);
        let delay = if internal {
            requested
        } else {
            timer_delay(requested, parent)
        };
        let deadline = Instant::now()
            .checked_add(delay)
            .ok_or_else(|| unsupported("Timer deadline exceeds the available clock range"))?;
        let id = self.id()?;
        let callback = match &handler {
            Handler::Callable(value) => value.clone(),
            Handler::Source(text) => Value::String(text.clone()),
        };
        let mut roots = Vec::with_capacity(args.len() + 1);
        roots.push(vm.root(callback));
        roots.extend(args.iter().cloned().map(|arg| vm.root(arg)));
        self.timers.push(Timer {
            id,
            deadline,
            nesting: if internal {
                0
            } else {
                parent.saturating_add(1)
            },
            interval: matches!(kind, TimerKind::Interval).then_some(delay),
            internal,
            handler,
            args,
            roots,
        });
        Ok(id)
    }

    fn clear_timer(&mut self, vm: &mut Vm, id: u32) {
        if let Some(active) = &mut self.current
            && active.id == id
            && !active.internal
        {
            active.cancelled = true;
        }
        if let Some(index) = self
            .timers
            .iter()
            .position(|timer| timer.id == id && !timer.internal)
        {
            self.timers.remove(index).release(vm);
        }
    }

    fn next_timer(&mut self, now: Instant) -> Option<Timer> {
        let (index, _) = self
            .timers
            .iter()
            .enumerate()
            .filter(|(_, timer)| timer.deadline <= now)
            .min_by_key(|(_, timer)| (timer.deadline, timer.id))?;
        let timer = self.timers.remove(index);
        self.current = Some(ActiveTimer {
            id: timer.id,
            nesting: timer.nesting,
            cancelled: false,
            internal: timer.internal,
        });
        Some(timer)
    }

    fn complete_timer(&mut self, vm: &mut Vm, mut timer: Timer, failed: bool) {
        let current = self.current.take().expect("active timer callback");
        if !failed
            && !current.cancelled
            && let Some(interval) = timer.interval
        {
            let delay = timer_delay(interval, timer.nesting);
            if let Some(deadline) = Instant::now().checked_add(delay) {
                timer.deadline = deadline;
                timer.nesting = timer.nesting.saturating_add(1);
                timer.interval = Some(delay);
                self.timers.push(timer);
                return;
            }
        }
        timer.release(vm);
    }

    fn request_frame(&mut self, vm: &mut Vm, callback: Value) -> Result<u32, JsError> {
        if self.frames.len() + self.rendering.len() >= MAX_FRAME_CALLBACKS {
            return Err(unsupported("Animation frame callback budget exceeded"));
        }
        let id = self.id()?;
        self.frames.push_back(Frame {
            id,
            callback: vm.root(callback),
        });
        self.ensure_frame_deadline();
        Ok(id)
    }

    fn cancel_frame(&mut self, vm: &mut Vm, id: u32) {
        for queue in [&mut self.frames, &mut self.rendering] {
            if let Some(index) = queue.iter().position(|frame| frame.id == id) {
                vm.unroot(queue.remove(index).unwrap().callback);
                break;
            }
        }
        if self.frames.is_empty() {
            self.frame_deadline = None;
        }
    }

    fn ensure_frame_deadline(&mut self) {
        if self.foreground && !self.frames.is_empty() && self.frame_deadline.is_none() {
            self.frame_deadline = Some(Instant::now() + FRAME_PERIOD);
        }
    }

    pub(super) fn set_foreground(&mut self, foreground: bool) {
        self.foreground = foreground;
        if foreground {
            self.ensure_frame_deadline();
        } else {
            self.frame_deadline = None;
        }
    }

    fn begin_frame(&mut self, now: Instant, needs_rendering: bool) -> Option<f64> {
        if self
            .rendering_deadline(now, needs_rendering)
            .is_none_or(|deadline| deadline > now)
        {
            return None;
        }
        self.frame_deadline = None;
        self.last_frame = Some(now);
        std::mem::swap(&mut self.rendering, &mut self.frames);
        Some(self.clock.now(now))
    }

    pub(super) fn next_deadline(&self, needs_rendering: bool) -> Option<Instant> {
        self.timers
            .iter()
            .map(|timer| timer.deadline)
            .chain(self.rendering_deadline(Instant::now(), needs_rendering))
            .min()
    }

    fn rendering_deadline(&self, now: Instant, needs_rendering: bool) -> Option<Instant> {
        if !self.foreground {
            return None;
        }
        self.frame_deadline.or_else(|| {
            needs_rendering.then(|| self.last_frame.map_or(now, |last| last + FRAME_PERIOD))
        })
    }
}

fn timer_delay(requested: Duration, parent_nesting: u32) -> Duration {
    if parent_nesting > 5 {
        requested.max(Duration::from_millis(4))
    } else {
        requested
    }
}

#[derive(Clone, Copy)]
enum TimerKind {
    Timeout,
    Interval,
    Internal,
}

pub(super) fn schedule_timer(
    vm: &mut Vm,
    state: &Shared,
    callback: Value,
    arguments: Vec<Value>,
    delay: Duration,
) -> Result<u32, JsError> {
    state.borrow_mut().scheduler.timer(
        vm,
        Handler::Callable(callback),
        arguments,
        delay,
        TimerKind::Internal,
    )
}

fn idl_long(vm: &mut Vm, value: &Value) -> Result<i32, JsError> {
    let number = vm.to_number(value)?;
    Ok(if number.is_finite() {
        number.trunc().rem_euclid(4_294_967_296.) as u32 as i32
    } else {
        0
    })
}

pub(super) fn install_timers(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    let window = vm.global();
    for (name, kind) in [
        ("setTimeout", TimerKind::Timeout),
        ("setInterval", TimerKind::Interval),
    ] {
        let state = state.clone();
        webidl::operation(vm, window.clone(), name, 1, move |vm, _, args| {
            webidl::required(args, 1, name)?;
            let (handler, arguments) = if vm.is_callable(&args[0]) {
                (
                    Handler::Callable(args[0].clone()),
                    args.get(2..).unwrap_or_default().to_vec(),
                )
            } else {
                (Handler::Source(vm.to_string(&args[0])?), Vec::new())
            };
            let delay = idl_long(vm, &arg(args, 1))?.max(0) as u64;
            let id = state.borrow_mut().scheduler.timer(
                vm,
                handler,
                arguments,
                Duration::from_millis(delay),
                kind,
            )?;
            Ok(id.into())
        })?;
    }
    for name in ["clearTimeout", "clearInterval"] {
        let state = state.clone();
        webidl::operation(vm, window.clone(), name, 0, move |vm, _, args| {
            let id = idl_long(vm, &arg(args, 0))?;
            if id > 0 {
                state.borrow_mut().scheduler.clear_timer(vm, id as u32);
            }
            Ok(Value::Undefined)
        })?;
    }
    let owner = state.clone();
    webidl::operation(
        vm,
        window.clone(),
        "requestAnimationFrame",
        1,
        move |vm, _, args| {
            let callback = arg(args, 0);
            if !vm.is_callable(&callback) {
                return Err(JsError::type_error(
                    "requestAnimationFrame requires a callable callback",
                ));
            }
            Ok(owner
                .borrow_mut()
                .scheduler
                .request_frame(vm, callback)?
                .into())
        },
    )?;
    let owner = state.clone();
    webidl::operation(
        vm,
        window.clone(),
        "cancelAnimationFrame",
        1,
        move |vm, _, args| {
            webidl::required(args, 1, "cancelAnimationFrame")?;
            let id = idl_long(vm, &args[0])?;
            if id > 0 {
                owner.borrow_mut().scheduler.cancel_frame(vm, id as u32);
            }
            Ok(Value::Undefined)
        },
    )?;
    install_queue_microtask(vm, window)
}

pub(super) fn install_queue_microtask(vm: &mut Vm, target: Value) -> Result<(), JsError> {
    webidl::operation(vm, target, "queueMicrotask", 1, |vm, _, args| {
        vm.enqueue_microtask(arg(args, 0))?;
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn clock(vm: &Vm, receiver: &Value) -> Result<Clock, JsError> {
    vm.host_data::<Clock>(receiver)
        .map(|clock| *clock)
        .ok_or_else(|| JsError::type_error("Illegal Performance receiver"))
}

fn install_navigation_timing(vm: &mut Vm, state: &Shared) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    webidl::interface(
        vm,
        prototype.clone(),
        "PerformanceTiming",
        0,
        |_, _, _, _| Err(JsError::type_error("Illegal PerformanceTiming constructor")),
    )?;
    for name in ["navigationStart", "responseStart"] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let timing = vm
                .host_data::<NavigationTiming>(&this)
                .ok_or_else(|| JsError::type_error("Illegal PerformanceTiming receiver"))?;
            Ok(timing.timestamp(name).into())
        })?;
    }
    let timing = vm.new_object_with_prototype(prototype)?;
    vm.set_host_data(
        timing.clone(),
        state.borrow().scheduler.navigation_timing.clone(),
    )?;
    Ok(timing)
}

pub(super) fn install_performance(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    let base = vm.get_global("EventTarget")?;
    let parent = vm.get_property(base.clone(), "prototype")?;
    let proto = vm.new_object_with_prototype(parent)?;
    let class = webidl::interface(vm, proto.clone(), "Performance", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Performance constructor"))
    })?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype, Value::Undefined, &[class, base])?;
    webidl::operation(vm, proto.clone(), "now", 0, |vm, this, _| {
        Ok(clock(vm, &this)?.now(Instant::now()).into())
    })?;
    getter(vm, proto.clone(), "timeOrigin", |vm, this, _| {
        Ok(clock(vm, &this)?.epoch_ms.into())
    })?;
    let timing = install_navigation_timing(vm, state)?;
    let timing_root = vm.root(timing);
    getter(vm, proto.clone(), "timing", move |vm, this, _| {
        clock(vm, &this)?;
        Ok(vm
            .root_value(timing_root)
            .expect("rooted navigation timing object"))
    })?;
    webidl::operation(vm, proto.clone(), "toJSON", 0, |vm, this, _| {
        let time_origin = clock(vm, &this)?.epoch_ms;
        let result = vm.new_object();
        vm.set_property(result.clone(), "timeOrigin", time_origin.into())?;
        Ok(result)
    })?;
    user_timing::install(
        vm,
        &proto,
        state.borrow().scheduler.navigation_timing.clone(),
    )?;
    let result = vm.new_object_with_prototype(proto)?;
    event_target::attach(vm, result.clone())?;
    vm.set_host_data(
        result.clone(),
        std::rc::Rc::new(state.borrow().scheduler.clock),
    )?;
    vm.set_global("performance", result)
}

fn report_callback(state: &Shared, result: Result<Value, JsError>) -> Result<(), JsError> {
    match result {
        Err(error) if fatal(&error) => Err(error),
        Err(error) => {
            diagnostic(state, error.to_string());
            Ok(())
        }
        Ok(_) => Ok(()),
    }
}

pub(super) fn run_timers(vm: &mut Vm, state: &Shared) {
    for _ in 0..32 {
        let timer = state.borrow_mut().scheduler.next_timer(Instant::now());
        let Some(timer) = timer else { break };
        let (window, url) = {
            let state = state.borrow();
            (state.window.clone(), state.url.to_string())
        };
        let result = vm.with_task(|vm| {
            let result = match &timer.handler {
                Handler::Callable(callback) => vm.call(callback.clone(), window, &timer.args),
                Handler::Source(code) => vm.eval_with_identifier(&code.to_string_lossy(), &url),
            };
            report_callback(state, result)?;
            drain_microtask_queue(vm, state)
        });
        state
            .borrow_mut()
            .scheduler
            .complete_timer(vm, timer, result.is_err());
        if let Err(error) = result {
            diagnostic(state, error.to_string());
            break;
        }
    }
}

pub(super) fn run_frames(vm: &mut Vm, state: &Shared, needs_rendering: bool) -> Option<f64> {
    let timestamp = state
        .borrow_mut()
        .scheduler
        .begin_frame(Instant::now(), needs_rendering)?;
    loop {
        let frame = state.borrow_mut().scheduler.rendering.pop_front();
        let Some(frame) = frame else { break };
        let callback = vm.unroot(frame.callback).unwrap();
        let result = vm.with_task(|vm| {
            let result = vm.call(callback, Value::Undefined, &[timestamp.into()]);
            report_callback(state, result)?;
            drain_microtask_queue(vm, state)
        });
        if let Err(error) = result {
            diagnostic(state, error.to_string());
        }
    }
    Some(timestamp)
}
