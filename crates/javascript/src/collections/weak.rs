//! Weak collections hold non-owning keys. The heap visits a WeakMap's value
//! only after it has marked the corresponding object or symbol key.
use super::{Kind, arg, initialize, tag};
use crate::{
    ErrorKind, JsError, JsString, ObjectId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::{ObjectKind, TraceTarget},
    value::SymbolData,
};
use std::{
    collections::BTreeMap,
    rc::{Rc, Weak},
};

const ROOT_BYTES: usize = 768;
const ENTRY_BYTES: usize = 192;

struct Entry {
    // Keep a symbol's allocation address unique until this weak entry is
    // pruned. Weak never makes its target reachable.
    _symbol_key: Option<Weak<SymbolData>>,
    value: Value,
}

pub(crate) struct WeakData {
    kind: Kind,
    entries: BTreeMap<TraceTarget, Entry>,
}

impl WeakData {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            entries: BTreeMap::new(),
        }
    }

    pub(crate) fn ephemerons(&self) -> impl Iterator<Item = (TraceTarget, TraceTarget)> + '_ {
        self.entries.iter().filter_map(|(&key, entry)| {
            TraceTarget::of_value(&entry.value).map(|value| (key, value))
        })
    }

    pub(crate) fn retain_keys(&mut self, mut keep: impl FnMut(TraceTarget) -> bool) {
        self.entries.retain(|&key, _| keep(key));
    }

    pub(crate) fn strings(&self, out: &mut Vec<JsString>) {
        out.extend(
            self.entries
                .values()
                .filter_map(|entry| entry.value.as_string().cloned()),
        );
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + if self.entries.is_empty() {
                0
            } else {
                ROOT_BYTES + self.entries.len().saturating_mul(ENTRY_BYTES)
            }
    }

    fn lookup_work(&self) -> usize {
        4 * (1 + self.entries.len().max(1).ilog2() as usize)
    }
}

fn receiver(vm: &Vm, this: &Value, kind: Kind) -> Result<ObjectId, JsError> {
    if let Some(id) = this.as_object()
        && let ObjectKind::WeakCollection(data) = &vm.heap.object(id)?.kind
        && data.kind == kind
    {
        return Ok(id);
    }
    Err(JsError::type_error(
        "Receiver does not have the requested weak collection data",
    ))
}

fn required_key(key: &Value) -> Result<TraceTarget, JsError> {
    TraceTarget::of_value(key).ok_or_else(|| {
        JsError::type_error("Weak collection key must be an object or non-registered symbol")
    })
}

impl Vm {
    fn weak_collection(&self, id: ObjectId) -> Result<&WeakData, JsError> {
        match &self.heap.object(id)?.kind {
            ObjectKind::WeakCollection(data) => Ok(data),
            _ => unreachable!("validated weak collection handle"),
        }
    }

    fn weak_collection_mut(&mut self, id: ObjectId) -> Result<&mut WeakData, JsError> {
        match &mut self.heap.object_mut(id)?.kind {
            ObjectKind::WeakCollection(data) => Ok(data),
            _ => unreachable!("validated weak collection handle"),
        }
    }

    fn weak_get(&mut self, id: ObjectId, identity: TraceTarget) -> Result<Option<Value>, JsError> {
        self.consume_work(self.weak_collection(id)?.lookup_work())?;
        Ok(self
            .weak_collection(id)?
            .entries
            .get(&identity)
            .map(|entry| entry.value.clone()))
    }

