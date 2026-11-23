//! MutationObserver bindings and delivery on the realm's microtask FIFO.
mod queue;

use super::{
    ErrorKind, JsError, Shared, State, Value, Vm, arg, collections::StaticNodeLists, diagnostic,
    fatal, node, webidl, wrap,
};
use crate::dom::{
    NodeId,
    mutation::{Mutation, siblings},
};
pub(super) use queue::Observers;
use queue::{ObserverId, Options, Record};
use std::{cell::Cell, collections::BTreeSet, rc::Rc};

const CALLBACK_SLOT: u32 = 1;
const ADDED_SLOT: u32 = 1;
const REMOVED_SLOT: u32 = 2;
const FILTER_ITEMS: usize = 256;
const FILTER_NAME_UNITS: usize = 4096;
const FILTER_INPUT_UNITS: usize = 32 * 1024;
const RESIDENT_RECORD_BYTES: usize = 8 * 1024 * 1024;

struct ObserverBrand(ObserverId);

struct MaterializedRecord {
    record: Record,
    bytes: usize,
    resident: Rc<Cell<usize>>,
}
impl Drop for MaterializedRecord {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.bytes);
    }
}

struct RecordFactory {
    prototype: Value,
    resident: Rc<Cell<usize>>,
}
impl RecordFactory {
    fn array(&self, vm: &mut Vm, records: Vec<Record>) -> Result<Value, JsError> {
        vm.charge_work(records.len())?;
        let bytes: usize = records.iter().map(Record::size).sum();
        if self.resident.get() + bytes > RESIDENT_RECORD_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Retained MutationRecord budget exceeded",
            ));
        }
        let mut objects = Vec::with_capacity(records.len());
        for record in records {
            let object = vm.new_object_with_prototype(self.prototype.clone())?;
            let bytes = record.size();
            self.resident.set(self.resident.get() + bytes);
            vm.set_host_data(
                object.clone(),
                Rc::new(MaterializedRecord {
                    record,
                    bytes,
                    resident: self.resident.clone(),
                }),
            )?;
            objects.push(object);
        }
        Ok(vm.new_array(&objects))
    }
}

pub(super) fn note(state: &State, change: Mutation<'_>) {
    state.mutations.borrow_mut().note(&state.document, change);
}

/// Capture sibling identities before an individual child is unlinked.
pub(super) fn removing(state: &State, parent: NodeId, child: NodeId, suppress: bool) {
    let mut observers = state.mutations.borrow_mut();
    if !observers.can_track() {
        return;
    }
    observers.note(&state.document, Mutation::Removing { parent, child });
    if !suppress {
        let (previous, next) = siblings(&state.document, parent, child);
        observers.note(
            &state.document,
            Mutation::Children {
                target: parent,
                added: &[],
                removed: &[child],
                previous,
                next,
            },
        );
    }
}

pub(super) fn schedule(vm: &mut Vm, state: &Shared) {
    let observers = state.borrow().mutations.clone();
    let job = {
        let observers = observers.borrow();
        if observers.queued || !observers.needs_job() {
            return;
        }
        observers
            .job
            .clone()
            .expect("installed MutationObserver notification job")
    };
    match vm.enqueue_microtask(job) {
        Ok(()) => observers.borrow_mut().queued = true,
        Err(error) => {
            observers.borrow_mut().discard_pending();
            diagnostic(state, format!("MutationObserver notification: {error}"));
        }
    }
}

pub(super) fn discard_jobs(vm: &mut Vm, state: &Shared) -> usize {
    let count = vm.clear_jobs();
    state.borrow().mutations.borrow_mut().discard_pending();
    count
}

pub(super) fn finish_checkpoint(state: &Shared) {
    state.borrow().mutations.borrow_mut().finish_checkpoint();
}

pub(super) fn begin_checkpoint(state: &Shared) -> bool {
    state.borrow().mutations.borrow_mut().begin_checkpoint()
}

