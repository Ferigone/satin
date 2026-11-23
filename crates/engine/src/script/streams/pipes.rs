//! Native pipe orchestration over the private readable and writable endpoints.
use super::{
    CLOSED, Lock, Status, StreamHost, Value, Vm, abort, arg, capability, clear_slot,
    hold_in_flight, promise_record, settle, stream, stream_error, writable,
};
use crate::script::webidl::operation;
use rust_browser_javascript::{JsError, ObjectId};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const SOURCE: u32 = 0;
const DESTINATION: u32 = 1;
const READER: u32 = 2;
const WRITER: u32 = 3;
const RESULT: u32 = 4;
const SIGNAL: u32 = 5;
const REASON: u32 = 6;
const PENDING_READ: u32 = 7;

#[derive(Default)]
struct Options {
    prevent_abort: bool,
    prevent_cancel: bool,
    prevent_close: bool,
}

impl Options {
    fn parse(vm: &mut Vm, value: Value) -> Result<(Self, Value), JsError> {
        if value.is_nullish() {
            return Ok((Self::default(), Value::Undefined));
        }
        if value.as_object().is_none() {
            return Err(JsError::type_error("StreamPipeOptions must be an object"));
        }
        let prevent_abort = vm.get_property(value.clone(), "preventAbort")?.is_truthy();
        let prevent_cancel = vm.get_property(value.clone(), "preventCancel")?.is_truthy();
        let prevent_close = vm.get_property(value.clone(), "preventClose")?.is_truthy();
        let signal = vm.get_property(value, "signal")?;
        if !signal.is_undefined() {
            abort::require_signal(vm, &signal)?;
        }
        Ok((
            Self {
                prevent_abort,
                prevent_cancel,
                prevent_close,
            },
            signal,
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    SourceClosed,
    SourceError,
    DestinationClosed,
    DestinationError,
    Abort,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Copying,
    Draining(Stop),
    Acting(Stop),
    Finished,
}

#[derive(Clone, Copy)]
enum Reaction {
    SourceClosed,
    DestinationClosed,
    Ready,
    Read,
    Write,
    Action,
}

struct Pipe {
    id: ObjectId,
    options: Options,
    phase: Cell<Phase>,
    deferred: Cell<bool>,
    reading: Cell<bool>,
    writes: Cell<usize>,
    actions: Cell<usize>,
    subscription: RefCell<Option<abort::Subscription>>,
}

impl Pipe {
    fn object(&self) -> Value {
        Value::Object(self.id)
    }

    fn get(&self, vm: &Vm, slot: u32) -> Value {
        vm.host_slot(&self.object(), slot).unwrap_or_default()
    }
}

impl StreamHost {
    pub(in crate::script) fn request_proxy(
        &self,
        vm: &mut Vm,
        source: Value,
    ) -> Result<Value, JsError> {
        let constructor = self
            .0
            .transform
            .get()
            .and_then(|root| vm.root_value(root))
            .ok_or_else(|| JsError::type_error("Missing TransformStream intrinsic"))?;
        let identity = vm.construct(constructor, &[])?;
        let (readable, writable) = super::transform::endpoints(vm, &identity)?;
        let pipe = self.pipe(vm, source, writable, Options::default(), Value::Undefined)?;
        self.handled(vm, pipe)?;
        Ok(readable)
    }

    fn pipe(
        &self,
        vm: &mut Vm,
        source: Value,
        destination: Value,
        options: Options,
        signal: Value,
    ) -> Result<Value, JsError> {
        let readable = stream(vm, &source)?;
        if !matches!(readable.borrow().lock, Lock::Free) || writable::locked(vm, &destination)? {
            return Err(JsError::type_error("Cannot pipe a locked stream"));
        }
        let owner = vm.new_object_with_prototype(Value::Null)?;
        let (promise, record) = capability(vm, &owner)?;
        let pipe = Rc::new(Pipe {
            id: owner.as_object().unwrap(),
            options,
            phase: Cell::new(Phase::Copying),
            deferred: Cell::new(false),
            reading: Cell::new(false),
            writes: Cell::new(0),
            actions: Cell::new(0),
            subscription: RefCell::default(),
        });
        vm.set_host_data(owner.clone(), pipe.clone())?;
        let reader = self.acquire(vm, &readable)?;
        let writer = self.0.writable.get_writer(vm, destination.clone())?;
        readable.borrow_mut().disturbed = true;
        for (slot, value) in [
            (SOURCE, source),
            (DESTINATION, destination),
            (READER, reader.clone()),
            (WRITER, writer.clone()),
            (RESULT, record),
            (SIGNAL, signal.clone()),
        ] {
            vm.set_host_slot(owner.clone(), slot, value)?;
        }

        // The pipe and its reaction callbacks share traced links. A live
        // network stream also observes terminal events while awaiting ready.
        if readable.borrow().status == Status::Readable {
            readable.borrow_mut().observe_close = true;
            hold_in_flight(vm, &readable);
        }
        let record = vm.host_slot(&reader, CLOSED).unwrap_or_default();
        self.observe_pipe(
            vm,
            &pipe,
            promise_record(vm, &record),
            Reaction::SourceClosed,
        )?;
        self.observe_pipe(
            vm,
            &pipe,
            writable::writer_closed(vm, &writer)?,
            Reaction::DestinationClosed,
        )?;

        if !signal.is_undefined() {
            if let Some(reason) = abort::reason(vm, &signal) {
                self.stop_pipe(vm, &pipe, Stop::Abort, reason)?;
                return Ok(promise);
            }
            let weak = Rc::downgrade(&pipe);
            let host = self.clone();
            let subscription = abort::subscribe_traced(vm, &signal, owner, move |vm, reason| {
                if let Some(pipe) = weak.upgrade() {
                    host.stop_pipe(vm, &pipe, Stop::Abort, reason)?;
                }
                Ok(())
            });
            let subscription = match subscription {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = self.finish_pipe(vm, &pipe, Ok(Value::Undefined));
                    return Err(error);
                }
            };
            *pipe.subscription.borrow_mut() = Some(subscription);
        }
        if !self.check_pipe(vm, &pipe)? {
            self.await_pipe_ready(vm, &pipe)?;
        }
        Ok(promise)
    }

    fn observe_pipe(
        &self,
        vm: &mut Vm,
        pipe: &Rc<Pipe>,
        promise: Value,
        reaction: Reaction,
    ) -> Result<(), JsError> {
        let callback = |vm: &mut Vm, rejected| -> Result<Value, JsError> {
            let host = self.clone();
            let owner = Rc::downgrade(pipe);
            let function = vm.new_native_function("", move |vm, _, args| {
                if let Some(pipe) = owner.upgrade() {
                    host.pipe_reaction(vm, &pipe, reaction, rejected, arg(args, 0))?;
                }
                Ok(Value::Undefined)
            });
            vm.set_host_slot(function.clone(), 0, pipe.object())?;
            Ok(function)
        };
        let fulfilled = callback(vm, false)?;
        let rejected = callback(vm, true)?;
        self.then(vm, promise, fulfilled, rejected)?;
        Ok(())
    }

    fn check_pipe(&self, vm: &mut Vm, pipe: &Rc<Pipe>) -> Result<bool, JsError> {
        let source = pipe.get(vm, SOURCE);
        let destination = pipe.get(vm, DESTINATION);
        let status = stream(vm, &source)?.borrow().status;
        if status == Status::Errored {
            self.stop_pipe(vm, pipe, Stop::SourceError, stream_error(vm, &source))?;
        } else if let Some(reason) = writable::errored(vm, &destination)? {
            self.stop_pipe(vm, pipe, Stop::DestinationError, reason)?;
        } else if status == Status::Closed {
            self.stop_pipe(vm, pipe, Stop::SourceClosed, Value::Undefined)?;
        } else if writable::closing_or_closed(vm, &destination)? {
            let error = vm.error_value(&JsError::type_error(
                "Pipe destination is closing or closed",
            ))?;
            self.stop_pipe(vm, pipe, Stop::DestinationClosed, error)?;
        }
        Ok(pipe.phase.get() != Phase::Copying)
    }

    fn await_pipe_ready(&self, vm: &mut Vm, pipe: &Rc<Pipe>) -> Result<(), JsError> {
        let ready = writable::writer_ready(vm, &pipe.get(vm, WRITER))?;
        self.observe_pipe(vm, pipe, ready, Reaction::Ready)
    }

    fn pipe_reaction(
        &self,
        vm: &mut Vm,
        pipe: &Rc<Pipe>,
        reaction: Reaction,
        rejected: bool,
        value: Value,
    ) -> Result<(), JsError> {
        match (reaction, pipe.phase.get()) {
            (Reaction::Read, Phase::Copying | Phase::Draining(_)) => {
                self.pipe_read_result(vm, pipe, rejected, value)
            }
            (Reaction::Write, Phase::Copying | Phase::Draining(_)) => {
                pipe.writes.set(pipe.writes.get() - 1);
                if rejected
                    && pipe.phase.get() == Phase::Copying
                    && !self.check_pipe(vm, pipe)?
                    && writable::stream_error(vm, &pipe.get(vm, DESTINATION))?.is_none()
                {
                    self.stop_pipe(vm, pipe, Stop::DestinationError, value)?;
                }
                self.drain_pipe(vm, pipe)
            }
            (Reaction::Ready, Phase::Copying) => {
                if self.check_pipe(vm, pipe)? {
                    return Ok(());
                }
                if rejected {
                    // An erroring destination still owns its in-flight write.
                    // Its closed reaction propagates the eventual stored error.
                    return if writable::stream_error(vm, &pipe.get(vm, DESTINATION))?.is_some() {
                        Ok(())
                    } else {
                        self.stop_pipe(vm, pipe, Stop::DestinationError, value)
                    };
                }
                let source = stream(vm, &pipe.get(vm, SOURCE))?;
                pipe.reading.set(true);
                let read = self.read_internal(vm, &source)?;
                vm.set_host_slot(pipe.object(), PENDING_READ, read.clone())?;
                self.observe_pipe(vm, pipe, read, Reaction::Read)?;
                self.drain_pipe(vm, pipe)
            }
            (Reaction::SourceClosed | Reaction::DestinationClosed, Phase::Copying) => {
                self.check_pipe(vm, pipe)?;
                Ok(())
            }
            (Reaction::Action, Phase::Acting(stop)) => {
                if rejected {
                    return self.finish_pipe(vm, pipe, Err(value));
                }
                let remaining = pipe.actions.get() - 1;
                pipe.actions.set(remaining);
                if remaining == 0 {
                    self.finish_pipe(vm, pipe, Self::pipe_outcome(vm, pipe, stop))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn pipe_read_result(
        &self,
        vm: &mut Vm,
        pipe: &Rc<Pipe>,
        rejected: bool,
        result: Value,
    ) -> Result<(), JsError> {
        pipe.reading.set(false);
        clear_slot(vm, &pipe.object(), PENDING_READ)?;
        if rejected {
            if pipe.phase.get() == Phase::Copying {
                self.stop_pipe(vm, pipe, Stop::SourceError, result)?;
            }
        } else if vm.get_property(result.clone(), "done")?.is_truthy() {
            if pipe.phase.get() == Phase::Copying {
                self.check_pipe(vm, pipe)?;
            }
        } else if writable::can_write(vm, &pipe.get(vm, DESTINATION))? {
            let chunk = vm.get_property(result, "value")?;
            pipe.writes.set(pipe.writes.get() + 1);
            let write = writable::writer_write(vm, pipe.get(vm, WRITER), chunk)?;
            self.observe_pipe(vm, pipe, write, Reaction::Write)?;
            if pipe.phase.get() == Phase::Copying && !self.check_pipe(vm, pipe)? {
                self.await_pipe_ready(vm, pipe)?;
            }
        } else if pipe.phase.get() == Phase::Copying && !self.check_pipe(vm, pipe)? {
            let ready = writable::writer_ready(vm, &pipe.get(vm, WRITER))?;
            self.observe_pipe(vm, pipe, ready, Reaction::Ready)?;
        }
        self.drain_pipe(vm, pipe)
    }

    fn stop_pipe(
        &self,
        vm: &mut Vm,
        pipe: &Rc<Pipe>,
        stop: Stop,
        reason: Value,
    ) -> Result<(), JsError> {
        if pipe.phase.get() != Phase::Copying {
            return Ok(());
        }
        pipe.phase.set(Phase::Draining(stop));
        vm.set_host_slot(pipe.object(), REASON, reason)?;
        if writable::can_write(vm, &pipe.get(vm, DESTINATION))? {
            // Give an already-delivered read and queued writes the same
            // chance to finish as subsequent writes in the ordinary pump.
            pipe.deferred.set(true);
            let host = self.clone();
            let owner = Rc::downgrade(pipe);
            let drain = vm.new_native_function("", move |vm, _, _| {
                if let Some(pipe) = owner.upgrade() {
                    pipe.deferred.set(false);
                    host.drain_pipe(vm, &pipe)?;
                }
                Ok(Value::Undefined)
            });
            vm.set_host_slot(drain.clone(), 0, pipe.object())?;
            vm.enqueue_microtask(drain)
        } else {
            self.drain_pipe(vm, pipe)
        }
    }

    fn pipe_outcome(vm: &Vm, pipe: &Pipe, stop: Stop) -> Result<Value, Value> {
        if stop == Stop::SourceClosed {
            Ok(Value::Undefined)
        } else {
            Err(pipe.get(vm, REASON))
        }
    }

    fn drain_pipe(&self, vm: &mut Vm, pipe: &Rc<Pipe>) -> Result<(), JsError> {
        let Phase::Draining(stop) = pipe.phase.get() else {
            return Ok(());
        };
        if pipe.deferred.get() {
            return Ok(());
        }
        let destination = pipe.get(vm, DESTINATION);
        if writable::can_write(vm, &destination)?
            && (pipe.writes.get() > 0
                || (pipe.reading.get() && {
                    let read = pipe.get(vm, PENDING_READ);
                    // Undefined means JavaScript reentered while starting the read.
                    read.is_undefined() || vm.is_promise_fulfilled(&read)
                }))
        {
            return Ok(());
        }

        pipe.phase.set(Phase::Acting(stop));
        let source = stream(vm, &pipe.get(vm, SOURCE))?;
        let writer = pipe.get(vm, WRITER);
        let reason = pipe.get(vm, REASON);
        let opts = &pipe.options;
        if !opts.prevent_abort
            && (stop == Stop::SourceError
                || (stop == Stop::Abort && writable::is_writable(vm, &destination)?))
        {
            let action = writable::writer_abort(vm, writer.clone(), reason.clone());
            self.observe_pipe_action(vm, pipe, action)?;
        }
        if !opts.prevent_cancel
            && (matches!(stop, Stop::DestinationError | Stop::DestinationClosed)
                || (stop == Stop::Abort && source.borrow().status == Status::Readable))
        {
            let action = self.cancel(vm, &source, reason);
            self.observe_pipe_action(vm, pipe, action)?;
        }
        if stop == Stop::SourceClosed && !opts.prevent_close {
            let action = if writable::closing_or_closed(vm, &destination)? {
                Self::settled_pipe_action(vm, Ok(Value::Undefined))
            } else if let Some(reason) = writable::errored(vm, &destination)? {
                Self::settled_pipe_action(vm, Err(reason))
            } else {
                writable::writer_close(vm, writer)
            };
            self.observe_pipe_action(vm, pipe, action)?;
        }
        if stop == Stop::Abort && pipe.actions.get() == 0 {
            let action = Self::settled_pipe_action(vm, Ok(Value::Undefined));
            self.observe_pipe_action(vm, pipe, action)?;
        }
        if pipe.actions.get() == 0 {
            self.finish_pipe(vm, pipe, Self::pipe_outcome(vm, pipe, stop))?;
        }
        Ok(())
    }

    fn settled_pipe_action(vm: &mut Vm, result: Result<Value, Value>) -> Result<Value, JsError> {
        let capability = vm.new_promise_capability()?;
        let (callback, value) = match result {
            Ok(value) => (capability.resolve, value),
            Err(reason) => (capability.reject, reason),
        };
        vm.call(callback, Value::Undefined, &[value])?;
        Ok(capability.promise)
    }

    fn observe_pipe_action(
        &self,
        vm: &mut Vm,
        pipe: &Rc<Pipe>,
        action: Result<Value, JsError>,
    ) -> Result<(), JsError> {
        let action = match action {
            Ok(action) => action,
            Err(error) => Self::reject(vm, error)?,
        };
        pipe.actions.set(pipe.actions.get() + 1);
        self.observe_pipe(vm, pipe, action, Reaction::Action)
    }

    fn finish_pipe(
        &self,
        vm: &mut Vm,
        pipe: &Rc<Pipe>,
        result: Result<Value, Value>,
    ) -> Result<(), JsError> {
        pipe.phase.set(Phase::Finished);
        writable::writer_release(vm, &pipe.get(vm, WRITER))?;
        let source = stream(vm, &pipe.get(vm, SOURCE))?;
        self.release(vm, &source, &pipe.get(vm, READER))?;
        if let Some(subscription) = pipe.subscription.borrow_mut().take() {
            subscription.detach(vm)?;
        }
        let record = pipe.get(vm, RESULT);
        for key in [
            SOURCE,
            DESTINATION,
            READER,
            WRITER,
            RESULT,
            REASON,
            SIGNAL,
            PENDING_READ,
        ] {
            clear_slot(vm, &pipe.object(), key)?;
        }
        settle(vm, &record, result)
    }
}

pub(super) fn install(vm: &mut Vm, host: &StreamHost, prototype: Value) -> Result<(), JsError> {
    let shared = host.clone();
    operation(vm, prototype.clone(), "pipeTo", 1, move |vm, this, args| {
        let result = (|| {
            stream(vm, &this)?;
            let destination = arg(args, 0);
            if !writable::is_stream(vm, &destination) {
                return Err(JsError::type_error("pipeTo requires a WritableStream"));
            }
            let (options, signal) = Options::parse(vm, arg(args, 1))?;
            shared.pipe(vm, this, destination, options, signal)
        })();
        match result {
            Ok(promise) => Ok(promise),
            Err(error) => StreamHost::reject(vm, error),
        }
    })?;
    let shared = host.clone();
    operation(vm, prototype, "pipeThrough", 1, move |vm, this, args| {
        stream(vm, &this)?;
        let pair = arg(args, 0);
        if pair.as_object().is_none() {
            return Err(JsError::type_error(
                "pipeThrough requires a readable/writable pair",
            ));
        }
        let readable = vm.get_property(pair.clone(), "readable")?;
        stream(vm, &readable)?;
        let writable = vm.get_property(pair, "writable")?;
        if !writable::is_stream(vm, &writable) {
            return Err(JsError::type_error("pipeThrough requires a WritableStream"));
        }
        let (options, signal) = Options::parse(vm, arg(args, 1))?;
        let pipe = shared.pipe(vm, this, writable, options, signal)?;
        shared.handled(vm, pipe)?;
        Ok(readable)
    })?;
    Ok(())
}
