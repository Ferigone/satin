//! Web streams shared by Fetch, Blob, and author-provided sources and sinks.
mod async_values;
mod bytes;
mod custom;
mod pipes;
#[cfg(test)]
mod tests;
mod transform;
mod writable;

use super::{abort, arg, fatal, getter, method};
use crate::network::{FETCH_BODY_LIMIT, FETCH_CHUNK_BYTES, FetchBody, FetchError};
use rust_browser_javascript::{JsError, ObjectId, PromiseCapability, RootId, Value, Vm};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    ops::Range,
    rc::{Rc, Weak},
    sync::{Arc, Weak as ArcWeak},
    task::Poll,
};

const MAX_QUEUE: usize = 512;
const MAX_READS: usize = 64;
const MAX_TEE_DEPTH: u8 = 24;
const LIVE_BYTES: usize = 32 * 1024 * 1024;
const BRANCH_BYTES: usize = 8 * 1024 * 1024;
const ITEMS: u32 = 0;
const ERROR: u32 = 1;
const READER: u32 = 2;
const OWNER: u32 = 3;
const CLOSED: u32 = 4;
const CONSUMER: u32 = 5;
const UPSTREAM: u32 = 6;
const SIGNAL: u32 = 7;
const CANCEL_FIRST: u32 = 8;
const CANCEL_SECOND: u32 = 9;
const REASON_FIRST: u32 = 10;
const REASON_SECOND: u32 = 11;
const INTERNAL_READ: u32 = 3;
const ITERATOR_READER: u32 = 4;

type Shared = Rc<RefCell<Stream>>;
type Convert = Box<dyn FnOnce(&mut Vm, Vec<u8>, Value, Value) -> Result<(), JsError>>;
type ReadFromHost = Box<dyn FnOnce(&mut Vm, Result<Option<Value>, Value>) -> Result<(), JsError>>;

struct HostRead(RefCell<Option<ReadFromHost>>);

#[derive(Clone)]
struct Bytes {
    backing: Arc<Vec<u8>>,
    range: Range<usize>,
}

impl Bytes {
    fn data(&self) -> &[u8] {
        &self.backing[self.range.clone()]
    }

    fn into_vec(self) -> Vec<u8> {
        if self.range.start == 0 && self.range.end == self.backing.len() {
            return Arc::unwrap_or_clone(self.backing);
        }
        self.data().to_vec()
    }
}

#[derive(Clone)]
enum Item {
    Bytes(Bytes),
    Js(Value),
}

impl Item {
    fn into_value(self, vm: &mut Vm) -> Result<Value, JsError> {
        match self {
            Item::Bytes(bytes) => vm.new_uint8_array(bytes.into_vec()),
            Item::Js(value) => Ok(value),
        }
    }
}

struct Charge {
    total: Rc<Cell<usize>>,
    len: usize,
}

impl Charge {
    fn new(total: &Rc<Cell<usize>>) -> Self {
        Self {
            total: total.clone(),
            len: 0,
        }
    }

    fn add(&mut self, bytes: usize) -> bool {
        let next = self.total.get().saturating_add(bytes);
        if next > LIVE_BYTES {
            return false;
        }
        self.total.set(next);
        self.len += bytes;
        true
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.total.set(self.total.get() - self.len);
    }
}

enum Chunk {
    Bytes(Bytes, Charge),
    Js(usize, Option<Charge>),
}

struct Collector {
    bytes: Vec<u8>,
    charge: Charge,
    convert: Convert,
}

struct Tee {
    branches: [Weak<RefCell<Stream>>; 2],
    canceling: bool,
    byte_reading: bool,
    byte_delivering: bool,
}