fn notify(vm: &mut Vm, state: &Shared, factory: &RecordFactory) -> Result<(), JsError> {
    let observers = state.borrow().mutations.clone();
    let (pending, overflow) = observers.borrow_mut().begin_delivery();
    if overflow {
        diagnostic(
            state,
            "MutationObserver tracking or pending record budget exceeded".into(),
        );
    }
    for id in pending {
        let delivery = observers.borrow_mut().delivery(id);
        let Some((root, records)) = delivery else {
            continue;
        };
        let observer = vm.root_value(root).expect("registered observer is rooted");
        let callback = vm
            .host_slot(&observer, CALLBACK_SLOT)
            .expect("observer callback is traced");
        let records = factory.array(vm, records)?;
        if let Err(error) = vm.call(callback, observer.clone(), &[records, observer]) {
            if fatal(&error) {
                return Err(error);
            }
            diagnostic(state, format!("MutationObserver callback: {error}"));
        }
    }
    Ok(())
}

fn observer_id(vm: &Vm, receiver: &Value) -> Result<ObserverId, JsError> {
    vm.host_data::<ObserverBrand>(receiver)
        .map(|brand| brand.0)
        .ok_or_else(|| JsError::type_error("Expected a MutationObserver receiver"))
}

fn flag(vm: &mut Vm, options: &Value, property: &str) -> Result<Option<bool>, JsError> {
    let value = vm.get_property(options.clone(), property)?;
    Ok((!value.is_undefined()).then(|| value.is_truthy()))
}

