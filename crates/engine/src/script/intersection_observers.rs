//! Samples geometry at the rendering checkpoint and delivers entries in a later task.
mod entries;
mod options;

use super::{
    ErrorKind, JsError, RootId, Shared, Value, Vm, arg, diagnostic, drain_microtask_queue, element,
    fatal, getter,
    layout_api::{GeometryStamp, LayoutHost},
    webidl, wrap,
};
use crate::{
    dom::NodeId,
    layout::{Layout, ObserverRoot, Rect},
};
use entries::{Entry, EntryFactory};
use options::Options;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

const MAX_REGISTRATIONS: usize = 4096;
const MAX_RETAINED_THRESHOLDS: usize = 256 * 1024;
const CALLBACK: u32 = 0;

struct Registration {
    target: NodeId,
    threshold_index: Option<usize>,
}

#[derive(Default)]
struct Tracking {
    targets: Vec<Registration>,
    records: Vec<Entry>,
}

struct Observer {
    index: u64,
    options: Options,
    tracking: RefCell<Tracking>,
    retained_thresholds: Rc<Cell<usize>>,
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.retained_thresholds
            .set(self.retained_thresholds.get() - self.options.thresholds.len());
    }
}

struct ActiveObserver {
    root: RootId,
    observer: Rc<Observer>,
}

#[derive(Default)]
struct Pending {
    next_index: u64,
    active: BTreeMap<u64, ActiveObserver>,
    registrations: usize,
    task_queued: bool,
    sampled: Option<GeometryStamp>,
}

impl Pending {
    fn release_if_idle(&mut self, vm: &mut Vm, observer: &Observer) {
        let tracking = observer.tracking.borrow();
        if tracking.targets.is_empty()
            && tracking.records.is_empty()
            && let Some(active) = self.active.remove(&observer.index)
        {
            vm.unroot(active.root);
        }
    }
}

pub(super) struct Observers {
    pending: Rc<RefCell<Pending>>,
    entries: EntryFactory,
}