enum Source {
    Empty,
    Memory(Bytes),
    Network(FetchBody),
    Custom(custom::Source),
    Branch { side: usize, tee: Rc<RefCell<Tee>> },
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Status {
    Readable,
    Closed,
    Errored,
}

enum Lock {
    Free,
    Reader,
    Consume(Option<Collector>),
    Tee(Rc<RefCell<Tee>>),
    Discarded,
}

struct Stream {
    id: ObjectId,
    status: Status,
    source: Source,
    abort: Option<abort::Subscription>,
    lock: Lock,
    closing: bool,
    disturbed: bool,
    driving: bool,
    bytes: bool,
    auto_allocate: Option<u64>,
    pull_intos: VecDeque<bytes::PullInto>,
    tee_depth: u8,
    queue: VecDeque<(Chunk, f64)>,
    queue_head: usize,
    queue_size: f64,
    buffered: usize,
    read_head: usize,
    reads: usize,
    observe_close: bool,
    in_flight: Option<RootId>,
}

impl Stream {
    fn value(&self) -> Value {
        Value::Object(self.id)
    }
}

struct Reader(bool);
struct Controller;

enum ReadKind {
    Default,
    Internal,
    Iterator(Value),
}

struct Intrinsics {
    stream: RootId,
    reader: RootId,
    controller: RootId,
    transform: Cell<Option<RootId>>,
    bytes: bytes::Intrinsics,
    writable: writable::Host,
    buffered: Rc<Cell<usize>>,
    backings: RefCell<Vec<ArcWeak<Vec<u8>>>>,
    network: RefCell<BTreeMap<u64, Weak<RefCell<Stream>>>>,
}

#[derive(Clone)]
pub(super) struct StreamHost(Rc<Intrinsics>);

fn stream(vm: &Vm, value: &Value) -> Result<Shared, JsError> {
    vm.host_data::<RefCell<Stream>>(value)
        .ok_or_else(|| JsError::type_error("Illegal ReadableStream receiver"))
}

fn reader_stream(vm: &Vm, reader: &Value) -> Result<Option<Shared>, JsError> {
    reader_stream_kind(vm, reader, false)
}

fn reader_stream_kind(vm: &Vm, reader: &Value, byob: bool) -> Result<Option<Shared>, JsError> {
    vm.host_data::<Reader>(reader)
        .filter(|kind| kind.0 == byob)
        .ok_or_else(|| JsError::type_error("Illegal ReadableStream reader receiver"))?;
    vm.host_slot(reader, OWNER)
        .filter(|value| !value.is_nullish())
        .map(|value| stream(vm, &value))
        .transpose()
}

fn table(vm: &mut Vm, owner: &Value) -> Result<Value, JsError> {
    if let Some(items) = vm.host_slot(owner, ITEMS) {
        return Ok(items);
    }
    let items = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_slot(owner.clone(), ITEMS, items.clone())?;
    Ok(items)
}

fn put(vm: &mut Vm, owner: &Value, key: &str, value: Value) -> Result<(), JsError> {
    let items = table(vm, owner)?;
    vm.set_property(items, key, value)
}

fn take(vm: &mut Vm, owner: &Value, key: &str) -> Result<Value, JsError> {
    let items = table(vm, owner)?;
    let value = vm.get_property(items.clone(), key)?;
    vm.set_property(items, key, Value::Undefined)?;
    Ok(value)
}

fn capability(vm: &mut Vm, owner: &Value) -> Result<(Value, Value), JsError> {
    let PromiseCapability {
        promise,
        resolve,
        reject,
    } = vm.new_promise_capability()?;
    let record = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_slot(record.clone(), 0, promise.clone())?;
    vm.set_host_slot(record.clone(), 1, resolve)?;
    vm.set_host_slot(record.clone(), 2, reject)?;
    vm.set_host_slot(promise.clone(), OWNER, owner.clone())?;
    Ok((promise, record))
}

fn promise_record(vm: &Vm, record: &Value) -> Value {
    vm.host_slot(record, 0).unwrap_or_default()
}

fn capabilities(vm: &mut Vm, record: &Value) -> Result<(Value, Value), JsError> {
    let promise = promise_record(vm, record);
    vm.set_host_slot(promise, OWNER, Value::Undefined)?;
    Ok((
        vm.host_slot(record, 1).unwrap_or_default(),
        vm.host_slot(record, 2).unwrap_or_default(),
    ))
}

fn settle(vm: &mut Vm, record: &Value, result: Result<Value, Value>) -> Result<(), JsError> {
    let (resolve, reject) = capabilities(vm, record)?;
    match result {
        Ok(value) => vm.call(resolve, Value::Undefined, &[value])?,
        Err(reason) => vm.call(reject, Value::Undefined, &[reason])?,
    };
    Ok(())
}

fn clear_slot(vm: &mut Vm, object: &Value, slot: u32) -> Result<(), JsError> {
    if vm.host_slot(object, slot).is_some() {
        vm.set_host_slot(object.clone(), slot, Value::Undefined)?;
    }
    Ok(())
}

fn read_result(vm: &mut Vm, value: Value, done: bool) -> Result<Value, JsError> {
    let result = vm.new_object();
    vm.define_data_property(result.clone(), "value", value, true, true, true)?;
    vm.define_data_property(result.clone(), "done", done.into(), true, true, true)?;
    Ok(result)
}

fn exception(vm: &mut Vm, error: JsError) -> Result<Value, JsError> {
    if fatal(&error) {
        return Err(error);
    }
    match error.thrown {
        Some(reason) => Ok(reason),
        None => vm.error_value(&JsError::new(error.kind, error.message)),
    }
}

fn stream_error(vm: &Vm, stream: &Value) -> Value {
    vm.host_slot(stream, ERROR).unwrap_or_default()
}

fn wants(data: &Stream) -> bool {
    match &data.lock {
        Lock::Reader => data.reads > 0,
        Lock::Consume(collector) => collector.is_some(),
        Lock::Tee(tee) => {
            let tee = tee.borrow();
            !tee.byte_delivering
                && (tee.byte_reading
                    || tee.branches.iter().any(|branch| {
                        branch.upgrade().is_some_and(|branch| {
                            let branch = branch.borrow();
                            branch.status == Status::Readable
                                && branch.queue.is_empty()
                                && wants(&branch)
                        })
                    }))
        }
        _ => false,
    }
}

fn observes_close(data: &Stream) -> bool {
    data.status == Status::Readable
        && (data.observe_close
            || matches!(&data.lock, Lock::Tee(tee) if tee
                .borrow().branches.iter().filter_map(Weak::upgrade)
                .any(|branch| observes_close(&branch.borrow()))))
}

fn hold_in_flight(vm: &mut Vm, source: &Shared) {
    if source.borrow().in_flight.is_some() {
        return;
    }
    let owner = source.borrow().value();
    let mut ancestor = source.clone();
    loop {
        if matches!(ancestor.borrow().source, Source::Network(_)) {
            source.borrow_mut().in_flight = Some(vm.root(owner));
            return;
        }
        let Some(upstream) = vm.host_slot(&ancestor.borrow().value(), UPSTREAM) else {
            return;
        };
        let Ok(next) = stream(vm, &upstream) else {
            return;
        };
        ancestor = next;
    }
}

fn release_if_idle(vm: &mut Vm, source: &Shared) {
    let mut state = source.borrow_mut();
    if state.reads == 0
        && !state.observe_close
        && !matches!(state.lock, Lock::Consume(Some(_)))
        && let Some(root) = state.in_flight.take()
    {
        vm.unroot(root);
    }
}

impl StreamHost {
    fn object(
        &self,
        vm: &mut Vm,
        source: Source,
        bytes: bool,
        depth: u8,
    ) -> Result<Value, JsError> {
        let proto = vm.root_value(self.0.stream).unwrap();
        let object = vm.new_object_with_prototype(proto)?;
        Self::initialize_object(vm, object, source, bytes, depth)
    }

    fn initialize_object(
        vm: &mut Vm,
        object: Value,
        source: Source,
        bytes: bool,
        depth: u8,
    ) -> Result<Value, JsError> {
        let state = Stream {
            id: object.as_object().unwrap(),
            status: Status::Readable,
            source,
            abort: None,
            lock: Lock::Free,
            closing: false,
            disturbed: false,
            driving: false,
            bytes,
            auto_allocate: None,
            pull_intos: VecDeque::new(),
            tee_depth: depth,
            queue: VecDeque::new(),
            queue_head: 0,
            queue_size: 0.0,
            buffered: 0,
            read_head: 0,
            reads: 0,
            observe_close: false,
            in_flight: None,
        };
        vm.set_host_data(object.clone(), Rc::new(RefCell::new(state)))?;
        Ok(object)
    }

    pub(super) fn memory_stream(
        &self,
        vm: &mut Vm,
        bytes: Arc<Vec<u8>>,
        range: Range<usize>,
    ) -> Result<Value, JsError> {
        let mut backings = self.0.backings.borrow_mut();
        vm.charge_work(backings.len() + 1)?;
        backings.retain(|entry| entry.strong_count() > 0);
        if !backings
            .iter()
            .any(|entry| entry.as_ptr() == Arc::as_ptr(&bytes))
        {
            let total = backings
                .iter()
                .filter_map(ArcWeak::upgrade)
                .map(|v| v.capacity())
                .sum::<usize>();
            if total.saturating_add(bytes.capacity()) > LIVE_BYTES {
                return Err(JsError::type_error(
                    "ReadableStream backing storage limit exceeded",
                ));
            }
            backings.push(Arc::downgrade(&bytes));
        }
        drop(backings);
        let object = self.object(
            vm,
            Source::Memory(Bytes {
                backing: bytes,
                range,
            }),
            true,
            0,
        )?;
        self.advance(vm, &stream(vm, &object)?)?;
        Ok(object)
    }

