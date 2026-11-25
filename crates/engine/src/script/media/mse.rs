//! The document owns object URLs, source-buffer state, and event delivery.
//! The media thread owns demuxing, coded samples, and playback.
mod idl;

use super::{MediaCommand, MediaKey, Shared, Value, Vm, blobs};
use crate::script::{JsError, RootId, event_target, webidl};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    rc::Rc,
};
use url::Url;

const MAX_URLS: usize = 128;
const MAX_BUFFERS: usize = 4;
const MAX_RANGES: usize = 1024;
const LIVE_RANGES: usize = 65536;
const MAX_TASKS: usize = 512;
const MAX_APPEND_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_BYTES: usize = 32 * 1024 * 1024;
const BUFFER_LIST: u32 = 100;
const ACTIVE_LIST: u32 = 101;
const OWNER: u32 = 102;
const FIRST_ENTRY: u32 = 110;

type Range = (f64, f64);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ReadyState {
    #[default]
    Closed,
    Open,
    Ended,
}

#[derive(Default)]
struct Source {
    key: Cell<Option<MediaKey>>,
    ready: Cell<ReadyState>,
    duration: Cell<f64>,
    live: Cell<Option<Range>>,
}

struct Buffer {
    id: u64,
    ranges: RefCell<Vec<Range>>,
    pending: Cell<Option<Operation>>,
    active: Cell<bool>,
    removed: Cell<bool>,
    timestamp_offset: Cell<f64>,
    append_window: Cell<Range>,
}

#[derive(Clone, Copy)]
struct Operation {
    id: u64,
    bytes: usize,
    removing: bool,
}

#[derive(Default)]
struct List {
    count: Cell<usize>,
}

struct Ranges {
    values: Vec<Range>,
    live: Rc<Cell<usize>>,
}

impl Drop for Ranges {
    fn drop(&mut self) {
        self.live.set(self.live.get() - self.values.len());
    }
}

enum Task {
    RateChange(crate::dom::NodeId),
    Event(RootId, &'static str),
    Opened(MediaKey),
    BufferUpdated {
        key: MediaKey,
        buffer: u64,
        operation: u64,
        result: Result<Vec<Range>, String>,
    },
}

#[derive(Default)]
pub(super) struct Store {
    urls: HashMap<String, RootId>,
    attached: HashMap<MediaKey, RootId>,
    tasks: VecDeque<Task>,
    events: Option<event_target::Events>,
    ranges_prototype: Value,
    live_ranges: Rc<Cell<usize>>,
    next_buffer: u64,
    next_operation: u64,
    pending_bytes: usize,
}

fn quota() -> JsError {
    JsError::new(
        super::ErrorKind::ExecutionLimit,
        "Media Source task limit exceeded",
    )
}

fn brand<T: 'static>(vm: &Vm, object: &Value, name: &str) -> Result<Rc<T>, JsError> {
    vm.host_data::<T>(object)
        .ok_or_else(|| JsError::type_error(format!("Illegal {name} receiver")))
}

fn list_values(vm: &Vm, list: &Value) -> Result<Vec<Value>, JsError> {
    let len = brand::<List>(vm, list, "SourceBufferList")?.count.get();
    Ok((0..len)
        .map(|index| {
            vm.host_slot(list, FIRST_ENTRY + index as u32)
                .unwrap_or_default()
        })
        .collect())
}

fn replace_list(vm: &mut Vm, list: Value, entries: &[Value]) -> Result<(), JsError> {
    let data = brand::<List>(vm, &list, "SourceBufferList")?;
    let count = data.count.get().max(entries.len());
    for index in 0..count {
        let value = entries.get(index).cloned().unwrap_or_default();
        vm.set_host_slot(list.clone(), FIRST_ENTRY + index as u32, value)?;
    }
    data.count.set(entries.len());
    Ok(())
}

fn list_for(vm: &Vm, source: &Value, slot: u32) -> Value {
    vm.host_slot(source, slot)
        .expect("source owns its buffer lists")
}