    fn weak_insert(
        &mut self,
        id: ObjectId,
        key: &Value,
        identity: TraceTarget,
        value: Value,
    ) -> Result<(), JsError> {
        let data = self.weak_collection(id)?;
        let count = data.entries.len();
        self.consume_work(data.lookup_work().saturating_mul(2))?;
        let value = self.account_value(value)?;
        if let Some(existing) = self.weak_collection_mut(id)?.entries.get_mut(&identity) {
            existing.value = value;
            return Ok(());
        }
        if count >= self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Weak collection entry budget exhausted",
            ));
        }
        self.reserve_allocation(ENTRY_BYTES + if count == 0 { ROOT_BYTES } else { 0 })?;
        let symbol = key.as_symbol().map(|symbol| Rc::downgrade(&symbol.0));
        self.weak_collection_mut(id)?.entries.insert(
            identity,
            Entry {
                _symbol_key: symbol,
                value,
            },
        );
        Ok(())
    }
}

fn install_kind(vm: &mut Vm, kind: Kind, name: &'static str) {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    let constructor = constructor(vm, name, 0, prototype.clone(), move |_, _, _| {
        Err(JsError::type_error(format!("{name} requires new")))
    });
    replace_construct(vm, constructor, move |vm, _, args, target| {
        let inherited = vm.prototype_from_constructor(target, intrinsic)?;
        let object = vm.allocate_constructed(
            ObjectKind::WeakCollection(Box::new(WeakData::new(kind))),
            inherited,
        )?;
        initialize(vm, object, kind, arg(args, 0))
    });
    if kind == Kind::Map {
        method(vm, prototype.clone(), "get", 1, |vm, this, args| {
            let id = receiver(vm, &this, Kind::Map)?;
            if let Some(key) = TraceTarget::of_value(&arg(args, 0)) {
                Ok(vm.weak_get(id, key)?.unwrap_or_default())
            } else {
                Ok(Value::Undefined)
            }
        });
        for (name, computed) in [("getOrInsert", false), ("getOrInsertComputed", true)] {
            method(vm, prototype.clone(), name, 2, move |vm, this, args| {
                let id = receiver(vm, &this, Kind::Map)?;
                let key = arg(args, 0);
                let identity = required_key(&key)?;
                let supplied = arg(args, 1);
                if computed && !vm.is_callable(&supplied) {
                    return Err(JsError::type_error("WeakMap callback is not callable"));
                }
                if let Some(value) = vm.weak_get(id, identity)? {
                    return Ok(value);
                }
                let value = if computed {
                    vm.invoke(supplied, Value::Undefined, std::slice::from_ref(&key))?
                } else {
                    supplied
                };
                vm.weak_insert(id, &key, identity, value.clone())?;
                Ok(value)
            });
        }
    }
    method(
        vm,
        prototype.clone(),
        kind.adder(),
        if kind == Kind::Map { 2 } else { 1 },
        move |vm, this, args| {
            let id = receiver(vm, &this, kind)?;
            let key = arg(args, 0);
            let identity = required_key(&key)?;
            let value = if kind == Kind::Map {
                arg(args, 1)
            } else {
                Value::Undefined
            };
            vm.weak_insert(id, &key, identity, value)?;
            Ok(this)
        },
    );
    method(vm, prototype.clone(), "has", 1, move |vm, this, args| {
        let id = receiver(vm, &this, kind)?;
        let Some(key) = TraceTarget::of_value(&arg(args, 0)) else {
            return Ok(false.into());
        };
        vm.consume_work(vm.weak_collection(id)?.lookup_work())?;
        Ok(vm.weak_collection(id)?.entries.contains_key(&key).into())
    });
    method(vm, prototype.clone(), "delete", 1, move |vm, this, args| {
        let id = receiver(vm, &this, kind)?;
        let Some(key) = TraceTarget::of_value(&arg(args, 0)) else {
            return Ok(false.into());
        };
        vm.consume_work(vm.weak_collection(id)?.lookup_work())?;
        Ok(vm
            .weak_collection_mut(id)?
            .entries
            .remove(&key)
            .is_some()
            .into())
    });
    tag(vm, prototype, name);
}

pub(super) fn install(vm: &mut Vm) {
    install_kind(vm, Kind::Map, "WeakMap");
    install_kind(vm, Kind::Set, "WeakSet");
}