    pub(super) fn network_stream(
        &self,
        vm: &mut Vm,
        id: u64,
        body: FetchBody,
        signal: Value,
        cancel: crate::network::CancelSignal,
    ) -> Result<Value, JsError> {
        let object = self.object(vm, Source::Network(body), true, 0)?;
        self.watch_abort(vm, &object, signal, cancel)?;
        let stream = stream(vm, &object)?;
        let mut network = self.0.network.borrow_mut();
        network.retain(|_, stream| {
            stream
                .upgrade()
                .is_some_and(|stream| matches!(stream.borrow().source, Source::Network(_)))
        });
        network.insert(id, Rc::downgrade(&stream));
        Ok(object)
    }

    pub(super) fn watch_abort(
        &self,
        vm: &mut Vm,
        object: &Value,
        signal: Value,
        cancel: crate::network::CancelSignal,
    ) -> Result<(), JsError> {
        let stream = stream(vm, object)?;
        if stream.borrow().status != Status::Readable {
            return Ok(());
        }
        let weak = Rc::downgrade(&stream);
        let host = self.clone();
        let subscription = abort::subscribe(vm, &signal, move |vm, reason| {
            cancel.cancel();
            if let Some(stream) = weak.upgrade() {
                host.fail(vm, &stream, reason)?;
            }
            Ok(())
        })?;
        vm.set_host_slot(object.clone(), SIGNAL, signal)?;
        stream.borrow_mut().abort = Some(subscription);
        Ok(())
    }

    pub(super) fn progress(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let stream = self.0.network.borrow().get(&id).and_then(Weak::upgrade);
        if let Some(stream) = stream {
            let live = if let Source::Network(body) = &stream.borrow().source {
                body.acknowledge();
                true
            } else {
                false
            };
            if live {
                self.advance(vm, &stream)?;
                if matches!(stream.borrow().source, Source::Network(_)) {
                    return Ok(());
                }
            }
        }
        self.0.network.borrow_mut().remove(&id);
        Ok(())
    }

    pub(super) fn stop(&self, vm: &mut Vm, reason: Value) -> Result<(), JsError> {
        let network = std::mem::take(&mut *self.0.network.borrow_mut());
        for stream in network.values().filter_map(Weak::upgrade) {
            let live = matches!(stream.borrow().source, Source::Network(_));
            if live {
                self.fail(vm, &stream, reason.clone())?;
            }
        }
        Ok(())
    }

    pub(super) fn has_network_waiters(&self) -> bool {
        self.0
            .network
            .borrow()
            .values()
            .filter_map(Weak::upgrade)
            .any(|stream| {
                let stream = stream.borrow();
                matches!(&stream.source, Source::Network(_))
                    && (observes_close(&stream) || wants(&stream))
            })
    }

    pub(super) fn is_stream(&self, vm: &Vm, value: &Value) -> bool {
        stream(vm, value).is_ok()
    }

    pub(super) fn disturbed(&self, vm: &Vm, value: &Value) -> bool {
        stream(vm, value).is_ok_and(|source| source.borrow().disturbed)
    }

    pub(super) fn unusable(&self, vm: &Vm, value: &Value) -> bool {
        stream(vm, value).is_ok_and(|source| {
            let source = source.borrow();
            source.disturbed || !matches!(source.lock, Lock::Free)
        })
    }

    /// Fetch uses the immutable bytes as its upload source. Acquiring its
    /// internal reader still locks and disturbs the original Request stream.
    pub(super) fn transfer_request(&self, vm: &Vm, value: &Value) {
        if let Ok(source) = stream(vm, value) {
            let mut source = source.borrow_mut();
            source.lock = Lock::Discarded;
            source.disturbed = true;
            source.source = Source::Empty;
            source.status = Status::Closed;
        }
    }

    pub(super) fn lock_upload(
        &self,
        vm: &mut Vm,
        body: &Value,
        inherited: bool,
    ) -> Result<Value, JsError> {
        let source = stream(vm, body)?;
        let reader = self.acquire(vm, &source)?;
        source.borrow_mut().disturbed |= inherited;
        Ok(reader)
    }

    pub(super) fn pull_upload(
        &self,
        vm: &mut Vm,
        reader: &Value,
        callback: ReadFromHost,
    ) -> Result<(), JsError> {
        let source = reader_stream(vm, reader)?
            .ok_or_else(|| JsError::type_error("Fetch upload reader was released"))?;
        let record = vm.new_object_with_prototype(Value::Null)?;
        vm.set_host_data(
            record.clone(),
            Rc::new(HostRead(RefCell::new(Some(callback)))),
        )?;
        self.perform_read(vm, &source, record)
    }

    pub(super) fn cancel_upload(
        &self,
        vm: &mut Vm,
        body: &Value,
        reason: Value,
    ) -> Result<(), JsError> {
        let source = stream(vm, body)?;
        if source.borrow().status == Status::Readable {
            let promise = self.cancel(vm, &source, reason)?;
            self.handled(vm, promise)?;
        }
        Ok(())
    }

    pub(super) fn collect_bytes(
        &self,
        vm: &mut Vm,
        value: Value,
        convert: Convert,
    ) -> Result<Value, JsError> {
        if value.is_nullish() {
            let cap = vm.new_promise_capability()?;
            if let Err(error) = convert(vm, Vec::new(), cap.resolve, cap.reject.clone()) {
                let error = exception(vm, error)?;
                vm.call(cap.reject, Value::Undefined, &[error])?;
            }
            return Ok(cap.promise);
        }
        let stream = stream(vm, &value)?;
        let (promise, record) = capability(vm, &value)?;
        if self.unusable(vm, &value) {
            let error =
                vm.error_value(&JsError::type_error("Body has already been used or locked"))?;
            let (_, reject) = capabilities(vm, &record)?;
            vm.enqueue_call(reject, Value::Undefined, &[error])?;
            return Ok(promise);
        }
        vm.set_host_slot(value, CONSUMER, record)?;
        {
            let mut stream = stream.borrow_mut();
            stream.disturbed = true;
            stream.lock = Lock::Consume(Some(Collector {
                bytes: Vec::new(),
                charge: Charge::new(&self.0.buffered),
                convert,
            }));
        }
        hold_in_flight(vm, &stream);
        self.advance(vm, &stream)?;
        Ok(promise)
    }

    fn complete_collector(
        &self,
        vm: &mut Vm,
        source: &Shared,
        status: Status,
    ) -> Result<(), JsError> {
        let (object, collector) = {
            let mut source = source.borrow_mut();
            let Lock::Consume(collector) = &mut source.lock else {
                return Ok(());
            };
            let collector = collector.take();
            (source.value(), collector)
        };
        let Some(collector) = collector else {
            return Ok(());
        };
        release_if_idle(vm, source);
        let record = vm.host_slot(&object, CONSUMER).unwrap_or_default();
        clear_slot(vm, &object, CONSUMER)?;
        if status == Status::Errored {
            return settle(vm, &record, Err(stream_error(vm, &object)));
        }
        let (resolve, reject) = capabilities(vm, &record)?;
        if let Err(error) = (collector.convert)(vm, collector.bytes, resolve, reject.clone()) {
            let error = exception(vm, error)?;
            vm.call(reject, Value::Undefined, &[error])?;
        }
        Ok(())
    }