impl Observers {
    pub(super) fn install(vm: &mut Vm, state: &Shared) -> Result<Self, JsError> {
        let entries = EntryFactory::install(vm, state)?;
        let pending = Rc::new(RefCell::new(Pending::default()));
        let prototype = vm.new_object();
        let intrinsic = prototype.clone();
        let constructor_state = state.clone();
        let instances = pending.clone();
        let retained_thresholds = Rc::new(Cell::new(0));
        let exception = vm.get_global("DOMException")?;
        vm.root(exception.clone());
        webidl::interface(
            vm,
            prototype.clone(),
            "IntersectionObserver",
            1,
            move |vm, _, args, target| {
                let target = target
                    .ok_or_else(|| JsError::type_error("IntersectionObserver requires new"))?;
                webidl::required(args, 1, "IntersectionObserver")?;
                let callback = arg(args, 0);
                if !vm.is_callable(&callback) {
                    return Err(JsError::type_error(
                        "IntersectionObserver callback must be callable",
                    ));
                }
                let options = Options::convert(vm, &constructor_state, arg(args, 1), &exception)?;
                let object = webidl::instance(vm, target, &intrinsic)?;
                let threshold_count = retained_thresholds.get() + options.thresholds.len();
                if threshold_count > MAX_RETAINED_THRESHOLDS {
                    return Err(JsError::new(
                        ErrorKind::HeapLimit,
                        "Retained IntersectionObserver threshold budget exceeded",
                    ));
                }
                let index = {
                    let mut instances = instances.borrow_mut();
                    let index = instances.next_index;
                    instances.next_index = index.checked_add(1).ok_or_else(|| {
                        JsError::new(
                            ErrorKind::HeapLimit,
                            "IntersectionObserver creation index exhausted",
                        )
                    })?;
                    index
                };
                retained_thresholds.set(threshold_count);
                vm.set_host_data(
                    object.clone(),
                    Rc::new(Observer {
                        index,
                        options,
                        tracking: RefCell::new(Tracking::default()),
                        retained_thresholds: retained_thresholds.clone(),
                    }),
                )?;
                vm.set_host_slot(object.clone(), CALLBACK, callback)?;
                Ok(object)
            },
        )?;

        let ctx = state.clone();
        getter(vm, prototype.clone(), "root", move |vm, this, _| {
            wrap(vm, &ctx, observer(vm, &this)?.options.root)
        })?;
        getter(vm, prototype.clone(), "rootMargin", |vm, this, _| {
            Ok(observer(vm, &this)?.options.serialized_margin().into())
        })?;
        let object = vm.get_global("Object")?;
        let freeze = vm.get_property(object, "freeze")?;
        vm.root(freeze.clone());
        getter(vm, prototype.clone(), "thresholds", move |vm, this, _| {
            let observer = observer(vm, &this)?;
            let values: Vec<_> = observer
                .options
                .thresholds
                .iter()
                .copied()
                .map(Value::from)
                .collect();
            let array = vm.try_new_array(&values)?;
            vm.call(freeze.clone(), Value::Undefined, &[array])
        })?;

        let ctx = state.clone();
        let document = pending.clone();
        webidl::operation(
            vm,
            prototype.clone(),
            "observe",
            1,
            move |vm, this, args| {
                let observer = observer(vm, &this)?;
                webidl::required(args, 1, "IntersectionObserver.observe")?;
                let target = element(&ctx, &arg(args, 0))?;
                let mut pending = document.borrow_mut();
                let mut tracking = observer.tracking.borrow_mut();
                if tracking
                    .targets
                    .iter()
                    .any(|registration| registration.target == target)
                {
                    return Ok(Value::Undefined);
                }
                if pending.registrations >= MAX_REGISTRATIONS {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "IntersectionObserver registration budget exceeded",
                    ));
                }
                tracking.targets.push(Registration {
                    target,
                    threshold_index: None,
                });
                drop(tracking);
                pending.registrations += 1;
                pending.sampled = None;
                pending
                    .active
                    .entry(observer.index)
                    .or_insert_with(|| ActiveObserver {
                        root: vm.root(this),
                        observer,
                    });
                Ok(Value::Undefined)
            },
        )?;

        let ctx = state.clone();
        let document = pending.clone();
        webidl::operation(
            vm,
            prototype.clone(),
            "unobserve",
            1,
            move |vm, this, args| {
                let observer = observer(vm, &this)?;
                webidl::required(args, 1, "IntersectionObserver.unobserve")?;
                let target = element(&ctx, &arg(args, 0))?;
                let mut pending = document.borrow_mut();
                let mut tracking = observer.tracking.borrow_mut();
                if let Some(index) = tracking
                    .targets
                    .iter()
                    .position(|registration| registration.target == target)
                {
                    tracking.targets.remove(index);
                    pending.registrations -= 1;
                    if tracking.targets.capacity() > tracking.targets.len() * 4 {
                        tracking.targets.shrink_to_fit();
                    }
                }
                drop(tracking);
                pending.release_if_idle(vm, &observer);
                Ok(Value::Undefined)
            },
        )?;

        let document = pending.clone();
        webidl::operation(
            vm,
            prototype.clone(),
            "disconnect",
            0,
            move |vm, this, _| {
                let observer = observer(vm, &this)?;
                let mut pending = document.borrow_mut();
                let mut tracking = observer.tracking.borrow_mut();
                pending.registrations -= std::mem::take(&mut tracking.targets).len();
                drop(tracking);
                pending.release_if_idle(vm, &observer);
                Ok(Value::Undefined)
            },
        )?;

        let document = pending.clone();
        let factory = entries.clone();
        webidl::operation(vm, prototype, "takeRecords", 0, move |vm, this, _| {
            let observer = observer(vm, &this)?;
            let entries = take_records(&observer);
            document.borrow_mut().release_if_idle(vm, &observer);
            factory.array(vm, entries)
        })?;

        Ok(Self { pending, entries })
    }

    pub(super) fn has_task(&self) -> bool {
        self.pending.borrow().task_queued
    }

    pub(super) fn needs_update(&self, layout: &LayoutHost) -> bool {
        let pending = self.pending.borrow();
        pending.registrations != 0
            && (pending.sampled.is_none() || pending.sampled != layout.geometry_stamp())
    }

    pub(super) fn update(&self, host: &LayoutHost, state: &Shared, timestamp: f64) {
        if self.has_task() || !self.needs_update(host) {
            return;
        }
        let (layout, stamp) = match host.observer_layout() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                diagnostic(state, format!("IntersectionObserver layout: {error}"));
                return;
            }
        };
        let mut pending = self.pending.borrow_mut();
        pending.sampled = Some(stamp);
        let mut queued = false;
        for active in pending.active.values() {
            let observer = &active.observer;
            let mut tracking = observer.tracking.borrow_mut();
            let Tracking { targets, records } = &mut *tracking;
            let options = &observer.options;
            let root = layout.observer_root(options.root_element, stamp.1, options.margin);
            for registration in targets {
                if let Some(entry) = sample(
                    &layout,
                    root.as_ref(),
                    &options.thresholds,
                    registration,
                    timestamp,
                ) {
                    records.push(entry);
                    queued = true;
                }
            }
        }
        pending.task_queued = queued;
    }

    pub(super) fn notify(&self, vm: &mut Vm, state: &Shared) {
        let ids: Vec<_> = {
            let mut pending = self.pending.borrow_mut();
            if !std::mem::take(&mut pending.task_queued) {
                return;
            }
            pending.active.keys().copied().collect()
        };
        let result = vm.with_task(|vm| {
            for id in ids {
                let (this, entries) = {
                    let mut pending = self.pending.borrow_mut();
                    let Some(active) = pending.active.get(&id) else {
                        continue;
                    };
                    let records = take_records(&active.observer);
                    if records.is_empty() {
                        continue;
                    }
                    let this = vm
                        .root_value(active.root)
                        .expect("active intersection observer is rooted");
                    let observer = active.observer.clone();
                    pending.release_if_idle(vm, &observer);
                    (this, records)
                };
                let records = self.entries.array(vm, entries)?;
                let callback = vm
                    .host_slot(&this, CALLBACK)
                    .expect("observer callback is traced");
                if let Err(error) = vm.call(callback, this.clone(), &[records, this]) {
                    if fatal(&error) {
                        return Err(error);
                    }
                    diagnostic(state, format!("IntersectionObserver callback: {error}"));
                }
                drain_microtask_queue(vm, state)?;
            }
            Ok(())
        });
        if let Err(error) = result {
            diagnostic(state, format!("IntersectionObserver notification: {error}"));
            let mut pending = self.pending.borrow_mut();
            pending.task_queued = pending
                .active
                .values()
                .any(|active| !active.observer.tracking.borrow().records.is_empty());
        }
    }
}

