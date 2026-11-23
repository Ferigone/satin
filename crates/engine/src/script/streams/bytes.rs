//! Byte reads share the normal stream driver and add pull-into descriptors.
mod controls;
pub(super) mod tee;
#[cfg(test)]
mod tests;

use super::{
    Bytes, Item, MAX_READS, Shared, Source, Status, StreamHost, arg, capability, clear_slot,
    custom, exception, hold_in_flight, read_result, reader_stream_kind, release_if_idle, settle,
    stream_error, wants,
};
use rust_browser_javascript::{ArrayBufferViewKind, ErrorKind, JsError, RootId, Value, Vm};
use std::{ops::Range, sync::Arc};

pub(super) use controls::{controller, install};

const REQUEST: u32 = 17;
const BUFFER_SLOTS: u32 = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReaderKind {
    Default,
    Byob,
    Released,
    TeeDefault,
    TeeByob,
}

pub(super) struct Intrinsics {
    pub(super) reader: RootId,
    pub(super) controller: RootId,
    pub(super) request: RootId,
}

impl Intrinsics {
    pub(super) fn new(vm: &mut Vm) -> Self {
        let reader = vm.new_object();
        let controller = vm.new_object();
        let request = vm.new_object();
        Self {
            reader: vm.root(reader),
            controller: vm.root(controller),
            request: vm.root(request),
        }
    }
}

#[derive(Clone)]
pub(super) struct PullInto {
    slot: u32,
    buffer_length: usize,
    range: Range<usize>,
    kind: ArrayBufferViewKind,
    minimum: usize,
    filled: usize,
    reader: ReaderKind,
    loan: Option<tee::Loan>,
    lent: bool,
}

impl PullInto {
    fn new(
        buffer_length: usize,
        range: Range<usize>,
        kind: ArrayBufferViewKind,
        minimum: usize,
        reader: ReaderKind,
    ) -> Self {
        Self {
            slot: 0,
            buffer_length,
            range,
            kind,
            minimum,
            filled: 0,
            reader,
            loan: None,
            lent: false,
        }
    }

    fn buffer(&self, vm: &Vm, object: &Value) -> Value {
        vm.host_slot(object, self.slot).unwrap_or_default()
    }

    fn offset(&self) -> usize {
        self.range.start + self.filled
    }

    fn remainder(&self) -> usize {
        self.filled % self.kind.element_width()
    }
}

pub(super) fn unsigned_long_long(vm: &mut Vm, value: &Value) -> Result<u64, JsError> {
    let integer = vm.to_number(value)?.trunc();
    if !(0.0..18_446_744_073_709_551_616.0).contains(&integer) {
        return Err(JsError::type_error(
            "Value is outside unsigned long long range",
        ));
    }
    Ok(integer as u64)
}

fn range_error(message: &'static str) -> JsError {
    JsError::new(ErrorKind::RangeError, message)
}

fn append(vm: &mut Vm, source: &Shared, mut pull: PullInto, buffer: Value) -> Result<(), JsError> {
    let mut state = source.borrow_mut();
    // The released head can remain before up to MAX_READS active reads.
    pull.slot = state.pull_intos.back().map_or(BUFFER_SLOTS, |previous| {
        BUFFER_SLOTS + (previous.slot - BUFFER_SLOTS + 1) % (MAX_READS as u32 + 1)
    });
    vm.set_host_slot(state.value(), pull.slot, buffer)?;
    state.pull_intos.push_back(pull);
    Ok(())
}

fn shift(vm: &mut Vm, source: &Shared) -> Result<Option<(PullInto, Value)>, JsError> {
    let (object, pull) = {
        let mut state = source.borrow_mut();
        let Some(pull) = state.pull_intos.pop_front() else {
            return Ok(None);
        };
        (state.value(), pull)
    };
    let buffer = pull.buffer(vm, &object);
    clear_slot(vm, &object, pull.slot)?;
    Ok(Some((pull, buffer)))
}

fn head(source: &Shared) -> Option<PullInto> {
    source.borrow().pull_intos.front().cloned()
}