    fn enqueue(
        &self,
        vm: &mut Vm,
        source: &Shared,
        item: Item,
        weight: f64,
    ) -> Result<(), JsError> {
        self.enqueue_at(vm, source, item, weight, false)
    }

    fn enqueue_front(
        &self,
        vm: &mut Vm,
        source: &Shared,
        item: Item,
        weight: f64,
    ) -> Result<(), JsError> {
        self.enqueue_at(vm, source, item, weight, true)
    }

    fn enqueue_at(
        &self,
        vm: &mut Vm,
        source: &Shared,
        item: Item,
        weight: f64,
        front: bool,
    ) -> Result<(), JsError> {
        let (object, index, buffered, is_bytes) = {
            let s = source.borrow();
            if s.queue.len() >= MAX_QUEUE {
                return Err(JsError::type_error("ReadableStream queue limit exceeded"));
            }
            let index = if front {
                (s.queue_head + MAX_QUEUE - 1) % MAX_QUEUE
            } else {
                (s.queue_head + s.queue.len()) % MAX_QUEUE
            };
            (s.value(), index, s.buffered, s.bytes)
        };
        let length = if is_bytes {
            Some(bytes::item_length(vm, &item)?)
        } else {
            None
        };
        let charge = if let Some(length) = length {
            let mut charge = Charge::new(&self.0.buffered);
            if length.saturating_add(buffered) > BRANCH_BYTES || !charge.add(length) {
                return Err(JsError::type_error(
                    "ReadableStream branch buffering limit exceeded",
                ));
            }
            Some(charge)
        } else {
            None
        };
        let weight = length.map_or(weight, |length| length as f64);
        let chunk = match item {
            Item::Js(value) => {
                put(vm, &object, &format!("q{index}"), value)?;
                Chunk::Js(index, charge)
            }
            Item::Bytes(bytes) => {
                Chunk::Bytes(bytes, charge.expect("native bytes have a byte stream"))
            }
        };
        let mut s = source.borrow_mut();
        s.buffered += length.unwrap_or_default();
        if front {
            s.queue_head = index;
            s.queue.push_front((chunk, weight));
        } else {
            s.queue.push_back((chunk, weight));
        }
        s.queue_size += weight;
        Ok(())
    }

    fn pop(&self, vm: &mut Vm, source: &Shared) -> Result<Option<Item>, JsError> {
        let (object, chunk) = {
            let mut s = source.borrow_mut();
            let Some((chunk, weight)) = s.queue.pop_front() else {
                return Ok(None);
            };
            s.queue_head = (s.queue_head + 1) % MAX_QUEUE;
            s.queue_size = (s.queue_size - weight).max(0.);
            s.buffered -= match &chunk {
                Chunk::Bytes(_, charge) | Chunk::Js(_, Some(charge)) => charge.len,
                Chunk::Js(_, None) => 0,
            };
            (s.value(), chunk)
        };
        Ok(Some(match chunk {
            Chunk::Bytes(bytes, _) => Item::Bytes(bytes),
            Chunk::Js(index, _) => Item::Js(take(vm, &object, &format!("q{index}"))?),
        }))
    }

    fn queue_read(vm: &mut Vm, source: &Shared, record: Value) -> Result<(), JsError> {
        let mut state = source.borrow_mut();
        let index = (state.read_head + state.reads) % MAX_READS;
        put(vm, &state.value(), &format!("r{index}"), record)?;
        state.reads += 1;
        Ok(())
    }

    fn next_read(vm: &mut Vm, stream: &Shared) -> Result<Option<Value>, JsError> {
        let (object, index) = {
            let mut s = stream.borrow_mut();
            if s.reads == 0 {
                return Ok(None);
            }
            let index = s.read_head;
            s.read_head = (s.read_head + 1) % MAX_READS;
            s.reads -= 1;
            (s.value(), index)
        };
        Ok(Some(take(vm, &object, &format!("r{index}"))?))
    }

    fn deliver(&self, vm: &mut Vm, source: &Shared, item: Item) -> Result<(), JsError> {
        // Take the lock classification without retaining a borrow across any
        // JavaScript operation or descent into a branch.
        let (tee, is_bytes, can_lend) = {
            let s = source.borrow();
            let tee = if let Lock::Tee(tee) = &s.lock {
                Some(tee.clone())
            } else {
                None
            };
            (
                tee,
                s.bytes,
                matches!(s.source, Source::Custom(_) | Source::Branch { .. }),
            )
        };
        if let Some(tee) = tee {
            source.borrow_mut().disturbed = true;
            if is_bytes && can_lend {
                if let Err(error) = bytes::tee::deliver(self, vm, source, item) {
                    let reason = exception(vm, error)?;
                    self.fail(vm, source, reason)?;
                }
                return Ok(());
            }
            return self.distribute_tee(vm, &tee, item, is_bytes);
        }
        let item = if is_bytes {
            match bytes::fill_read(self, vm, source, item) {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(()),
                Err(error) => {
                    let reason = exception(vm, error)?;
                    return self.fail(vm, source, reason);
                }
            }
        } else {
            item
        };
        if let Some(record) = Self::next_read(vm, source)? {
            let value = item.into_value(vm)?;
            // The last queued chunk is delivered as data, but closing the
            // stream comes before resolving that read's promise.
            self.close_if_drained(vm, source)?;
            release_if_idle(vm, source);
            return self.finish_read(vm, source, &record, Ok(Some(value)));
        }
        let copy = match &item {
            Item::Bytes(bytes) => Ok(Cow::Borrowed(bytes.data())),
            Item::Js(value) if vm.is_uint8_array(value) => vm
                .copy_buffer_source(value)
                .map(|bytes| Cow::Owned(bytes.unwrap_or_default())),
            Item::Js(_) => Err(JsError::type_error(
                "Response stream chunk must be a Uint8Array",
            )),
        };
        let bytes = match copy {
            Ok(bytes) => bytes,
            Err(error) => {
                let reason = exception(vm, error)?;
                return self.fail(vm, source, reason);
            }
        };
        vm.charge_work(bytes.len())?;
        {
            let mut source = source.borrow_mut();
            if let Lock::Consume(Some(collector)) = &mut source.lock
                && collector.bytes.len().saturating_add(bytes.len()) <= FETCH_BODY_LIMIT
                && collector.charge.add(bytes.len())
            {
                collector.bytes.extend_from_slice(&bytes);
                return Ok(());
            }
        }
        let reason =
            vm.error_value(&JsError::type_error("Fetch body collection limit exceeded"))?;
        self.fail(vm, source, reason)
    }

