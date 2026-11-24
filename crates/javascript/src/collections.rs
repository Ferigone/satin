//! Native Map and Set data. Monotonic insertion positions let iterators
//! follow live entries while removal releases both indexes immediately.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, SymbolId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::{ObjectKind, Property, PropertyKind, TraceEdges},
};
use std::{collections::BTreeMap, rc::Rc};

mod map;
pub(crate) use map::group_by;
mod set;
pub(crate) mod weak;
pub(crate) mod weak_ref;

// Each index has its own partially filled root node. Beyond the roots, the
// per-entry estimate covers both keys, values, tree edges and node overhead.
const INDEX_ROOT_BYTES: usize = 1536;
const ENTRY_BYTES: usize = 384;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Map,
    Set,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Set => "Set",
        }
    }

    fn adder(self) -> &'static str {
        match self {
            Self::Map => "set",
            Self::Set => "add",
        }
    }

    fn iterator_name(self) -> &'static str {
        match self {
            Self::Map => "Map Iterator",
            Self::Set => "Set Iterator",
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    BigInt(crate::JsBigInt),
    String(JsString),
    Symbol(SymbolId),
    Object(ObjectId),
}

impl From<&Value> for Key {
    fn from(value: &Value) -> Self {
        match value {
            Value::Undefined => Self::Undefined,
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(*value),
            Value::Number(value) => Self::Number(if *value == 0.0 {
                0
            } else if value.is_nan() {
                f64::NAN.to_bits()
            } else {
                value.to_bits()
            }),
            Value::String(value) => Self::String(value.clone()),
            Value::BigInt(value) => Self::BigInt(value.clone()),
            Value::Symbol(value) => Self::Symbol(value.clone()),
            Value::Object(value) => Self::Object(*value),
        }
    }
}

fn canonical_key(value: Value) -> Value {
    if value.as_number() == Some(0.0) {
        Value::Number(0.0)
    } else {
        value
    }
}

fn key_units(value: &Value) -> usize {
    if let Value::BigInt(value) = value {
        (value.bit_length() as usize).div_ceil(32).max(1)
    } else {
        value.as_string().map_or(1, |s| s.len().max(1))
    }
}

#[derive(Clone)]
struct Entry {
    key: Value,
    value: Value,
}

pub(crate) type CollectionCloneEntry = (Value, Option<Value>);

#[derive(Clone)]
pub(crate) struct CollectionData {
    kind: Kind,
    by_key: BTreeMap<Key, u64>,
    entries: BTreeMap<u64, Entry>,
    next_order: u64,
}

impl CollectionData {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            by_key: BTreeMap::new(),
            entries: BTreeMap::new(),
            next_order: 0,
        }
    }

    pub(crate) fn trace(&self, pending: &mut TraceEdges) {
        for entry in self.entries.values() {
            pending.value(&entry.key);
            pending.value(&entry.value);
        }
    }

    pub(crate) fn strings(&self, pending: &mut Vec<JsString>) {
        for entry in self.entries.values() {
            pending.extend(entry.key.as_string().cloned());
            pending.extend(entry.value.as_string().cloned());
        }
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + if self.entries.is_empty() {
                0
            } else {
                INDEX_ROOT_BYTES + self.entries.len().saturating_mul(ENTRY_BYTES)
            }
    }

    fn tree_work(&self) -> usize {
        // Node-local comparisons in a B-tree are bounded, including for a
        // string with a long common prefix. The order index compares integers.
        4 * (1 + self.entries.len().max(1).ilog2() as usize)
    }
}

#[derive(Clone, Copy)]
enum Iteration {
    Keys,
    Values,
    Entries,
}

#[derive(Clone, Copy)]
pub(crate) struct CollectionIterator {
    kind: Kind,
    mode: Iteration,
    pub(crate) target: Option<ObjectId>,
    next_order: u64,
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn receiver(vm: &Vm, value: &Value, kind: Kind) -> Result<ObjectId, JsError> {
    if let Some(id) = value.as_object()
        && let ObjectKind::Collection(data) = &vm.heap.object(id)?.kind
        && data.kind == kind
    {
        return Ok(id);
    }
    Err(JsError::type_error(format!(
        "Receiver does not have {} data",
        kind.name()
    )))
}

impl Vm {
    fn collection(&self, id: ObjectId) -> Result<&CollectionData, JsError> {
        match &self.heap.object(id)?.kind {
            ObjectKind::Collection(data) => Ok(data),
            _ => unreachable!("validated collection handle"),
        }
    }