fn valid_ranges(mut ranges: Vec<Range>) -> Vec<Range> {
    ranges
        .retain(|&(start, end)| start.is_finite() && end.is_finite() && start >= 0. && start < end);
    ranges.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<Range> = Vec::new();
    for range in ranges.into_iter().take(MAX_RANGES) {
        if let Some(prior) = merged.last_mut()
            && range.0 <= prior.1
        {
            prior.1 = prior.1.max(range.1);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn intersect(left: &[Range], right: &[Range]) -> Vec<Range> {
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    while let (Some(a), Some(b)) = (left.get(i), right.get(j)) {
        let start = a.0.max(b.0);
        let end = a.1.min(b.1);
        if start < end {
            result.push((start, end));
        }
        if a.1 <= b.1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    result
}

impl Store {
    pub(super) fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }

    pub(super) fn reset_rate_changes(
        &mut self,
        node: crate::dom::NodeId,
        changed: bool,
    ) -> Result<(), JsError> {
        // The media load algorithm discards pending tasks for the old load.
        self.tasks
            .retain(|task| !matches!(task, Task::RateChange(target) if *target == node));
        if changed {
            self.queue_rate_change(node)?;
        }
        Ok(())
    }

    pub(super) fn queue_rate_change(&mut self, node: crate::dom::NodeId) -> Result<(), JsError> {
        self.ensure_tasks(1)?;
        self.tasks.push_back(Task::RateChange(node));
        Ok(())
    }

    fn ensure_tasks(&self, additional: usize) -> Result<(), JsError> {
        if self.tasks.len() + additional > MAX_TASKS {
            return Err(quota());
        }
        Ok(())
    }

    fn event(&mut self, vm: &mut Vm, target: &Value, name: &'static str) {
        debug_assert!(self.tasks.len() < MAX_TASKS);
        self.tasks
            .push_back(Task::Event(vm.root(target.clone()), name));
    }

    fn create_url(&mut self, vm: &mut Vm, url: &Url, object: Value) -> Result<String, JsError> {
        if self.urls.len() >= MAX_URLS {
            return Err(JsError::new(
                super::ErrorKind::ExecutionLimit,
                "Object URL limit exceeded",
            ));
        }
        let mut bytes = [0; 16];
        SystemRandom::new().fill(&mut bytes).map_err(|_| {
            JsError::new(super::ErrorKind::Error, "Object URL randomness unavailable")
        })?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let mut result = format!("blob:{}/", url.origin().ascii_serialization());
        for (i, byte) in bytes.into_iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                result.push('-');
            }
            write!(result, "{byte:02x}").expect("write to a string");
        }
        if self.urls.contains_key(&result) {
            return Err(JsError::new(
                super::ErrorKind::InternalError,
                "Object URL collision",
            ));
        }
        self.urls.insert(result.clone(), vm.root(object));
        Ok(result)
    }

    pub(super) fn resolve_url(&self, vm: &Vm, url: &Url) -> Option<Value> {
        let mut address = url.clone();
        address.set_fragment(None);
        vm.root_value(*self.urls.get(address.as_str())?)
    }

    pub(super) fn attach(&mut self, vm: &mut Vm, object: Value, key: MediaKey) -> bool {
        let Some(source) = vm.host_data::<Source>(&object) else {
            return false;
        };
        if source.key.get().is_some() {
            return false;
        }
        source.key.set(Some(key));
        self.attached.insert(key, vm.root(object));
        true
    }

    fn object_for(&self, vm: &Vm, key: MediaKey) -> Option<Value> {
        vm.root_value(*self.attached.get(&key)?)
    }

    fn release_pending(&mut self, buffer: &Buffer) -> Option<Operation> {
        let operation = buffer.pending.take()?;
        self.pending_bytes -= operation.bytes;
        Some(operation)
    }

    pub(super) fn detach(&mut self, vm: &mut Vm, key: MediaKey) -> Result<(), JsError> {
        let Some(object) = self.object_for(vm, key) else {
            return Ok(());
        };
        let source = brand::<Source>(vm, &object, "MediaSource")?;
        let list = list_for(vm, &object, BUFFER_LIST);
        let active = list_for(vm, &object, ACTIVE_LIST);
        let buffers = list_values(vm, &list)?;
        // Preserve enough queue space for closing, even if JS filled the task queue.
        let queue_events = self.ensure_tasks(3 + buffers.len() * 2).is_ok();
        for value in &buffers {
            let buffer = brand::<Buffer>(vm, value, "SourceBuffer")?;
            buffer.removed.set(true);
            buffer.ranges.borrow_mut().clear();
            if self.release_pending(&buffer).is_some() && queue_events {
                self.event(vm, value, "abort");
                self.event(vm, value, "updateend");
            }
        }
        let was_active = brand::<List>(vm, &active, "SourceBufferList")?.count.get() != 0;
        replace_list(vm, list.clone(), &[])?;
        replace_list(vm, active.clone(), &[])?;
        let old_state = source.ready.replace(ReadyState::Closed);
        source.key.set(None);
        source.duration.set(f64::NAN);
        source.live.set(None);
        if queue_events {
            if was_active {
                self.event(vm, &active, "removesourcebuffer");
            }
            if !buffers.is_empty() {
                self.event(vm, &list, "removesourcebuffer");
            }
            if old_state != ReadyState::Closed {
                self.event(vm, &object, "sourceclose");
            }
        }
        if let Some(root) = self.attached.remove(&key) {
            vm.unroot(root);
        }
        Ok(())
    }

    pub(super) fn metadata(&self, vm: &Vm, key: MediaKey, duration: f64) {
        if let Some(source) = self
            .object_for(vm, key)
            .and_then(|obj| vm.host_data::<Source>(&obj))
            && source.duration.get().is_nan()
            && !duration.is_nan()
            && duration >= 0.
        {
            source.duration.set(duration);
        }
    }

    pub(super) fn enqueue_opened(&mut self, key: MediaKey) -> Result<(), JsError> {
        self.ensure_tasks(1)?;
        self.tasks.push_back(Task::Opened(key));
        Ok(())
    }

    pub(super) fn enqueue_result(
        &mut self,
        key: MediaKey,
        buffer: u64,
        operation: u64,
        result: Result<Vec<Range>, String>,
    ) -> Result<(), JsError> {
        self.ensure_tasks(1)?;
        self.tasks.push_back(Task::BufferUpdated {
            key,
            buffer,
            operation,
            result,
        });
        Ok(())
    }
}

pub(super) fn run_next(vm: &mut Vm, state: &Shared) -> Result<bool, JsError> {
    let task = state.borrow_mut().media.mse.tasks.pop_front();
    match task {
        None => return Ok(false),
        Some(Task::RateChange(node)) => {
            super::event(vm, state, node, "ratechange")?;
        }
        Some(Task::Event(root, name)) => {
            let events = state
                .borrow()
                .media
                .mse
                .events
                .clone()
                .expect("media events installed");
            let result = (|| {
                let target = vm.root_value(root).expect("event target rooted");
                let event = vm.new_object_with_prototype(events.event_proto.clone())?;
                let event = event_target::init_event(vm, event, name.into(), [false; 3], true)?;
                events.dispatch(vm, target, event)
            })();
            vm.unroot(root);
            result?;
        }
        Some(Task::Opened(key)) => {
            let mut state = state.borrow_mut();
            let store = &mut state.media.mse;
            if let Some(object) = store.object_for(vm, key)
                && let Some(source) = vm.host_data::<Source>(&object)
                && source.ready.get() == ReadyState::Closed
            {
                store.ensure_tasks(1)?;
                source.ready.set(ReadyState::Open);
                store.event(vm, &object, "sourceopen");
            }
        }
        Some(Task::BufferUpdated {
            key,
            buffer,
            operation,
            result,
        }) => {
            finish_operation(vm, state, key, buffer, operation, result)?;
        }
    }
    Ok(true)
}

fn finish_operation(
    vm: &mut Vm,
    state: &Shared,
    key: MediaKey,
    id: u64,
    operation: u64,
    result: Result<Vec<Range>, String>,
) -> Result<(), JsError> {
    let mut realm = state.borrow_mut();
    let media = &mut realm.media;
    let store = &mut media.mse;
    let Some(source_value) = store.object_for(vm, key) else {
        return Ok(());
    };
    let list = list_for(vm, &source_value, BUFFER_LIST);
    let buffers = list_values(vm, &list)?;
    let Some((value, buffer)) = buffers.iter().find_map(|value| {
        let buffer = vm.host_data::<Buffer>(value)?;
        (buffer.id == id).then_some((value, buffer))
    }) else {
        return Ok(());
    };
    if operation != 0
        && buffer
            .pending
            .get()
            .is_none_or(|current| current.id != operation)
    {
        return Ok(());
    }
    store.ensure_tasks(3)?;
    let pending = store.release_pending(&buffer);
    match result {
        Ok(ranges) => {
            let ranges = valid_ranges(ranges);
            if !ranges.is_empty() && !buffer.active.get() {
                let active = list_for(vm, &source_value, ACTIVE_LIST);
                buffer.active.set(true);
                let ordered: Vec<_> = buffers
                    .iter()
                    .filter(|entry| {
                        vm.host_data::<Buffer>(entry)
                            .is_some_and(|data| data.active.get())
                    })
                    .cloned()
                    .collect();
                replace_list(vm, active.clone(), &ordered)?;
                store.event(vm, &active, "addsourcebuffer");
            }
            buffer.ranges.replace(ranges);
            store.event(vm, value, "update");
        }
        Err(error) => {
            if operation == 0 {
                buffer.removed.set(true);
            }
            store.event(vm, value, "error");
            drop(realm);
            super::diagnostic(state, format!("SourceBuffer: {error}"));
            if pending.is_some() {
                state.borrow_mut().media.mse.event(vm, value, "updateend");
            }
            return Ok(());
        }
    }
    if pending.is_some() {
        store.event(vm, value, "updateend");
    }
    Ok(())
}

fn make_ranges(vm: &mut Vm, store: &Store, ranges: Vec<Range>) -> Result<Value, JsError> {
    let total = store.live_ranges.get() + ranges.len();
    if total > LIVE_RANGES {
        return Err(JsError::new(
            super::ErrorKind::HeapLimit,
            "Live TimeRanges limit exceeded",
        ));
    }
    vm.charge_work(ranges.len().saturating_mul(16))?;
    let value = vm.new_object_with_prototype(store.ranges_prototype.clone())?;
    store.live_ranges.set(total);
    vm.set_host_data(
        value.clone(),
        Rc::new(Ranges {
            values: ranges,
            live: store.live_ranges.clone(),
        }),
    )?;
    Ok(value)
}

pub(super) fn element_ranges(
    vm: &mut Vm,
    state: &Shared,
    key: Option<MediaKey>,
    seekable: bool,
    duration: f64,
) -> Result<Value, JsError> {
    let realm = state.borrow();
    let store = &realm.media.mse;
    let source_value = key.and_then(|key| store.object_for(vm, key));
    let Some(source_value) = source_value else {
        let ranges = if duration.is_finite() && duration > 0. {
            vec![(0., duration)]
        } else {
            Vec::new()
        };
        return make_ranges(vm, store, ranges);
    };
    let source = brand::<Source>(vm, &source_value, "MediaSource")?;
    let active = list_for(vm, &source_value, ACTIVE_LIST);
    let ranges: Vec<_> = list_values(vm, &active)?
        .iter()
        .filter_map(|value| {
            vm.host_data::<Buffer>(value)
                .map(|buffer| buffer.ranges.borrow().clone())
        })
        .collect();
    let highest = ranges
        .iter()
        .filter_map(|ranges| ranges.last().map(|range| range.1))
        .fold(0., f64::max);
    let value = if seekable {
        let duration = source.duration.get();
        if duration.is_finite() && duration > 0. {
            vec![(0., duration)]
        } else if let Some((start, end)) = source.live.get() {
            let earliest = ranges
                .iter()
                .filter_map(|ranges| ranges.first().map(|range| range.0))
                .fold(start, f64::min);
            vec![(earliest, end.max(highest))]
        } else if highest > 0. {
            vec![(0., highest)]
        } else {
            Vec::new()
        }
    } else if highest == 0. {
        Vec::new()
    } else {
        ranges
            .into_iter()
            .fold(vec![(0., highest)], |prior, mut current| {
                if source.ready.get() == ReadyState::Ended
                    && let Some(last) = current.last_mut()
                {
                    last.1 = highest;
                }
                intersect(&prior, &current)
            })
    };
    make_ranges(vm, store, value)
}

pub(super) use idl::install;