    fn distribute_tee(
        &self,
        vm: &mut Vm,
        tee: &Rc<RefCell<Tee>>,
        item: Item,
        is_bytes: bool,
    ) -> Result<(), JsError> {
        let branches = tee.borrow().branches.each_ref().map(|branch| {
            branch
                .upgrade()
                .filter(|branch| branch.borrow().status == Status::Readable)
        });
        let other = if is_bytes && branches.iter().all(Option::is_some) {
            bytes::clone_item(vm, &item)?
        } else {
            item.clone()
        };
        for (branch, item) in branches.into_iter().zip([item, other]) {
            if let Some(branch) = branch {
                self.deliver_branch(vm, &branch, item)?;
            }
        }
        Ok(())
    }

    fn deliver_branch(&self, vm: &mut Vm, branch: &Shared, item: Item) -> Result<(), JsError> {
        let direct = {
            let state = branch.borrow();
            if state.status != Status::Readable {
                return Ok(());
            }
            wants(&state) && state.queue.is_empty()
        };
        if direct {
            self.deliver(vm, branch, item)?;
        } else if let Err(error) = self.enqueue(vm, branch, item, 1.) {
            let reason = exception(vm, error)?;
            self.fail(vm, branch, reason)?;
        }
        Ok(())
    }

    fn terminal(&self, vm: &mut Vm, source: &Shared, reason: Option<Value>) -> Result<(), JsError> {
        let (object, tee, status, author_close) = {
            let mut stream = source.borrow_mut();
            if stream.status != Status::Readable {
                return Ok(());
            }
            let status = if reason.is_some() {
                Status::Errored
            } else {
                Status::Closed
            };
            stream.status = status;
            stream.closing = true;
            stream.observe_close = false;
            let author_close = stream.bytes && matches!(stream.source, Source::Custom(_));
            stream.source = Source::Empty;
            stream.abort = None;
            let tee = match &stream.lock {
                Lock::Tee(tee) => Some(tee.clone()),
                _ => None,
            };
            (stream.value(), tee, status, author_close)
        };
        clear_slot(vm, &object, SIGNAL)?;
        clear_slot(vm, &object, UPSTREAM)?;
        custom::clear(vm, &object)?;
        if let Some(reason) = &reason {
            vm.set_host_slot(object.clone(), ERROR, reason.clone())?;
        }
        if let Some(reader) = vm.host_slot(&object, READER).filter(|v| !v.is_nullish())
            && let Some(record) = vm.host_slot(&reader, CLOSED)
        {
            if reason.is_some() {
                self.handled(vm, promise_record(vm, &record))?;
            }
            settle(
                vm,
                &record,
                reason.clone().map_or(Ok(Value::Undefined), Err),
            )?;
        }
        if status == Status::Errored {
            bytes::clear(vm, source)?;
        } else if !author_close {
            if bytes::tee::owns_loan(source) {
                bytes::tee::responded(self, vm, source)?;
            } else {
                bytes::finish_closed(self, vm, source)?;
            }
        }
        if !(status == Status::Closed && author_close && bytes::is_pending_byob(source)) {
            while let Some(record) = Self::next_read(vm, source)? {
                self.finish_read(vm, source, &record, reason.clone().map_or(Ok(None), Err))?;
            }
        }
        self.complete_collector(vm, source, status)?;
        release_if_idle(vm, source);
        if let Some(tee) = tee
            && (status == Status::Errored || !bytes::tee::waits_for_close(source))
        {
            self.finish_tee(vm, &object, &tee, reason)?;
        }
        Ok(())
    }

    fn finish_tee(
        &self,
        vm: &mut Vm,
        source: &Value,
        tee: &Rc<RefCell<Tee>>,
        reason: Option<Value>,
    ) -> Result<(), JsError> {
        bytes::tee::clear_waiters(vm, source)?;
        let branches = tee.borrow().branches.clone();
        for branch in branches.iter().filter_map(Weak::upgrade) {
            match &reason {
                Some(reason) => self.fail(vm, &branch, reason.clone())?,
                None => self.close(vm, &branch)?,
            }
        }
        if !tee.borrow().canceling {
            Self::finish_tee_cancels(vm, source, Ok(Value::Undefined))?;
        }
        Ok(())
    }

    fn fail(&self, vm: &mut Vm, source: &Shared, reason: Value) -> Result<(), JsError> {
        while self.pop(vm, source)?.is_some() {}
        self.terminal(vm, source, Some(reason))
    }

    fn close(&self, vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
        source.borrow_mut().closing = true;
        if self.close_if_drained(vm, source)? {
            return Ok(());
        }
        self.advance(vm, source)
    }

    fn close_if_drained(&self, vm: &mut Vm, source: &Shared) -> Result<bool, JsError> {
        let drained = {
            let state = source.borrow();
            state.status == Status::Readable && state.closing && state.queue.is_empty()
        };
        if !drained {
            return Ok(false);
        }
        if bytes::incomplete_element(source) {
            let reason = vm.error_value(&JsError::type_error(
                "Byte stream closed with an incomplete element",
            ))?;
            self.fail(vm, source, reason)?;
        } else {
            self.terminal(vm, source, None)?;
        }
        Ok(true)
    }

    fn advance(&self, vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
        if source.borrow().driving {
            return Ok(());
        }
        source.borrow_mut().driving = true;
        let result = self.advance_inner(vm, source);
        source.borrow_mut().driving = false;
        result
    }