    fn collection_mut(&mut self, id: ObjectId) -> Result<&mut CollectionData, JsError> {
        match &mut self.heap.object_mut(id)?.kind {
            ObjectKind::Collection(data) => Ok(data),
            _ => unreachable!("validated collection handle"),
        }
    }

    fn new_collection(&mut self, kind: Kind, prototype: ObjectId) -> Result<Value, JsError> {
        self.allocate_constructed(
            ObjectKind::Collection(Box::new(CollectionData::new(kind))),
            prototype,
        )
    }

    fn collection_lookup(
        &mut self,
        id: ObjectId,
        value: &Value,
    ) -> Result<(Key, Option<u64>), JsError> {
        let units = key_units(value);
        self.consume_work(self.collection(id)?.tree_work().saturating_mul(units))?;
        let key = Key::from(value);
        let order = self.collection(id)?.by_key.get(&key).copied();
        Ok((key, order))
    }

    pub(crate) fn collection_insert(
        &mut self,
        id: ObjectId,
        key: Value,
        value: Value,
    ) -> Result<(), JsError> {
        let (index_key, existing) = self.collection_lookup(id, &key)?;
        if let Some(order) = existing {
            if self.collection(id)?.kind == Kind::Map {
                let value = self.account_value(value)?;
                self.collection_mut(id)?
                    .entries
                    .get_mut(&order)
                    .unwrap()
                    .value = value;
            }
            return Ok(());
        }
        let data = self.collection(id)?;
        let count = data.entries.len();
        if count >= self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Collection entry budget exhausted",
            ));
        }
        let next_order = data.next_order.checked_add(1).ok_or_else(|| {
            JsError::new(
                ErrorKind::ExecutionLimit,
                "Collection insertion order exhausted",
            )
        })?;
        let units = key_units(&key);
        self.consume_work(data.tree_work().saturating_mul(units))?;
        let key = self.account_value(canonical_key(key))?;
        let value = self.account_value(value)?;
        self.reserve_allocation(ENTRY_BYTES + if count == 0 { INDEX_ROOT_BYTES } else { 0 })?;
        let data = self.collection_mut(id)?;
        data.by_key.insert(index_key, data.next_order);
        data.entries.insert(data.next_order, Entry { key, value });
        data.next_order = next_order;
        Ok(())
    }

    pub(crate) fn collection_clone_snapshot(
        &mut self,
        source: ObjectId,
    ) -> Result<(Value, Vec<CollectionCloneEntry>), JsError> {
        let collection = self.collection(source)?;
        let count = collection.entries.len();
        let kind = collection.kind;
        let prototype = match kind {
            Kind::Map => self.realm.clone_intrinsics.map,
            Kind::Set => self.realm.clone_intrinsics.set,
        }
        .expect("initialized collection prototype");
        self.consume_work(count)?;
        self.reserve_allocation(count.saturating_mul(std::mem::size_of::<CollectionCloneEntry>()))?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(|_| {
            JsError::new(
                ErrorKind::HeapLimit,
                "Structured clone collection allocation failed",
            )
        })?;
        entries.extend(self.collection(source)?.entries.values().map(|entry| {
            (
                entry.key.clone(),
                (kind == Kind::Map).then(|| entry.value.clone()),
            )
        }));
        Ok((self.new_collection(kind, prototype)?, entries))
    }

    fn collection_delete(&mut self, id: ObjectId, key: Value) -> Result<bool, JsError> {
        let (index_key, order) = self.collection_lookup(id, &key)?;
        let Some(order) = order else { return Ok(false) };
        let units = key_units(&key);
        self.consume_work(self.collection(id)?.tree_work().saturating_mul(units))?;
        let data = self.collection_mut(id)?;
        data.by_key.remove(&index_key);
        data.entries.remove(&order);
        Ok(true)
    }

    fn collection_entry(
        &mut self,
        id: ObjectId,
        cursor: u64,
    ) -> Result<Option<(u64, Value, Value)>, JsError> {
        self.consume_work(self.collection(id)?.tree_work())?;
        let data = self.collection(id)?;
        Ok(data.entries.range(cursor..).next().map(|(&order, entry)| {
            let value = if data.kind == Kind::Set {
                entry.key.clone()
            } else {
                entry.value.clone()
            };
            (order, entry.key.clone(), value)
        }))
    }

    fn collection_next(&mut self, this: Value, kind: Kind) -> Result<Value, JsError> {
        let invalid = || JsError::type_error("Illegal collection iterator receiver");
        let id = this.as_object().ok_or_else(invalid)?;
        let ObjectKind::CollectionIterator(mut data) = self.heap.object(id)?.kind else {
            return Err(invalid());
        };
        if data.kind != kind {
            return Err(invalid());
        }
        let Some(target) = data.target else {
            return self.iterator_result(Value::Undefined, true);
        };
        let Some((order, key, value)) = self.collection_entry(target, data.next_order)? else {
            data.target = None;
            self.heap.object_mut(id)?.kind = ObjectKind::CollectionIterator(data);
            return self.iterator_result(Value::Undefined, true);
        };
        data.next_order = order + 1;
        self.heap.object_mut(id)?.kind = ObjectKind::CollectionIterator(data);
        let item = match data.mode {
            Iteration::Keys => key,
            Iteration::Values => value,
            Iteration::Entries => self.try_new_array(&[key, value])?,
        };
        self.iterator_result(item, false)
    }
}