fn invalidate_request(vm: &mut Vm, object: &Value) -> Result<(), JsError> {
    if let Some(request) = vm
        .host_slot(object, REQUEST)
        .filter(|value| !value.is_undefined())
    {
        controls::invalidate(vm, &request)?;
        clear_slot(vm, object, REQUEST)?;
    }
    Ok(())
}

pub(super) fn clear(vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let object = source.borrow().value();
    invalidate_request(vm, &object)?;
    tee::clear_waiters(vm, &object)?;
    while shift(vm, source)?.is_some() {}
    Ok(())
}

pub(super) fn acquire(host: &StreamHost, vm: &mut Vm, source: &Shared) -> Result<Value, JsError> {
    if !source.borrow().bytes {
        return Err(JsError::type_error(
            "A BYOB reader requires a readable byte stream",
        ));
    }
    host.acquire_with(vm, source, host.0.bytes.reader, true)
}

fn args_read(
    host: &StreamHost,
    vm: &mut Vm,
    reader: Value,
    args: &[Value],
) -> Result<Value, JsError> {
    reader_stream_kind(vm, &reader, true)?;
    let view = arg(args, 0);
    // Check conversion before accessing the options dictionary, then get the
    // current view again since the min getter can transfer its buffer.
    vm.fixed_array_buffer_view(&view)?;
    let options = arg(args, 1);
    let minimum = if options.is_nullish() {
        1
    } else {
        if options.as_object().is_none() {
            return Err(JsError::type_error("BYOB read options must be an object"));
        }
        let minimum = vm.get_property(options, "min")?;
        if minimum.is_undefined() {
            1
        } else {
            unsigned_long_long(vm, &minimum)?
        }
    };
    let view = vm.fixed_array_buffer_view(&view)?;
    if view.byte_range.is_empty() || minimum == 0 {
        return Err(JsError::type_error(
            "The BYOB view and minimum must be nonempty",
        ));
    }
    let width = view.kind.element_width();
    if minimum > (view.byte_range.len() / width) as u64 {
        return Err(range_error("BYOB minimum exceeds the view length"));
    }
    let Some(source) = reader_stream_kind(vm, &reader, true)? else {
        return Err(JsError::type_error("ReadableStream reader was released"));
    };
    let (object, status, pending, reads) = {
        let mut state = source.borrow_mut();
        state.disturbed = true;
        (
            state.value(),
            state.status,
            !state.pull_intos.is_empty(),
            state.reads,
        )
    };
    if reads >= MAX_READS {
        return Err(JsError::type_error("Too many pending ReadableStream reads"));
    }
    let (promise, record) = capability(vm, &object)?;
    if status == Status::Errored {
        settle(vm, &record, Err(stream_error(vm, &object)))?;
        return Ok(promise);
    }
    let buffer = vm.transfer_array_buffer(&view.buffer)?;
    if status == Status::Closed && !pending {
        let start = view.byte_range.start;
        let result = vm.new_array_buffer_view(&buffer, start..start, view.kind)?;
        let result = read_result(vm, result, true)?;
        settle(vm, &record, Ok(result))?;
        return Ok(promise);
    }
    let pull = PullInto::new(
        vm.array_buffer_byte_length(&buffer)?,
        view.byte_range,
        view.kind,
        minimum as usize * width,
        ReaderKind::Byob,
    );
    append(vm, &source, pull, buffer)?;
    StreamHost::queue_read(vm, &source, record)?;
    hold_in_flight(vm, &source);
    host.advance(vm, &source)?;
    Ok(promise)
}

pub(super) fn auto_allocate(vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let size = {
        let state = source.borrow();
        if !state.queue.is_empty() {
            return Ok(());
        }
        state.auto_allocate
    };
    if let Some(size) = size {
        let size =
            usize::try_from(size).map_err(|_| range_error("Byte stream buffer is too large"))?;
        let buffer = vm.new_zeroed_array_buffer(size)?;
        let pull = PullInto::new(
            size,
            0..size,
            ArrayBufferViewKind::UINT8,
            1,
            ReaderKind::Default,
        );
        append(vm, source, pull, buffer)?;
    }
    Ok(())
}

struct ReadyRead {
    record: Value,
    view: Value,
    reader: ReaderKind,
}