    fn advance_inner(&self, vm: &mut Vm, source: &Shared) -> Result<(), JsError> {
        for _ in 0..MAX_QUEUE {
            let end = match &source.borrow().source {
                Source::Memory(bytes) if bytes.range.is_empty() => Some(Ok(())),
                Source::Network(body) => body.outcome(),
                _ => None,
            };
            if let Some(result) = end {
                if let Err(error) = result {
                    return self.fetch_error(vm, source, error);
                }
                source.borrow_mut().closing = true;
            }
            let (object, status, demand) = {
                let s = source.borrow();
                (s.value(), s.status, wants(&s))
            };
            if status != Status::Readable {
                return self.complete_collector(vm, source, status);
            }
            if self.close_if_drained(vm, source)? {
                return Ok(());
            }
            if bytes::tee::delivering(source) {
                return Ok(());
            }
            if demand {
                bytes::tee::begin(vm, source, None)?;
            }
            if demand && let Some(chunk) = self.pop(vm, source)? {
                self.deliver(vm, source, chunk)?;
                continue;
            }
            let next = if demand {
                match &mut source.borrow_mut().source {
                    Source::Memory(bytes) => {
                        let start = bytes.range.start;
                        let end = bytes.range.end.min(start + FETCH_CHUNK_BYTES);
                        bytes.range.start = end;
                        Poll::Ready(Ok(Some(Bytes {
                            backing: bytes.backing.clone(),
                            range: start..end,
                        })))
                    }
                    Source::Network(body) => body.read().map_ok(|chunk| {
                        chunk.map(|chunk| Bytes {
                            range: 0..chunk.len(),
                            backing: Arc::new(chunk),
                        })
                    }),
                    _ => Poll::Pending,
                }
            } else {
                Poll::Pending
            };
            match next {
                Poll::Ready(Ok(Some(chunk))) => self.deliver(vm, source, Item::Bytes(chunk))?,
                Poll::Ready(Ok(None)) => source.borrow_mut().closing = true,
                Poll::Ready(Err(error)) => return self.fetch_error(vm, source, error),
                Poll::Pending => {
                    let upstream = vm.host_slot(&object, UPSTREAM).filter(|v| !v.is_nullish());
                    if demand && let Some(upstream) = upstream {
                        let upstream = stream(vm, &upstream)?;
                        if bytes::tee::pull_from_branch(vm, &upstream, source)? {
                            self.advance(vm, &upstream)?;
                        }
                        let state = source.borrow();
                        if state.status == Status::Readable
                            && (state.closing || !state.queue.is_empty())
                        {
                            continue;
                        }
                        return Ok(());
                    }
                    return self.maybe_pull(vm, source);
                }
            }
        }
        let owner = Rc::downgrade(source);
        let host = self.clone();
        let resume = vm.new_native_function("continue stream", move |vm, _, _| {
            if let Some(stream) = owner.upgrade() {
                host.advance(vm, &stream)?;
            }
            Ok(Value::Undefined)
        });
        vm.enqueue_microtask(resume)
    }

    fn fetch_error(&self, vm: &mut Vm, source: &Shared, error: FetchError) -> Result<(), JsError> {
        let value = source.borrow().value();
        let reason = match vm
            .host_slot(&value, SIGNAL)
            .and_then(|signal| abort::reason(vm, &signal))
        {
            Some(reason) => reason,
            None => vm.error_value(&JsError::type_error(error.message))?,
        };
        self.fail(vm, source, reason)
    }

    pub(super) fn tee(&self, vm: &mut Vm, value: &Value) -> Result<[Value; 2], JsError> {
        let source = stream(vm, value)?;
        let (bytes, depth) = {
            let s = source.borrow();
            if !matches!(s.lock, Lock::Free) {
                return Err(JsError::type_error("ReadableStream is locked"));
            }
            (s.bytes, s.tee_depth + 1)
        };
        if depth > MAX_TEE_DEPTH {
            return Err(JsError::type_error(
                "ReadableStream tee nesting limit exceeded",
            ));
        }
        let pair = Rc::new(RefCell::new(Tee {
            branches: [Weak::new(), Weak::new()],
            canceling: false,
            byte_reading: false,
            byte_delivering: false,
        }));
        let left = self.object(
            vm,
            Source::Branch {
                side: 0,
                tee: pair.clone(),
            },
            bytes,
            depth,
        )?;
        let right = self.object(
            vm,
            Source::Branch {
                side: 1,
                tee: pair.clone(),
            },
            bytes,
            depth,
        )?;
        for (index, branch) in [&left, &right].into_iter().enumerate() {
            vm.set_host_slot(branch.clone(), UPSTREAM, value.clone())?;
            pair.borrow_mut().branches[index] = Rc::downgrade(&stream(vm, branch)?);
        }
        source.borrow_mut().lock = Lock::Tee(pair.clone());
        let status = source.borrow().status;
        if status != Status::Readable {
            let branches = pair.borrow().branches.clone();
            for branch in branches.iter().filter_map(Weak::upgrade) {
                if status == Status::Closed {
                    self.close(vm, &branch)?;
                } else {
                    self.fail(vm, &branch, stream_error(vm, value))?;
                }
            }
        }
        Ok([left, right])
    }
}

impl StreamHost {
    pub(super) fn install(vm: &mut Vm, abort: abort::AbortHost) -> Result<Self, JsError> {
        use super::webidl::interface;

        let stream_proto = vm.new_object();
        let reader_proto = vm.new_object();
        let controller_proto = vm.new_object();
        let host = Self(Rc::new(Intrinsics {
            stream: vm.root(stream_proto.clone()),
            reader: vm.root(reader_proto.clone()),
            controller: vm.root(controller_proto.clone()),
            transform: Cell::new(None),
            bytes: bytes::Intrinsics::new(vm),
            writable: writable::Host::install(vm, abort)?,
            buffered: Rc::new(Cell::new(0)),
            backings: RefCell::default(),
            network: RefCell::default(),
        }));

        let shared = host.clone();
        interface(
            vm,
            stream_proto.clone(),
            "ReadableStream",
            0,
            move |vm, _, args, new| {
                let new = new.ok_or_else(|| JsError::type_error("ReadableStream requires new"))?;
                shared.create_custom(vm, arg(args, 0), arg(args, 1), Some(new))
            },
        )?;
        getter(vm, stream_proto.clone(), "locked", |vm, this, _| {
            Ok((!matches!(stream(vm, &this)?.borrow().lock, Lock::Free)).into())
        })?;
        let shared = host.clone();
        method(
            vm,
            stream_proto.clone(),
            "getReader",
            move |vm, this, args| {
                let source = stream(vm, &this)?;
                let options = arg(args, 0);
                if !options.is_nullish() {
                    if options.as_object().is_none() {
                        return Err(JsError::type_error(
                            "ReadableStream reader options must be an object",
                        ));
                    }
                    let mode = vm.get_property(options, "mode")?;
                    if !mode.is_undefined() {
                        if super::text(vm, mode)? != "byob" {
                            return Err(JsError::type_error("Invalid ReadableStream reader mode"));
                        }
                        return bytes::acquire(&shared, vm, &source);
                    }
                }
                shared.acquire(vm, &source)
            },
        )?;
        let shared = host.clone();
        method(vm, stream_proto.clone(), "cancel", move |vm, this, args| {
            let source = match stream(vm, &this) {
                Ok(source) if matches!(source.borrow().lock, Lock::Free) => source,
                Ok(_) => return Self::reject(vm, JsError::type_error("ReadableStream is locked")),
                Err(error) => return Self::reject(vm, error),
            };
            shared.cancel(vm, &source, arg(args, 0))
        })?;
        let shared = host.clone();
        method(vm, stream_proto.clone(), "tee", move |vm, this, _| {
            let pair = shared.tee(vm, &this)?;
            Ok(vm.new_array(&pair))
        })?;
        pipes::install(vm, &host, stream_proto.clone())?;
        transform::install(vm, &host)?;
        async_values::install(vm, &host, stream_proto)?;

        let shared = host.clone();
        interface(
            vm,
            reader_proto.clone(),
            "ReadableStreamDefaultReader",
            1,
            move |vm, _, args, new| {
                if new.is_none() {
                    return Err(JsError::type_error(
                        "ReadableStreamDefaultReader requires new",
                    ));
                }
                let source = stream(vm, &arg(args, 0))?;
                shared.acquire(vm, &source)
            },
        )?;
        getter(vm, reader_proto.clone(), "closed", |vm, this, _| {
            Self::reader_closed(vm, &this, false)
        })?;
        let shared = host.clone();
        method(vm, reader_proto.clone(), "read", move |vm, this, _| {
            let source = match reader_stream(vm, &this) {
                Ok(Some(source)) => source,
                Ok(None) => {
                    return Self::reject(
                        vm,
                        JsError::type_error("ReadableStream reader was released"),
                    );
                }
                Err(error) => return Self::reject(vm, error),
            };
            shared.read(vm, &source)
        })?;
        let shared = host.clone();
        method(vm, reader_proto.clone(), "cancel", move |vm, this, args| {
            let source = match reader_stream(vm, &this) {
                Ok(Some(source)) => source,
                Ok(None) => {
                    return Self::reject(
                        vm,
                        JsError::type_error("ReadableStream reader was released"),
                    );
                }
                Err(error) => return Self::reject(vm, error),
            };
            shared.cancel(vm, &source, arg(args, 0))
        })?;
        let shared = host.clone();
        method(vm, reader_proto, "releaseLock", move |vm, reader, _| {
            if let Some(source) = reader_stream(vm, &reader)? {
                shared.release(vm, &source, &reader)?;
            }
            Ok(Value::Undefined)
        })?;
        custom::install(vm, &host, controller_proto)?;
        bytes::install(vm, &host)?;
        Ok(host)
    }

