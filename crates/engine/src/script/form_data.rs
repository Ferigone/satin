//! Ordered FormData entries. A hidden VM object traces each File wrapper so
//! iteration, copies, and event handoff preserve identity across collection.
mod mime_body;

use super::{arg, blobs, event_target, forms::FormHost, getter, webidl};
use crate::{dom::NodeId, network::FETCH_BODY_LIMIT};
use rust_browser_javascript::{ErrorKind, JsError, RootId, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
const RECORD_BYTES: usize = 256;
const ENTRY_BYTES: usize = 128;
const FILE_TABLE: u32 = 0;
const ITERATED_FORM: u32 = 0;
const EVENT_FORM_DATA: u32 = 2;

fn limit() -> JsError {
    JsError::new(
        ErrorKind::ExecutionLimit,
        "FormData entry list exceeds its count or byte budget",
    )
}

#[derive(Clone)]
enum Field {
    Text(Rc<str>),
    File(Value),
}

#[derive(Clone)]
struct Entry {
    name: Rc<str>,
    field: Field,
}

impl Entry {
    fn cost(&self) -> usize {
        ENTRY_BYTES
            + self.name.len()
            + match &self.field {
                Field::Text(text) => text.len(),
                Field::File(_) => 0,
            }
    }
}

/// Temporary list used only in one VM task. File Values become traced before
/// the result can escape through a constructor or an event listener.
#[derive(Default)]
pub(super) struct EntryList {
    entries: Vec<Entry>,
    bytes: usize,
}

impl EntryList {
    fn push(&mut self, entry: Entry) -> Result<(), JsError> {
        let bytes = self.bytes + entry.cost();
        if bytes > MAX_ENTRY_BYTES || self.entries.len() >= MAX_ENTRIES {
            return Err(limit());
        }
        self.bytes = bytes;
        self.entries.push(entry);
        Ok(())
    }

    pub(super) fn text(&mut self, name: &str, text: String) -> Result<(), JsError> {
        if self
            .bytes
            .saturating_add(ENTRY_BYTES + name.len() + text.len())
            > MAX_ENTRY_BYTES
        {
            return Err(limit());
        }
        self.push(Entry {
            name: Rc::from(name),
            field: Field::Text(Rc::from(text)),
        })
    }

    pub(super) fn file(&mut self, name: &str, value: Value) -> Result<(), JsError> {
        if self.bytes.saturating_add(ENTRY_BYTES + name.len()) > MAX_ENTRY_BYTES {
            return Err(limit());
        }
        self.push(Entry {
            name: Rc::from(name),
            field: Field::File(value),
        })
    }
}

#[derive(Clone)]
enum StoredField {
    Text(Rc<str>),
    File(u32),
}

struct StoredEntry {
    name: Rc<str>,
    field: StoredField,
}

impl StoredEntry {
    fn cost(&self) -> usize {
        ENTRY_BYTES
            + self.name.len()
            + match &self.field {
                StoredField::Text(text) => text.len(),
                StoredField::File(_) => 0,
            }
    }
}

struct Record {
    entries: Vec<StoredEntry>,
    next_slot: u32,
    free_slots: Vec<u32>,
    bytes: usize,
    resident: Rc<Cell<usize>>,
    reserved: usize,
}

impl Record {
    fn new(resident: Rc<Cell<usize>>) -> Self {
        Self {
            entries: Vec::new(),
            next_slot: 0,
            free_slots: Vec::new(),
            bytes: 0,
            resident,
            reserved: 0,
        }
    }

    fn preflight(&self, count: usize, bytes: usize) -> Result<usize, JsError> {
        if count > MAX_ENTRIES || bytes > MAX_ENTRY_BYTES {
            return Err(limit());
        }
        let reserved = RECORD_BYTES + bytes;
        if self.resident.get() - self.reserved + reserved > MAX_RETAINED_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Retained FormData fields exceed 16 MiB",
            ));
        }
        Ok(reserved)
    }

    fn commit_budget(&mut self, bytes: usize, reserved: usize) {
        self.resident
            .set(self.resident.get() - self.reserved + reserved);
        self.bytes = bytes;
        self.reserved = reserved;
    }

    fn store(
        &mut self,
        vm: &mut Vm,
        table: &Value,
        entry: Entry,
        retired_slot: Option<u32>,
    ) -> Result<StoredEntry, JsError> {
        let field = match entry.field {
            Field::Text(text) => StoredField::Text(text),
            Field::File(value) => {
                let reusable = retired_slot.or_else(|| self.free_slots.last().copied());
                let slot = reusable.unwrap_or(self.next_slot);
                // All properties belong to a null-prototype, unexposed object.
                vm.define_data_property(
                    table.clone(),
                    &slot.to_string(),
                    value,
                    true,
                    false,
                    true,
                )?;
                if reusable.is_none() {
                    self.next_slot += 1;
                } else if retired_slot.is_none() {
                    self.free_slots.pop();
                }
                StoredField::File(slot)
            }
        };
        Ok(StoredEntry {
            name: entry.name,
            field,
        })
    }

    fn check_name(&self, vm: &mut Vm, name: &str) -> Result<(), JsError> {
        vm.charge_work(
            self.entries
                .iter()
                .map(|entry| 1 + entry.name.len().min(name.len()).div_ceil(32))
                .sum::<usize>(),
        )
    }

    fn edit(
        &mut self,
        vm: &mut Vm,
        table: &Value,
        name: &str,
        replacement: Option<Entry>,
    ) -> Result<(), JsError> {
        self.check_name(vm, name)?;
        let mut removed = 0;
        let mut bytes = self.bytes;
        let mut retired = Vec::new();
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.name.as_ref() == name)
        {
            removed += 1;
            bytes -= entry.cost();
            if let StoredField::File(slot) = entry.field {
                retired.push(slot);
            }
        }
        let count = self.entries.len() - removed + usize::from(replacement.is_some());
        bytes += replacement.as_ref().map_or(0, Entry::cost);
        let reserved = self.preflight(count, bytes)?;
        let mut replacement = replacement
            .map(|entry| self.store(vm, table, entry, retired.first().copied()))
            .transpose()?;
        let reused = replacement.as_ref().and_then(|entry| match entry.field {
            StoredField::Text(_) => None,
            StoredField::File(slot) => Some(slot),
        });
        for slot in retired.into_iter().filter(|slot| Some(*slot) != reused) {
            vm.set_property(table.clone(), &slot.to_string(), Value::Undefined)?;
            self.free_slots.push(slot);
        }
        self.entries.retain_mut(|entry| {
            if entry.name.as_ref() != name {
                true
            } else if let Some(new) = replacement.take() {
                *entry = new;
                true
            } else {
                false
            }
        });
        self.entries.extend(replacement);
        if self.entries.capacity() > self.entries.len().saturating_mul(2).max(8) {
            self.entries.shrink_to(self.entries.len().max(8));
        }
        self.commit_budget(bytes, reserved);
        Ok(())
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.reserved);
    }
}