fn take_ready_read(vm: &mut Vm, source: &Shared) -> Result<ReadyRead, JsError> {
    let pull = head(source).expect("pending pull-into descriptor");
    let buffer = pull.buffer(vm, &source.borrow().value());
    let view = vm.new_array_buffer_view(&buffer, pull.range.start..pull.offset(), pull.kind)?;
    shift(vm, source)?;
    let record = StreamHost::next_read(vm, source)?.expect("pending byte read");
    Ok(ReadyRead {
        record,
        view,
        reader: pull.reader,
    })
}

fn finish_ready_reads(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    ready: Vec<ReadyRead>,
    done: bool,
) -> Result<(), JsError> {
    release_if_idle(vm, source);
    for ReadyRead {
        record,
        view,
        reader,
    } in ready
    {
        if reader == ReaderKind::Default {
            host.finish_read(vm, source, &record, Ok(Some(view)))?;
        } else {
            let result = read_result(vm, view, done)?;
            settle(vm, &record, Ok(result))?;
        }
    }
    Ok(())
}

pub(super) fn incomplete_element(source: &Shared) -> bool {
    head(source).is_some_and(|pull| pull.remainder() != 0)
}

pub(super) fn is_pending_byob(source: &Shared) -> bool {
    source
        .borrow()
        .pull_intos
        .iter()
        .any(|pull| pull.reader == ReaderKind::Byob)
}

pub(super) fn finish_closed(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
) -> Result<(), JsError> {
    invalidate_request(vm, &source.borrow().value())?;
    let mut ready = Vec::new();
    let prepared = (|| {
        while let Some(pull) = head(source) {
            if pull.reader == ReaderKind::Byob {
                ready.push(take_ready_read(vm, source)?);
            } else {
                shift(vm, source)?;
            }
        }
        Ok(())
    })();
    finish_ready_reads(host, vm, source, ready, true)?;
    prepared
}

fn copy_to_queue(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    buffer: &Value,
    range: Range<usize>,
    front: bool,
) -> Result<(), JsError> {
    if range.is_empty() {
        return Ok(());
    }
    let bytes = vm.copy_array_buffer_range(buffer, range)?;
    let weight = bytes.len() as f64;
    let item = native_item(bytes);
    if front {
        host.enqueue_front(vm, source, item, weight)
    } else {
        host.enqueue(vm, source, item, weight)
    }
}

fn native_item(bytes: Vec<u8>) -> Item {
    Item::Bytes(Bytes {
        range: 0..bytes.len(),
        backing: Arc::new(bytes),
    })
}

pub(super) fn release(host: &StreamHost, vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
    let (custom, readable, object) = {
        let state = source.borrow();
        (
            matches!(state.source, Source::Custom(_)),
            state.status == Status::Readable,
            state.value(),
        )
    };
    if readable && (custom || head(source).is_some_and(|pull| pull.lent)) {
        let mut state = source.borrow_mut();
        if let Some(first) = state.pull_intos.front_mut() {
            first.reader = ReaderKind::Released;
        }
        let keep = usize::from(!state.pull_intos.is_empty());
        for pull in state.pull_intos.drain(keep..) {
            clear_slot(vm, &object, pull.slot)?;
        }
        return Ok(());
    }
    if readable && let Some(pull) = head(source) {
        let buffer = pull.buffer(vm, &object);
        copy_to_queue(
            host,
            vm,
            source,
            &buffer,
            pull.range.start..pull.offset(),
            true,
        )?;
    }
    clear(vm, source)
}

fn respond(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    buffer: Value,
    count: usize,
) -> Result<(), JsError> {
    let object = source.borrow().value();
    let next = vm.transfer_array_buffer(&buffer)?;
    invalidate_request(vm, &object)?;
    accept_response(host, vm, source, next, count)
}