    fn reader_closed(vm: &mut Vm, reader: &Value, byob: bool) -> Result<Value, JsError> {
        match reader_stream_kind(vm, reader, byob) {
            Ok(Some(source)) if source.borrow().status == Status::Readable => {
                source.borrow_mut().observe_close = true;
                hold_in_flight(vm, &source);
            }
            Ok(_) => {}
            Err(error) => return Self::reject(vm, error),
        }
        let record = vm.host_slot(reader, CLOSED).unwrap_or_default();
        Ok(promise_record(vm, &record))
    }

    fn reject(vm: &mut Vm, error: JsError) -> Result<Value, JsError> {
        let cap = vm.new_promise_capability()?;
        let error = exception(vm, error)?;
        vm.call(cap.reject, Value::Undefined, &[error])?;
        Ok(cap.promise)
    }

    fn then(
        &self,
        vm: &mut Vm,
        promise: Value,
        resolve: Value,
        reject: Value,
    ) -> Result<Value, JsError> {
        vm.then_intrinsic(promise, resolve, reject)
    }

    fn handled(&self, vm: &mut Vm, promise: Value) -> Result<(), JsError> {
        vm.mark_promise_handled(&promise)
    }

    fn acquire(&self, vm: &mut Vm, source: &Shared) -> Result<Value, JsError> {
        self.acquire_with(vm, source, self.0.reader, false)
    }

    fn acquire_with(
        &self,
        vm: &mut Vm,
        source: &Shared,
        prototype: RootId,
        byob: bool,
    ) -> Result<Value, JsError> {
        let (value, status) = {
            let s = source.borrow();
            if !matches!(s.lock, Lock::Free) {
                return Err(JsError::type_error("ReadableStream is locked"));
            }
            (s.value(), s.status)
        };
        let reader = vm.new_object_with_prototype(vm.root_value(prototype).unwrap())?;
        vm.set_host_data(reader.clone(), Rc::new(Reader(byob)))?;
        vm.set_host_slot(reader.clone(), OWNER, value.clone())?;
        let (promise, record) = capability(vm, &reader)?;
        vm.set_host_slot(reader.clone(), CLOSED, record.clone())?;
        vm.set_host_slot(value.clone(), READER, reader.clone())?;
        source.borrow_mut().lock = Lock::Reader;
        match status {
            Status::Readable => {}
            Status::Closed => settle(vm, &record, Ok(Value::Undefined))?,
            Status::Errored => {
                self.handled(vm, promise)?;
                settle(vm, &record, Err(stream_error(vm, &value)))?;
            }
        }
        Ok(reader)
    }

    fn read(&self, vm: &mut Vm, source: &Shared) -> Result<Value, JsError> {
        self.read_with_kind(vm, source, ReadKind::Default)
    }

    fn read_internal(&self, vm: &mut Vm, source: &Shared) -> Result<Value, JsError> {
        self.read_with_kind(vm, source, ReadKind::Internal)
    }

    fn read_with_kind(
        &self,
        vm: &mut Vm,
        source: &Shared,
        kind: ReadKind,
    ) -> Result<Value, JsError> {
        let value = source.borrow().value();
        let (promise, record) = capability(vm, &value)?;
        if !matches!(kind, ReadKind::Default) {
            vm.set_host_slot(record.clone(), INTERNAL_READ, true.into())?;
        }
        if let ReadKind::Iterator(reader) = kind {
            vm.set_host_slot(record.clone(), ITERATOR_READER, reader)?;
        }
        self.perform_read(vm, source, record)?;
        Ok(promise)
    }

    fn perform_read(&self, vm: &mut Vm, source: &Shared, record: Value) -> Result<(), JsError> {
        let (value, status, reads) = {
            let mut s = source.borrow_mut();
            s.disturbed = true;
            (s.value(), s.status, s.reads)
        };
        if reads >= MAX_READS {
            let reason = vm.error_value(&JsError::type_error(
                "Too many pending ReadableStream reads",
            ))?;
            return self.finish_read(vm, source, &record, Err(reason));
        }
        match status {
            Status::Closed => self.finish_read(vm, source, &record, Ok(None))?,
            Status::Errored => {
                let reason = stream_error(vm, &value);
                self.finish_read(vm, source, &record, Err(reason))?;
            }
            Status::Readable => match bytes::auto_allocate(vm, source) {
                Ok(()) => {
                    Self::queue_read(vm, source, record)?;
                    hold_in_flight(vm, source);
                    self.advance(vm, source)?;
                }
                Err(error) => {
                    let reason = exception(vm, error)?;
                    self.finish_read(vm, source, &record, Err(reason))?;
                }
            },
        }
        Ok(())
    }