fn record(vm: &Vm, value: &Value) -> Result<Rc<RefCell<Record>>, JsError> {
    vm.host_data::<RefCell<Record>>(value)
        .ok_or_else(|| JsError::type_error("Illegal FormData receiver"))
}

fn table(vm: &Vm, value: &Value) -> Value {
    vm.host_slot(value, FILE_TABLE)
        .expect("FormData has a traced file table")
}

fn field(vm: &mut Vm, table: &Value, entry: &StoredEntry) -> Result<Field, JsError> {
    Ok(match &entry.field {
        StoredField::Text(text) => Field::Text(text.clone()),
        StoredField::File(slot) => Field::File(vm.get_property(table.clone(), &slot.to_string())?),
    })
}

fn value(vm: &mut Vm, table: &Value, entry: &StoredEntry) -> Result<Value, JsError> {
    Ok(match field(vm, table, entry)? {
        Field::Text(text) => {
            vm.charge_work(text.len().div_ceil(8))?;
            vm.ensure_string_allocation(text.len())?;
            text.as_ref().into()
        }
        Field::File(file) => file,
    })
}

fn snapshot(vm: &mut Vm, target: &Value) -> Result<EntryList, JsError> {
    let data = record(vm, target)?;
    let data = data.borrow();
    vm.charge_work(data.entries.len())?;
    let table = table(vm, target);
    let mut entries = Vec::with_capacity(data.entries.len());
    for entry in &data.entries {
        entries.push(Entry {
            name: entry.name.clone(),
            field: field(vm, &table, entry)?,
        });
    }
    Ok(EntryList {
        entries,
        bytes: data.bytes,
    })
}