fn accept_response(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    next: Value,
    count: usize,
) -> Result<(), JsError> {
    let pull = {
        let mut state = source.borrow_mut();
        let object = state.value();
        let current = state
            .pull_intos
            .front_mut()
            .expect("BYOB request has a pull-into descriptor");
        vm.set_host_slot(object, current.slot, next.clone())?;
        current.filled += count;
        current.lent = false;
        current.clone()
    };
    if matches!(pull.reader, ReaderKind::TeeDefault | ReaderKind::TeeByob) {
        return tee::responded(host, vm, source);
    }
    if source.borrow().status == Status::Closed {
        return finish_closed(host, vm, source);
    }
    let mut ready = Vec::new();
    if pull.reader == ReaderKind::Released {
        shift(vm, source)?;
        deliver_released(host, vm, source, &next, pull.range.start..pull.offset())?;
    } else if pull.filled >= pull.minimum {
        let remainder = pull.remainder();
        if remainder != 0 {
            copy_to_queue(
                host,
                vm,
                source,
                &next,
                pull.offset() - remainder..pull.offset(),
                true,
            )?;
            source.borrow_mut().pull_intos.front_mut().unwrap().filled -= remainder;
        }
        ready.push(take_ready_read(vm, source)?);
    }
    let prepared = drain_byob_queue(host, vm, source, &mut ready);
    finish_ready_reads(host, vm, source, ready, false)?;
    prepared?;
    host.advance(vm, source)
}

fn deliver_released(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    buffer: &Value,
    mut range: Range<usize>,
) -> Result<(), JsError> {
    // A response fills pending auto-allocated views. controller.enqueue(),
    // in contrast, passes the supplied view to a default reader directly.
    let mut ready = Vec::new();
    while !range.is_empty()
        && let Some(pull) = head(source).filter(|pull| pull.reader == ReaderKind::Default)
    {
        let target = pull.buffer(vm, &source.borrow().value());
        let count = range.len().min(pull.range.len());
        let end = range.start + count;
        vm.copy_into_array_buffer(buffer, range.start..end, &target, pull.range.start)?;
        let view = vm.new_array_buffer_view(
            &target,
            pull.range.start..pull.range.start + count,
            pull.kind,
        )?;
        shift(vm, source)?;
        let record = StreamHost::next_read(vm, source)?.expect("auto-allocated byte read");
        ready.push((record, view));
        range.start = end;
    }
    copy_to_queue(host, vm, source, buffer, range, false)?;
    release_if_idle(vm, source);
    for (record, view) in ready {
        host.finish_read(vm, source, &record, Ok(Some(view)))?;
    }
    Ok(())
}

pub(super) fn item_length(vm: &Vm, item: &Item) -> Result<usize, JsError> {
    match item {
        Item::Bytes(bytes) => Ok(bytes.range.len()),
        Item::Js(view) => Ok(vm.fixed_array_buffer_view(view)?.byte_range.len()),
    }
}

pub(super) fn clone_item(vm: &mut Vm, item: &Item) -> Result<Item, JsError> {
    match item {
        Item::Bytes(_) => Ok(item.clone()),
        Item::Js(view) => {
            let bytes = vm.copy_buffer_source(view)?.expect("byte chunk is a view");
            Ok(native_item(bytes))
        }
    }
}

pub(super) fn fill_read(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    item: Item,
) -> Result<Option<Item>, JsError> {
    let Some(pull) = head(source) else {
        return Ok(Some(item));
    };
    if pull.reader == ReaderKind::Default {
        shift(vm, source)?;
        return Ok(Some(item));
    }

    let mut ready = Vec::new();
    let prepared = fill_byob(host, vm, source, pull, item, &mut ready)
        .and_then(|()| drain_byob_queue(host, vm, source, &mut ready));
    if prepared.is_ok() {
        host.close_if_drained(vm, source)?;
    }
    finish_ready_reads(host, vm, source, ready, false)?;
    prepared?;
    Ok(None)
}

fn drain_byob_queue(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    ready: &mut Vec<ReadyRead>,
) -> Result<(), JsError> {
    // Pull-into descriptors must leave the controller before any of the
    // associated promises resolve. Promise resolution can synchronously read
    // an inherited then getter which inspects or reenters this controller.
    while let Some(pull) = head(source).filter(|pull| pull.reader == ReaderKind::Byob) {
        let Some(item) = host.pop(vm, source)? else {
            break;
        };
        fill_byob(host, vm, source, pull, item, ready)?;
    }
    Ok(())
}