    fn finish_read(
        &self,
        vm: &mut Vm,
        source: &Shared,
        record: &Value,
        chunk: Result<Option<Value>, Value>,
    ) -> Result<(), JsError> {
        if let Some(host) = vm.host_data::<HostRead>(record) {
            let callback = host.0.borrow_mut().take();
            if let Some(callback) = callback {
                return callback(vm, chunk);
            }
            return Ok(());
        }
        if !matches!(chunk, Ok(Some(_)))
            && let Some(reader) = vm.host_slot(record, ITERATOR_READER)
        {
            self.release(vm, source, &reader)?;
        }
        let result = match chunk {
            Ok(value) => {
                let done = value.is_none();
                read_result(vm, value.unwrap_or_default(), done)?
            }
            Err(reason) => return settle(vm, record, Err(reason)),
        };
        if vm.host_slot(record, INTERNAL_READ).is_some() {
            let promise = promise_record(vm, record);
            clear_slot(vm, &promise, OWNER)?;
            vm.fulfill_promise(&promise, result)
        } else {
            settle(vm, record, Ok(result))
        }
    }

    fn release(&self, vm: &mut Vm, source: &Shared, reader: &Value) -> Result<(), JsError> {
        bytes::release(self, vm, source)?;
        let (value, status) = {
            let mut s = source.borrow_mut();
            s.lock = Lock::Free;
            s.observe_close = false;
            (s.value(), s.status)
        };
        let reason = vm.error_value(&JsError::type_error("ReadableStream reader was released"))?;
        let record = if status == Status::Readable {
            vm.host_slot(reader, CLOSED).unwrap_or_default()
        } else {
            let (_, record) = capability(vm, reader)?;
            vm.set_host_slot(reader.clone(), CLOSED, record.clone())?;
            record
        };
        self.handled(vm, promise_record(vm, &record))?;
        settle(vm, &record, Err(reason.clone()))?;
        clear_slot(vm, reader, OWNER)?;
        clear_slot(vm, &value, READER)?;
        while let Some(record) = Self::next_read(vm, source)? {
            settle(vm, &record, Err(reason.clone()))?;
        }
        release_if_idle(vm, source);
        Ok(())
    }

    fn cancel(&self, vm: &mut Vm, source: &Shared, reason: Value) -> Result<Value, JsError> {
        let (object, status, branch) = {
            let mut s = source.borrow_mut();
            s.disturbed = true;
            let branch = match &s.source {
                Source::Branch { side, tee } => Some((*side, tee.clone())),
                _ => None,
            };
            (s.value(), s.status, branch)
        };
        let cap = vm.new_promise_capability()?;
        match status {
            Status::Closed => {
                vm.call(cap.resolve, Value::Undefined, &[])?;
                return Ok(cap.promise);
            }
            Status::Errored => {
                vm.call(cap.reject, Value::Undefined, &[stream_error(vm, &object)])?;
                return Ok(cap.promise);
            }
            Status::Readable => {}
        }
        bytes::clear(vm, source)?;
        if let Some((side, tee)) = branch {
            return self.cancel_branch(vm, source, &object, side, &tee, reason);
        }
        let callback = custom::cancel_callback(vm, &object);
        while self.pop(vm, source)?.is_some() {}
        self.terminal(vm, source, None)?;
        if let Some((receiver, callback)) = callback {
            let native = vm.host_data::<custom::NativeSource>(&receiver).is_some();
            match vm.call(callback, receiver, &[reason]) {
                Ok(result) if native => {
                    let done = vm.new_native_function("", |_, _, _| Ok(Value::Undefined));
                    return self.then(vm, result, done, Value::Undefined);
                }
                Ok(result) => {
                    let source = vm.new_promise_capability()?;
                    vm.call(source.resolve, Value::Undefined, &[result])?;
                    let done = vm.new_native_function("", |_, _, _| Ok(Value::Undefined));
                    let result = self.then(vm, source.promise, done, Value::Undefined)?;
                    vm.call(cap.resolve, Value::Undefined, &[result])?;
                }
                Err(error) => {
                    let error = exception(vm, error)?;
                    vm.call(cap.reject, Value::Undefined, &[error])?;
                }
            }
        } else {
            vm.call(cap.resolve, Value::Undefined, &[])?;
        }
        Ok(cap.promise)
    }

    fn cancel_branch(
        &self,
        vm: &mut Vm,
        source: &Shared,
        branch: &Value,
        side: usize,
        tee: &Rc<RefCell<Tee>>,
        reason: Value,
    ) -> Result<Value, JsError> {
        let upstream = vm.host_slot(branch, UPSTREAM).unwrap_or_default();
        let original = stream(vm, &upstream)?;
        let (promise, record) = capability(vm, &upstream)?;
        let (cancel_slot, reason_slot) = if side == 0 {
            (CANCEL_FIRST, REASON_FIRST)
        } else {
            (CANCEL_SECOND, REASON_SECOND)
        };
        vm.set_host_slot(upstream.clone(), cancel_slot, record)?;
        vm.set_host_slot(upstream.clone(), reason_slot, reason)?;
        while self.pop(vm, source)?.is_some() {}
        self.terminal(vm, source, None)?;
        let gone = tee.borrow().branches.iter().all(|branch| {
            branch
                .upgrade()
                .is_none_or(|s| s.borrow().status != Status::Readable)
        });
        if original.borrow().status != Status::Readable {
            Self::finish_tee_cancels(vm, &upstream, Ok(Value::Undefined))?;
        } else if gone {
            tee.borrow_mut().canceling = true;
            let reasons = [REASON_FIRST, REASON_SECOND]
                .map(|slot| vm.host_slot(&upstream, slot).unwrap_or_default());
            let reasons = vm.new_array(&reasons);
            let result = self.cancel(vm, &original, reasons)?;
            let owner = Rc::downgrade(&original);
            let resolve_owner = owner.clone();
            let resolve = vm.new_native_function("", move |vm, _, _| {
                if let Some(owner) = resolve_owner.upgrade() {
                    let value = owner.borrow().value();
                    Self::finish_tee_cancels(vm, &value, Ok(Value::Undefined))?;
                }
                Ok(Value::Undefined)
            });
            let reject = vm.new_native_function("", move |vm, _, args| {
                if let Some(owner) = owner.upgrade() {
                    let value = owner.borrow().value();
                    Self::finish_tee_cancels(vm, &value, Err(arg(args, 0)))?;
                }
                Ok(Value::Undefined)
            });
            vm.set_host_slot(resolve.clone(), OWNER, upstream.clone())?;
            vm.set_host_slot(reject.clone(), OWNER, upstream)?;
            self.then(vm, result, resolve, reject)?;
        }
        Ok(promise)
    }

    fn finish_tee_cancels(
        vm: &mut Vm,
        upstream: &Value,
        result: Result<Value, Value>,
    ) -> Result<(), JsError> {
        for (slot, reason) in [(CANCEL_FIRST, REASON_FIRST), (CANCEL_SECOND, REASON_SECOND)] {
            if let Some(record) = vm.host_slot(upstream, slot).filter(|v| !v.is_nullish()) {
                clear_slot(vm, upstream, slot)?;
                clear_slot(vm, upstream, reason)?;
                settle(vm, &record, result.clone())?;
            }
        }
        Ok(())
    }
}