#[derive(Clone)]
pub(super) struct FormDataHost {
    prototype: RootId,
    event_prototype: RootId,
    resident: Rc<Cell<usize>>,
    blobs: blobs::BlobFactory,
}

impl FormDataHost {
    fn create(
        &self,
        vm: &mut Vm,
        list: EntryList,
        target: Option<Value>,
    ) -> Result<Value, JsError> {
        let mut data = Record::new(self.resident.clone());
        let prototype = vm
            .root_value(self.prototype)
            .expect("rooted FormData prototype");
        let object = if let Some(target) = target {
            webidl::instance(vm, target, &prototype)?
        } else {
            vm.new_object_with_prototype(prototype)?
        };
        let reserved = data.preflight(list.entries.len(), list.bytes)?;
        let table = vm.new_object_with_prototype(Value::Null)?;
        data.entries.reserve(list.entries.len());
        for entry in list.entries {
            let entry = data.store(vm, &table, entry, None)?;
            data.entries.push(entry);
        }
        data.commit_budget(list.bytes, reserved);
        vm.set_host_slot(object.clone(), FILE_TABLE, table)?;
        vm.set_host_data(object.clone(), Rc::new(RefCell::new(data)))?;
        Ok(object)
    }

    pub(super) fn construct_form(
        &self,
        vm: &mut Vm,
        dom: &super::Shared,
        form: NodeId,
        entries: EntryList,
    ) -> Result<Value, JsError> {
        let data = self.create(vm, entries, None)?;
        let prototype = vm
            .root_value(self.event_prototype)
            .expect("rooted FormDataEvent prototype");
        let event = vm.new_object_with_prototype(prototype)?;
        let event =
            event_target::init_event(vm, event, "formdata".into(), [true, false, false], true)?;
        vm.set_host_slot(event.clone(), EVENT_FORM_DATA, data.clone())?;
        super::dispatch_prepared(vm, dom, Some(form), event, &[])?;
        Ok(data)
    }

    pub(super) fn empty_file(&self, vm: &mut Vm) -> Result<Value, JsError> {
        self.blobs
            .file_from_bytes(vm, Vec::new(), "application/octet-stream", String::new())
    }

    #[allow(
        clippy::wrong_self_convention,
        reason = "the factory uses realm intrinsics and budget"
    )]
    pub(super) fn from_body(
        &self,
        vm: &mut Vm,
        mime: &str,
        bytes: &[u8],
    ) -> Result<Value, JsError> {
        let list = mime_body::parse(vm, &self.blobs, mime, bytes)?;
        self.create(vm, list, None)
    }
}

type BodyInit = (Vec<u8>, Option<String>);

pub(super) fn body_init(vm: &mut Vm, value: &Value) -> Result<Option<BodyInit>, JsError> {
    if let Some(blob) = blobs::get(vm, value) {
        let bytes = blob.bytes();
        if bytes.len() > FETCH_BODY_LIMIT {
            return Err(mime_body::too_large());
        }
        vm.charge_work(bytes.len().div_ceil(32))?;
        return Ok(Some((
            bytes.to_vec(),
            (!blob.mime.is_empty()).then(|| blob.mime.to_string()),
        )));
    }
    if vm.host_data::<RefCell<Record>>(value).is_some() {
        let (bytes, mime) = encode(vm, value, Encoding::Multipart)?;
        return Ok(Some((bytes, Some(mime))));
    }
    Ok(None)
}

pub(super) use mime_body::Encoding;

pub(super) fn encode(
    vm: &mut Vm,
    value: &Value,
    encoding: Encoding,
) -> Result<(Vec<u8>, String), JsError> {
    let entries = snapshot(vm, value)?;
    mime_body::encode(vm, &entries, encoding)
}

#[derive(Clone, Copy)]
enum Iteration {
    Entries,
    Keys,
    Values,
}

struct FormIterator {
    index: Cell<usize>,
    kind: Iteration,
}