fn options(vm: &mut Vm, value: Value) -> Result<Options, JsError> {
    if value.as_object().is_none() {
        // A nullish dictionary converts to empty, which fails the observation
        // mode requirement. Other primitives are invalid dictionaries.
        return Err(JsError::type_error(
            "MutationObserver options require a dictionary with an observation mode",
        ));
    }
    // Web IDL reads dictionary members in lexicographic order, including
    // getter side effects and conversion of the attributeFilter sequence.
    let filter = vm.get_property(value.clone(), "attributeFilter")?;
    let attribute_filter = if filter.is_undefined() {
        None
    } else {
        let iterator = webidl::iterator_method(vm, &filter)?;
        let mut units = 0usize;
        let names = webidl::sequence(vm, filter, iterator, FILTER_ITEMS, |vm, item| {
            let name = vm.to_string(&item)?;
            vm.charge_work(name.len())?;
            units = units.saturating_add(name.len());
            if units > FILTER_INPUT_UNITS || name.len() > FILTER_NAME_UNITS {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "MutationObserver filter string budget exceeded",
                ));
            }
            // DOM names in this arena are UTF-8. A lone surrogate from the
            // DOMString sequence cannot match any of those names.
            Ok(String::from_utf16(name.as_utf16()).ok())
        })?;
        Some(names.into_iter().flatten().collect::<BTreeSet<_>>())
    };
    let attribute_old_value = flag(vm, &value, "attributeOldValue")?;
    let attributes = flag(vm, &value, "attributes")?
        .unwrap_or(attribute_old_value.is_some() || attribute_filter.is_some());
    let character_data = flag(vm, &value, "characterData")?;
    let character_data_old_value = flag(vm, &value, "characterDataOldValue")?;
    let character_data = character_data.unwrap_or(character_data_old_value.is_some());
    let child_list = flag(vm, &value, "childList")?.unwrap_or(false);
    let subtree = flag(vm, &value, "subtree")?.unwrap_or(false);
    let attribute_old_value = attribute_old_value.unwrap_or(false);
    let character_data_old_value = character_data_old_value.unwrap_or(false);
    if !(attributes || character_data || child_list)
        || !attributes && (attribute_old_value || attribute_filter.is_some())
        || !character_data && character_data_old_value
    {
        return Err(JsError::type_error(
            "Inconsistent MutationObserver observation modes",
        ));
    }
    Ok(Options {
        attributes,
        character_data,
        child_list,
        subtree,
        attribute_old_value,
        character_data_old_value,
        attribute_filter,
    })
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    node_lists: StaticNodeLists,
) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let record_proto = vm.new_object();
    vm.root(prototype.clone());
    vm.root(record_proto.clone());
    webidl::interface(
        vm,
        record_proto.clone(),
        "MutationRecord",
        0,
        |_, _, _, _| Err(JsError::type_error("Illegal constructor")),
    )?;
    let lists = Rc::new(node_lists);
    for property in [
        "type",
        "target",
        "addedNodes",
        "removedNodes",
        "previousSibling",
        "nextSibling",
        "attributeName",
        "attributeNamespace",
        "oldValue",
    ] {
        let lists = lists.clone();
        let state = state.clone();
        let getter = vm.new_native_function(&format!("get {property}"), move |vm, this, _| {
            let data = vm
                .host_data::<MaterializedRecord>(&this)
                .ok_or_else(|| JsError::type_error("Expected a MutationRecord receiver"))?;
            let change = &data.record.change;
            match property {
                "type" => Ok(change.kind.into()),
                "target" => wrap(vm, &state, change.target),
                "previousSibling" => wrap(vm, &state, change.previous),
                "nextSibling" => wrap(vm, &state, change.next),
                "attributeName" => optional_string(vm, change.name.as_deref()),
                "attributeNamespace" => {
                    Ok(change.namespace.map_or(Value::Null, |ns| ns.uri().into()))
                }
                "oldValue" => optional_string(vm, data.record.old_value.as_deref()),
                "addedNodes" | "removedNodes" => {
                    let (slot, nodes) = if property == "addedNodes" {
                        (ADDED_SLOT, &change.added)
                    } else {
                        (REMOVED_SLOT, &change.removed)
                    };
                    if let Some(list) = vm.host_slot(&this, slot) {
                        return Ok(list);
                    }
                    let list = lists.create(vm, nodes.clone())?;
                    vm.set_host_slot(this, slot, list.clone())?;
                    Ok(list)
                }
                _ => unreachable!("known MutationRecord field"),
            }
        });
        vm.define_accessor(record_proto.clone(), property, Some(getter), None)?;
    }
    let factory = Rc::new(RecordFactory {
        prototype: record_proto,
        resident: Rc::default(),
    });
    let observer_state = state.borrow().mutations.clone();
    let constructor_host = observer_state.clone();
    let intrinsic = prototype.clone();
    webidl::interface(
        vm,
        prototype.clone(),
        "MutationObserver",
        1,
        move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("MutationObserver requires new"))?;
            webidl::required(args, 1, "MutationObserver")?;
            let callback = arg(args, 0);
            if !vm.is_callable(&callback) {
                return Err(JsError::type_error(
                    "MutationObserver callback must be callable",
                ));
            }
            let observer = webidl::instance(vm, target, &intrinsic)?;
            let mut observers = constructor_host.borrow_mut();
            let id = observers.next_id;
            observers.next_id = id.checked_add(1).ok_or_else(|| {
                JsError::new(
                    ErrorKind::ExecutionLimit,
                    "MutationObserver creation index exhausted",
                )
            })?;
            vm.set_host_data(observer.clone(), Rc::new(ObserverBrand(id)))?;
            vm.set_host_slot(observer.clone(), CALLBACK_SLOT, callback)?;
            Ok(observer)
        },
    )?;

    let observe_host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "observe",
        1,
        move |vm, this, args| {
            let id = observer_id(vm, &this)?;
            webidl::required(args, 1, "MutationObserver.observe")?;
            let target = node(&observe_host, &args[0])?;
            let options = options(vm, arg(args, 1))?;
            let observers = observe_host.borrow().mutations.clone();
            observers
                .borrow_mut()
                .observe(id, target, options, || vm.root(this))?;
            Ok(Value::Undefined)
        },
    )?;
    let disconnect_host = observer_state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "disconnect",
        0,
        move |vm, this, _| {
            if let Some(root) = disconnect_host
                .borrow_mut()
                .disconnect(observer_id(vm, &this)?)
            {
                vm.unroot(root);
            }
            Ok(Value::Undefined)
        },
    )?;
    let records_host = observer_state.clone();
    let records_factory = factory.clone();
    webidl::operation(vm, prototype, "takeRecords", 0, move |vm, this, _| {
        let records = records_host
            .borrow_mut()
            .take_records(observer_id(vm, &this)?);
        records_factory.array(vm, records)
    })?;
    let notify_state = state.clone();
    let job = vm.new_native_function("notify mutation observers", move |vm, _, _| {
        notify(vm, &notify_state, &factory)?;
        Ok(Value::Undefined)
    });
    vm.root(job.clone());
    observer_state.borrow_mut().job = Some(job);
    Ok(())
}

fn optional_string(vm: &mut Vm, value: Option<&str>) -> Result<Value, JsError> {
    let Some(value) = value else {
        return Ok(Value::Null);
    };
    vm.charge_work(value.len())?;
    Ok(value.into())
}