fn observer(vm: &Vm, this: &Value) -> Result<Rc<Observer>, JsError> {
    vm.host_data(this)
        .ok_or_else(|| JsError::type_error("Expected an IntersectionObserver receiver"))
}

fn take_records(observer: &Observer) -> Vec<Entry> {
    std::mem::take(&mut observer.tracking.borrow_mut().records)
}

fn area(rect: Rect) -> f64 {
    f64::from(rect.width) * f64::from(rect.height)
}

fn sample(
    layout: &Layout,
    root: Option<&ObserverRoot>,
    thresholds: &[f64],
    registration: &mut Registration,
    time: f64,
) -> Option<Entry> {
    let geometry = root.map_or_else(Default::default, |root| {
        layout.intersection_sample(root, registration.target)
    });
    let ratio = geometry.intersection.map_or(0., |intersection| {
        let target_area = area(geometry.target);
        if target_area == 0. {
            1.
        } else {
            (area(intersection) / target_area).min(1.)
        }
    });
    // Match deployed Chrome and WPT. An intersection below the first threshold
    // has the same notification state as no intersection at all.
    let index = if geometry.intersection.is_some() {
        thresholds.partition_point(|&threshold| threshold <= ratio)
    } else {
        0
    };
    (registration.threshold_index.replace(index) != Some(index)).then_some(Entry {
        target: registration.target,
        time,
        target_rect: geometry.target,
        root_rect: geometry.root,
        intersection_rect: geometry.intersection.unwrap_or_default(),
        ratio,
        intersecting: index > 0,
    })
}