fn fill_byob(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    pull: PullInto,
    item: Item,
    ready: &mut Vec<ReadyRead>,
) -> Result<(), JsError> {
    debug_assert!(!pull.lent);
    debug_assert!(pull.reader == ReaderKind::Byob);
    let (object, queued) = {
        let state = source.borrow();
        (state.value(), state.queue_size as usize)
    };
    let buffer = pull.buffer(vm, &object);
    let length = item_length(vm, &item)?;
    let capacity = pull.range.end - pull.offset();
    let max_fill = pull.filled + capacity.min(queued.saturating_add(length));
    let aligned = max_fill - max_fill % pull.kind.element_width();
    let filled_minimum = aligned >= pull.minimum;
    let limit = if filled_minimum { aligned } else { max_fill };
    let count = length.min(limit - pull.filled);
    let leftover = copy_into_view(vm, &buffer, pull.offset(), item, count)?;
    let filled = pull.filled + count;
    source.borrow_mut().pull_intos.front_mut().unwrap().filled = filled;
    if let Some(leftover) = leftover {
        host.enqueue_front(vm, source, leftover, (length - count) as f64)?;
    }
    if filled_minimum && filled == aligned {
        ready.push(take_ready_read(vm, source)?);
    } else {
        let state = source.borrow();
        if state.closing && state.queue.is_empty() && matches!(state.source, Source::Custom(_)) {
            return Err(JsError::type_error(
                "Byte stream closed before reaching the requested minimum",
            ));
        }
    }
    Ok(())
}

fn copy_into_view(
    vm: &mut Vm,
    buffer: &Value,
    offset: usize,
    item: Item,
    count: usize,
) -> Result<Option<Item>, JsError> {
    Ok(match item {
        Item::Bytes(mut bytes) => {
            vm.write_array_buffer(buffer, offset, &bytes.data()[..count])?;
            bytes.range.start += count;
            (!bytes.range.is_empty()).then_some(Item::Bytes(bytes))
        }
        Item::Js(view) => {
            let view = vm.fixed_array_buffer_view(&view)?;
            let middle = view.byte_range.start + count;
            vm.copy_into_array_buffer(&view.buffer, view.byte_range.start..middle, buffer, offset)?;
            if middle < view.byte_range.end {
                let value = vm.new_array_buffer_view(
                    &view.buffer,
                    middle..view.byte_range.end,
                    ArrayBufferViewKind::UINT8,
                )?;
                Some(Item::Js(value))
            } else {
                None
            }
        }
    })
}

fn enqueue_view(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    value: Value,
) -> Result<(), JsError> {
    let view = vm.fixed_array_buffer_view(&value)?;
    if view.byte_range.is_empty() {
        return Err(JsError::type_error("A byte stream chunk cannot be empty"));
    }
    if !custom::open(source) {
        return Err(JsError::type_error("ReadableStream cannot accept chunks"));
    }
    let transferred = vm.transfer_array_buffer(&view.buffer)?;
    let weight = view.byte_range.len() as f64;
    let value =
        vm.new_array_buffer_view(&transferred, view.byte_range, ArrayBufferViewKind::UINT8)?;
    let object = source.borrow().value();
    if let Some(pull) = head(source) {
        let buffer = pull.buffer(vm, &object);
        vm.array_buffer_byte_length(&buffer)?;
        if vm
            .host_slot(&object, REQUEST)
            .is_some_and(|value| !value.is_undefined())
        {
            let next = vm.transfer_array_buffer(&buffer)?;
            invalidate_request(vm, &object)?;
            vm.set_host_slot(object, pull.slot, next)?;
        }
        if pull.reader == ReaderKind::Released {
            let (_, buffer) = shift(vm, source)?.unwrap();
            copy_to_queue(
                host,
                vm,
                source,
                &buffer,
                pull.range.start..pull.offset(),
                false,
            )?;
        }
    }
    let direct = {
        let state = source.borrow();
        state.queue.is_empty() && wants(&state)
    };
    let item = Item::Js(value);
    let result = if direct {
        host.deliver(vm, source, item)
    } else {
        host.enqueue(vm, source, item, weight).and_then(|()| {
            let mut ready = Vec::new();
            let prepared = drain_byob_queue(host, vm, source, &mut ready);
            finish_ready_reads(host, vm, source, ready, false)?;
            prepared
        })
    };
    if let Err(error) = result {
        let reason = exception(vm, error.clone())?;
        host.fail(vm, source, reason)?;
        return Err(error);
    }
    host.advance(vm, source)
}