fn install_iterators(vm: &mut Vm, form: &Value) -> Result<(), JsError> {
    let prototype = vm.new_object_with_prototype(vm.intrinsic_iterator_prototype())?;
    vm.root(prototype.clone());
    webidl::tag(vm, prototype.clone(), "FormData Iterator")?;
    webidl::operation(vm, prototype.clone(), "next", 0, |vm, this, _| {
        let iterator = vm
            .host_data::<FormIterator>(&this)
            .ok_or_else(|| JsError::type_error("Illegal FormData iterator"))?;
        let target = vm
            .host_slot(&this, ITERATED_FORM)
            .expect("iterator retains FormData");
        let data = record(vm, &target)?;
        let data = data.borrow();
        let index = iterator.index.get();
        let Some(entry) = data.entries.get(index) else {
            return vm.iterator_result(Value::Undefined, true);
        };
        vm.charge_work(1 + entry.name.len().div_ceil(8))?;
        vm.ensure_string_allocation(entry.name.len())?;
        let table = table(vm, &target);
        let item = match iterator.kind {
            Iteration::Entries => {
                let value = value(vm, &table, entry)?;
                vm.try_new_array(&[entry.name.as_ref().into(), value])?
            }
            Iteration::Keys => entry.name.as_ref().into(),
            Iteration::Values => value(vm, &table, entry)?,
        };
        iterator.index.set(index + 1);
        vm.iterator_result(item, false)
    })?;
    for (name, kind) in [
        ("entries", Iteration::Entries),
        ("keys", Iteration::Keys),
        ("values", Iteration::Values),
    ] {
        let prototype = prototype.clone();
        let function = webidl::operation(vm, form.clone(), name, 0, move |vm, this, _| {
            record(vm, &this)?;
            let iterator = vm.new_object_with_prototype(prototype.clone())?;
            vm.set_host_data(
                iterator.clone(),
                Rc::new(FormIterator {
                    index: Cell::new(0),
                    kind,
                }),
            )?;
            vm.set_host_slot(iterator.clone(), ITERATED_FORM, this)?;
            Ok(iterator)
        })?;
        if matches!(kind, Iteration::Entries) {
            vm.define_data_property_key(
                form.clone(),
                webidl::well_known_key(vm, "iterator"),
                function,
                true,
                false,
                true,
            )?;
        }
    }
    webidl::operation(vm, form.clone(), "forEach", 1, |vm, this, args| {
        let data = record(vm, &this)?;
        webidl::required(args, 1, "FormData.forEach")?;
        if !vm.is_callable(&args[0]) {
            return Err(JsError::type_error(
                "FormData.forEach callback is not callable",
            ));
        }
        let table = table(vm, &this);
        let mut index = 0;
        loop {
            let pair = {
                let data = data.borrow();
                let Some(entry) = data.entries.get(index) else {
                    break;
                };
                vm.charge_work(1 + entry.name.len().div_ceil(8))?;
                vm.ensure_string_allocation(entry.name.len())?;
                (value(vm, &table, entry)?, Value::from(entry.name.as_ref()))
            };
            vm.call(
                args[0].clone(),
                arg(args, 1),
                &[pair.0, pair.1, this.clone()],
            )?;
            index += 1;
        }
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn install_entries(
    vm: &mut Vm,
    prototype: &Value,
    blobs: &blobs::BlobFactory,
) -> Result<(), JsError> {
    for (method, replace) in [("append", false), ("set", true)] {
        let blobs = blobs.clone();
        webidl::operation(vm, prototype.clone(), method, 2, move |vm, this, args| {
            let data = record(vm, &this)?;
            webidl::required(args, 2, method)?;
            let name = webidl::usv_string(vm, &args[0], MAX_ENTRY_BYTES)?;
            let field = if let Some(blob) = blobs::get(vm, &args[1]) {
                let filename = args
                    .get(2)
                    .filter(|value| !value.is_undefined())
                    .map(|value| webidl::usv_string(vm, value, blobs::NAME_BYTES))
                    .transpose()?;
                Field::File(blobs.as_file(vm, args[1].clone(), &blob, filename)?)
            } else {
                if args.len() >= 3 {
                    return Err(JsError::type_error("A FormData filename requires a Blob"));
                }
                let text = webidl::usv_string(vm, &args[1], MAX_ENTRY_BYTES)?;
                Field::Text(Rc::from(text))
            };
            let table = table(vm, &this);
            let entry = Entry {
                name: Rc::from(name.as_str()),
                field,
            };
            let mut data = data.borrow_mut();
            if replace {
                data.edit(vm, &table, &name, Some(entry))?;
            } else {
                let bytes = data.bytes + entry.cost();
                let reserved = data.preflight(data.entries.len() + 1, bytes)?;
                let entry = data.store(vm, &table, entry, None)?;
                data.entries.push(entry);
                data.commit_budget(bytes, reserved);
            }
            Ok(Value::Undefined)
        })?;
    }
    for method in ["delete", "get", "getAll", "has"] {
        webidl::operation(vm, prototype.clone(), method, 1, move |vm, this, args| {
            let data = record(vm, &this)?;
            webidl::required(args, 1, method)?;
            let name = webidl::usv_string(vm, &args[0], MAX_ENTRY_BYTES)?;
            let table = table(vm, &this);
            if method == "delete" {
                data.borrow_mut().edit(vm, &table, &name, None)?;
                return Ok(Value::Undefined);
            }
            let data = data.borrow();
            data.check_name(vm, &name)?;
            let mut matches = data
                .entries
                .iter()
                .filter(|entry| entry.name.as_ref() == name);
            match method {
                "has" => Ok(matches.next().is_some().into()),
                "get" => matches
                    .next()
                    .map_or(Ok(Value::Null), |entry| value(vm, &table, entry)),
                _ => {
                    let values = matches
                        .map(|entry| value(vm, &table, entry))
                        .collect::<Result<Vec<_>, _>>()?;
                    vm.try_new_array(&values)
                }
            }
        })?;
    }
    Ok(())
}

pub(super) fn install(
    vm: &mut Vm,
    forms: &FormHost,
    blobs: &blobs::BlobFactory,
) -> Result<FormDataHost, JsError> {
    let prototype = vm.new_object();
    let event = vm.get_global("Event")?;
    let parent = vm.get_property(event.clone(), "prototype")?;
    let event_prototype = vm.new_object_with_prototype(parent)?;
    let host = FormDataHost {
        prototype: vm.root(prototype.clone()),
        event_prototype: vm.root(event_prototype.clone()),
        resident: Rc::new(Cell::new(0)),
        blobs: blobs.clone(),
    };
    let factory = host.clone();
    let forms_for_new = forms.clone();
    webidl::interface(
        vm,
        prototype.clone(),
        "FormData",
        0,
        move |vm, _, args, new| {
            let new = new.ok_or_else(|| JsError::type_error("FormData requires new"))?;
            let form = forms_for_new.form_data_args(vm, &arg(args, 0), &arg(args, 1))?;
            let entries = if let Some((form, submitter)) = form {
                let data = forms_for_new.form_data(vm, form, submitter)?;
                let data = data.ok_or_else(|| forms_for_new.entry_list_in_progress(vm))?;
                snapshot(vm, &data)?
            } else {
                EntryList::default()
            };
            factory.create(vm, entries, Some(new))
        },
    )?;
    install_entries(vm, &prototype, blobs)?;
    install_iterators(vm, &prototype)?;
    let intrinsic = event_prototype.clone();
    let constructor = webidl::interface(
        vm,
        event_prototype.clone(),
        "FormDataEvent",
        2,
        move |vm, _, args, new| {
            let new = new.ok_or_else(|| JsError::type_error("FormDataEvent requires new"))?;
            webidl::required(args, 2, "FormDataEvent")?;
            let kind = event_target::kind(vm, &args[0])?;
            if !args[1].is_nullish() && args[1].as_object().is_none() {
                return Err(JsError::type_error(
                    "FormDataEvent options must be a dictionary",
                ));
            }
            let flags = event_target::flags(vm, &args[1])?;
            let data = event_target::option(vm, &args[1], "formData")?;
            record(vm, &data)?;
            let object = webidl::instance(vm, new, &intrinsic)?;
            let event = event_target::init_event(vm, object, kind, flags, false)?;
            vm.set_host_slot(event.clone(), EVENT_FORM_DATA, data)?;
            Ok(event)
        },
    )?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype, Value::Undefined, &[constructor, event])?;
    getter(vm, event_prototype, "formData", |vm, this, _| {
        event_target::event(vm, &this)?;
        let data = vm
            .host_slot(&this, EVENT_FORM_DATA)
            .ok_or_else(|| JsError::type_error("Illegal FormDataEvent receiver"))?;
        record(vm, &data)?;
        Ok(data)
    })?;
    forms.set_form_data(host.clone());
    forms.set_file_factory(blobs.clone());
    Ok(host)
}