fn add_iterable_entry(
    vm: &mut Vm,
    kind: Kind,
    adder: Value,
    target: Value,
    entry: Value,
) -> Result<Value, JsError> {
    if kind == Kind::Set {
        return vm.invoke(adder, target, &[entry]);
    }
    if entry.as_object().is_none() {
        return Err(JsError::type_error("Map iterator entry must be an object"));
    }
    let key = vm.get_property(entry.clone(), "0")?;
    let value = vm.get_property(entry, "1")?;
    vm.invoke(adder, target, &[key, value])
}

fn initialize(vm: &mut Vm, this: Value, kind: Kind, iterable: Value) -> Result<Value, JsError> {
    if iterable.is_nullish() {
        return Ok(this);
    }
    let adder = vm.get_property(this.clone(), kind.adder())?;
    if !vm.is_callable(&adder) {
        return Err(JsError::type_error("Collection adder is not callable"));
    }
    let mut iterator = vm.get_iterator(iterable)?;
    while let Some(entry) = vm.iterator_step(&mut iterator)? {
        if let Err(error) = add_iterable_entry(vm, kind, adder.clone(), this.clone(), entry) {
            return vm.iterator_close(&mut iterator, Err(error));
        }
    }
    Ok(this)
}

fn accessor(vm: &mut Vm, object: Value, key: crate::PropertyKey, getter: Value) {
    vm.define_key(
        object,
        key,
        Property {
            kind: PropertyKind::Accessor {
                get: Some(getter),
                set: None,
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
}

fn tag(vm: &mut Vm, object: Value, label: &'static str) {
    vm.define_data_property_key(
        object,
        vm.well_known_key("toStringTag"),
        label.into(),
        false,
        false,
        true,
    )
    .unwrap();
}

fn alias(vm: &mut Vm, object: Value, key: crate::PropertyKey, value: Value) {
    vm.define_data_property_key(object, key, value, true, false, true)
        .unwrap();
}

fn install_iterator(vm: &mut Vm, kind: Kind) -> ObjectId {
    let prototype = vm
        .new_object_with_prototype(Value::Object(vm.realm.iterator_prototype.unwrap()))
        .unwrap();
    // Native callbacks returning iterators capture the intrinsic prototype.
    vm.root(prototype.clone());
    method(vm, prototype.clone(), "next", 0, move |vm, this, _| {
        vm.collection_next(this, kind)
    });
    tag(vm, prototype.clone(), kind.iterator_name());
    prototype.as_object().unwrap()
}

fn iterator_method(
    vm: &mut Vm,
    object: Value,
    name: &str,
    kind: Kind,
    mode: Iteration,
    prototype: ObjectId,
) -> Value {
    method(vm, object, name, 0, move |vm, this, _| {
        let id = receiver(vm, &this, kind)?;
        vm.allocate_constructed(
            ObjectKind::CollectionIterator(CollectionIterator {
                kind,
                mode,
                target: Some(id),
                next_order: 0,
            }),
            prototype,
        )
    })
}

fn install_collection(vm: &mut Vm, kind: Kind) {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    match kind {
        Kind::Map => vm.realm.clone_intrinsics.map = Some(intrinsic),
        Kind::Set => vm.realm.clone_intrinsics.set = Some(intrinsic),
    }
    let constructor = constructor(vm, kind.name(), 0, prototype.clone(), move |_, _, _| {
        Err(JsError::type_error(format!("{} requires new", kind.name())))
    });
    replace_construct(vm, constructor.clone(), move |vm, _, args, target| {
        let inherited = vm.prototype_from_constructor(target, intrinsic)?;
        let this = vm.new_collection(kind, inherited)?;
        initialize(vm, this, kind, arg(args, 0))
    });
    let species = vm.native(
        "get [Symbol.species]",
        0,
        false,
        Rc::new(|_, this, _| Ok(this)),
    );
    let species_key = vm.well_known_key("species");
    accessor(vm, constructor.clone(), species_key, species);

    match kind {
        Kind::Map => map::install(vm, constructor, prototype.clone(), intrinsic),
        Kind::Set => set::install(vm, prototype.clone(), intrinsic),
    }
    method(
        vm,
        prototype.clone(),
        kind.adder(),
        if kind == Kind::Map { 2 } else { 1 },
        move |vm, this, args| {
            let id = receiver(vm, &this, kind)?;
            let value = if kind == Kind::Map {
                arg(args, 1)
            } else {
                Value::Undefined
            };
            vm.collection_insert(id, arg(args, 0), value)?;
            Ok(this)
        },
    );
    method(vm, prototype.clone(), "has", 1, move |vm, this, args| {
        let id = receiver(vm, &this, kind)?;
        Ok(vm.collection_lookup(id, &arg(args, 0))?.1.is_some().into())
    });
    method(vm, prototype.clone(), "delete", 1, move |vm, this, args| {
        let id = receiver(vm, &this, kind)?;
        Ok(vm.collection_delete(id, arg(args, 0))?.into())
    });
    method(vm, prototype.clone(), "clear", 0, move |vm, this, _| {
        let id = receiver(vm, &this, kind)?;
        vm.consume_work(vm.collection(id)?.entries.len().saturating_mul(2))?;
        let data = vm.collection_mut(id)?;
        data.by_key.clear();
        data.entries.clear();
        Ok(Value::Undefined)
    });

    let iterator = install_iterator(vm, kind);
    let entries = iterator_method(
        vm,
        prototype.clone(),
        "entries",
        kind,
        Iteration::Entries,
        iterator,
    );
    method(
        vm,
        prototype.clone(),
        "forEach",
        1,
        move |vm, this, args| {
            let id = receiver(vm, &this, kind)?;
            let callback = arg(args, 0);
            if !vm.is_callable(&callback) {
                return Err(JsError::type_error(
                    "Collection forEach callback is not callable",
                ));
            }
            let callback_this = arg(args, 1);
            let mut cursor = 0;
            while let Some((order, key, value)) = vm.collection_entry(id, cursor)? {
                vm.invoke(
                    callback.clone(),
                    callback_this.clone(),
                    &[value, key, this.clone()],
                )?;
                cursor = order + 1;
            }
            Ok(Value::Undefined)
        },
    );
    if kind == Kind::Map {
        iterator_method(
            vm,
            prototype.clone(),
            "keys",
            kind,
            Iteration::Keys,
            iterator,
        );
    }
    let size = vm.native(
        "get size",
        0,
        false,
        Rc::new(move |vm, this, _| {
            let id = receiver(vm, &this, kind)?;
            Ok(vm.collection(id)?.entries.len().into())
        }),
    );
    accessor(vm, prototype.clone(), "size".into(), size);
    let values = iterator_method(
        vm,
        prototype.clone(),
        "values",
        kind,
        Iteration::Values,
        iterator,
    );
    let default_iterator = match kind {
        Kind::Map => entries,
        Kind::Set => {
            alias(vm, prototype.clone(), "keys".into(), values.clone());
            values
        }
    };
    let iterator_key = vm.well_known_key("iterator");
    alias(vm, prototype.clone(), iterator_key, default_iterator);
    tag(vm, prototype, kind.name());
}

pub(crate) fn install(vm: &mut Vm) {
    install_collection(vm, Kind::Map);
    install_collection(vm, Kind::Set);
    weak::install(vm);
    weak_ref::install(vm);
}
