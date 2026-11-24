//! Iterator helpers share the same traced iterator records and close rules as
//! for-of. The active state is moved out during callbacks to detect reentry.
mod zip;
use super::{IteratorData, IteratorRecord};
use crate::{
    ErrorKind, JsError, JsString, ObjectId, PropertyKey, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::{ObjectKind, Property, PropertyKind},
};
use std::rc::Rc;

type ResultValue = Result<Value, JsError>;
fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}
fn invalid(s: &str) -> JsError {
    JsError::type_error(s)
}
fn range(s: &str) -> JsError {
    JsError::new(ErrorKind::RangeError, s)
}
fn record(iterator: Value) -> Result<IteratorRecord, JsError> {
    if iterator.as_object().is_none() {
        return Err(invalid("Iterator receiver must be an object"));
    }
    Ok(IteratorRecord {
        iterator,
        next: Value::Undefined,
        done: false,
    })
}
fn get_next(vm: &mut Vm, record: &mut IteratorRecord) -> Result<(), JsError> {
    record.next = vm.get_property(record.iterator.clone(), "next")?;
    Ok(())
}
fn flattenable(vm: &mut Vm, value: Value, strings: bool) -> Result<IteratorRecord, JsError> {
    if value.as_object().is_none() && !(strings && matches!(value, Value::String(_))) {
        return Err(invalid("Expected an iterable object"));
    }
    let method = vm.get_property_key(value.clone(), vm.well_known_key("iterator"))?;
    if !method.is_nullish() {
        return vm.iterator_from_method(value, method);
    }
    let mut r = record(value)?;
    get_next(vm, &mut r)?;
    Ok(r)
}
fn checked<T>(
    vm: &mut Vm,
    r: &mut IteratorRecord,
    result: Result<T, JsError>,
) -> Result<T, JsError> {
    match result {
        Ok(v) => Ok(v),
        Err(e) => Err(vm.iterator_close(r, Err(e)).unwrap_err()),
    }
}

#[derive(Clone, Copy)]
enum Lazy {
    Map,
    Filter,
    FlatMap,
    Drop,
    Take,
    Chunks,
    Windows,
    Concat,
    Zip,
}
#[derive(Clone)]
pub(crate) struct Helper {
    mode: Lazy,
    source: Option<IteratorRecord>,
    inner: Option<IteratorRecord>,
    callback: Value,
    index: usize,
    remaining: f64,
    partial: bool,
    window_full: bool,
    started: bool,
    buffer: Vec<Value>,
    sources: Vec<(Value, Value)>,
    zip: Option<Box<zip::Zip>>,
}
impl Helper {
    pub(super) fn visit(&self, f: &mut impl FnMut(&Value)) {
        for r in self.source.iter().chain(self.inner.iter()) {
            f(&r.iterator);
            f(&r.next);
        }
        if let Some(z) = &self.zip {
            z.visit(f);
        }
        f(&self.callback);
        for v in &self.buffer {
            f(v);
        }
        for (v, m) in &self.sources {
            f(v);
            f(m);
        }
    }
    pub(super) fn extra_bytes(&self) -> usize {
        self.zip.as_deref().map_or(0, zip::Zip::extra_bytes)
            + size_of::<Self>()
            + self.buffer.capacity() * size_of::<Value>()
            + self.sources.capacity() * size_of::<(Value, Value)>()
    }
    fn new(mode: Lazy) -> Self {
        Self {
            mode,
            source: None,
            inner: None,
            callback: Value::Undefined,
            index: 0,
            remaining: 0.,
            partial: false,
            window_full: false,
            started: false,
            buffer: Vec::new(),
            sources: Vec::new(),
            zip: None,
        }
    }
    fn close(&mut self, vm: &mut Vm) -> ResultValue {
        if let Some(z) = &mut self.zip {
            return z.close(vm, Ok(Value::Undefined));
        }
        let mut result = Ok(Value::Undefined);
        if let Some(inner) = &mut self.inner {
            result = vm.iterator_close(inner, result);
        }
        if let Some(source) = &mut self.source {
            result = vm.iterator_close(source, result);
        }
        result
    }
    fn step(&mut self, vm: &mut Vm) -> Result<Option<Value>, JsError> {
        if let Some(z) = &mut self.zip {
            return z.step(vm);
        }
        loop {
            if let Some(inner) = &mut self.inner {
                let result = vm.iterator_step(inner);
                match result {
                    Ok(Some(v)) => return Ok(Some(v)),
                    Ok(None) => self.inner = None,
                    Err(e) => {
                        return if let Some(source) = &mut self.source {
                            checked(vm, source, Err(e))
                        } else {
                            Err(e)
                        };
                    }
                }
            }
            if matches!(self.mode, Lazy::Concat) {
                let Some((v, m)) = self.sources.get(self.index).cloned() else {
                    return Ok(None);
                };
                self.index += 1;
                self.inner = Some(vm.iterator_from_method(v, m)?);
                continue;
            }
            let r = self.source.as_mut().unwrap();
            if matches!(self.mode, Lazy::Take) && self.remaining == 0. {
                vm.iterator_close(r, Ok(Value::Undefined))?;
                return Ok(None);
            }
            if matches!(self.mode, Lazy::Drop) && self.remaining > 0. {
                vm.iterator_skip(r)?;
                self.remaining -= 1.;
                if r.done {
                    return Ok(None);
                }
                continue;
            }
            let value = vm.iterator_step(r)?;
            let Some(v) = value else {
                if matches!(self.mode, Lazy::Chunks | Lazy::Windows)
                    && self.partial
                    && !self.window_full
                    && !self.buffer.is_empty()
                {
                    let value = vm.try_new_array(&self.buffer)?;
                    self.buffer.clear();
                    return Ok(Some(value));
                }
                return Ok(None);
            };
            match self.mode {
                Lazy::Take => {
                    self.remaining -= 1.;
                    return Ok(Some(v));
                }
                Lazy::Drop => return Ok(Some(v)),
                Lazy::Map | Lazy::Filter | Lazy::FlatMap => {
                    let mapped = vm.invoke(
                        self.callback.clone(),
                        Value::Undefined,
                        &[v.clone(), Value::from(self.index)],
                    );
                    self.index += 1;
                    let mapped = checked(vm, r, mapped)?;
                    match self.mode {
                        Lazy::Map => return Ok(Some(mapped)),
                        Lazy::Filter if mapped.is_truthy() => return Ok(Some(v)),
                        Lazy::FlatMap => {
                            let flat = flattenable(vm, mapped, false);
                            self.inner = Some(checked(vm, r, flat)?);
                        }
                        _ => {}
                    }
                }
                Lazy::Chunks | Lazy::Windows => {
                    if self.window_full && matches!(self.mode, Lazy::Windows) {
                        vm.consume_work(self.buffer.len())?;
                        self.buffer.rotate_left(1);
                        *self.buffer.last_mut().unwrap() = v;
                    } else {
                        if self.buffer.len() == self.buffer.capacity() {
                            let n = self.buffer.capacity().max(4);
                            vm.reserve_allocation(n * size_of::<Value>())?;
                            self.buffer.try_reserve_exact(n).map_err(|_| {
                                JsError::new(ErrorKind::HeapLimit, "Iterator buffer limit")
                            })?;
                        }
                        self.buffer.push(v);
                    }
                    if self.buffer.len() == self.remaining as usize {
                        let chunk = vm.try_new_array(&self.buffer)?;
                        if matches!(self.mode, Lazy::Chunks) {
                            self.buffer.clear();
                        } else {
                            self.window_full = true;
                        }
                        return Ok(Some(chunk));
                    }
                }
                Lazy::Concat | Lazy::Zip => unreachable!(),
            }
        }
    }
}

fn resume(vm: &mut Vm, this: Value, close: bool) -> ResultValue {
    let id = this
        .as_object()
        .ok_or_else(|| invalid("Expected an Iterator Helper"))?;
    let state = match &mut vm.heap.object_mut(id)?.kind {
        ObjectKind::Iterator(IteratorData::Helpers(s)) => s
            .take()
            .ok_or_else(|| invalid("Iterator Helper is already running"))?,
        ObjectKind::Iterator(IteratorData::FinishedHelper) => {
            return vm.iterator_result(Value::Undefined, true);
        }
        _ => return Err(invalid("Expected an Iterator Helper")),
    };
    let mut state = state;
    let res = if close {
        if !state.started {
            vm.heap.object_mut(id)?.kind = ObjectKind::Iterator(IteratorData::FinishedHelper);
        }
        state.close(vm).map(|_| None)
    } else {
        state.started = true;
        state.step(vm)
    };
    let next = match &res {
        Ok(Some(_)) => IteratorData::Helpers(Some(state)),
        _ => IteratorData::FinishedHelper,
    };
    vm.heap.object_mut(id)?.kind = ObjectKind::Iterator(next);
    match res? {
        Some(v) => vm.iterator_result(v, false),
        None => vm.iterator_result(Value::Undefined, true),
    }
}
fn helper(vm: &mut Vm, proto: ObjectId, state: Helper) -> ResultValue {
    vm.reserve_allocation(size_of::<Helper>())?;
    vm.allocate_constructed(
        ObjectKind::Iterator(IteratorData::Helpers(Some(Box::new(state)))),
        proto,
    )
}

fn lazy(vm: &mut Vm, this: Value, args: &[Value], kind: Lazy, proto: ObjectId) -> ResultValue {
    let mut r = record(this)?;
    let v = arg(args, 0);
    let mut h = Helper::new(kind);
    let validation = (|| {
        match kind {
            Lazy::Map | Lazy::Filter | Lazy::FlatMap => {
                if !vm.is_callable(&v) {
                    return Err(invalid("Iterator callback must be callable"));
                }
                h.callback = v;
            }
            Lazy::Drop | Lazy::Take => {
                let n = vm.to_number(&v)?;
                if n.is_nan() || n.trunc() < 0. || (n.is_finite() && n > 9_007_199_254_740_991.) {
                    return Err(range("Invalid iterator limit"));
                }
                h.remaining = n.trunc();
            }
            Lazy::Chunks | Lazy::Windows => {
                let Value::Number(n) = v else {
                    return Err(invalid("Chunk size must be an integer"));
                };
                if !n.is_finite() || n.fract() != 0. {
                    return Err(invalid("Chunk size must be an integer"));
                }
                if !(1.0..=u32::MAX as f64).contains(&n) {
                    return Err(range("Chunk size outside valid range"));
                }
                h.remaining = n;
                h.partial = match kind {
                    Lazy::Chunks => true,
                    _ => match arg(args, 1) {
                        Value::Undefined => false,
                        Value::String(s) if s == JsString::from("only-full") => false,
                        Value::String(s) if s == JsString::from("allow-partial") => true,
                        _ => return Err(invalid("Invalid undersized window mode")),
                    },
                };
            }
            Lazy::Concat | Lazy::Zip => unreachable!(),
        }
        Ok(())
    })();
    checked(vm, &mut r, validation)?;
    get_next(vm, &mut r)?;
    h.source = Some(r);
    helper(vm, proto, h)
}

#[derive(Clone, Copy)]
enum Consume {
    Every,
    Some,
    Find,
    ForEach,
    Reduce,
    Array,
    Includes,
    Join,
}
fn consume(vm: &mut Vm, this: Value, args: &[Value], op: Consume) -> ResultValue {
    let mut r = record(this)?;
    let callback = arg(args, 0);
    let mut skip = 0.;
    let mut separator = JsString::from(",");
    let mut text = Vec::new();
    let mut list = Vec::new();
    let mut acc = args.get(1).cloned();
    let validation = (|| {
        match op {
            Consume::Includes => {
                if !arg(args, 1).is_undefined() {
                    let Value::Number(n) = arg(args, 1) else {
                        return Err(invalid("Skipped elements must be a number"));
                    };
                    if n.is_nan() || (n.is_finite() && n.fract() != 0.) {
                        return Err(invalid("Skipped elements must be integral"));
                    }
                    if n < 0. || (n.is_finite() && n > 9_007_199_254_740_991.) {
                        return Err(range("Invalid number of skipped elements"));
                    }
                    skip = n;
                }
            }
            Consume::Join => {
                if !callback.is_undefined() {
                    separator = vm.to_string(&callback)?;
                }
            }
            Consume::Array => {}
            _ => {
                if !vm.is_callable(&callback) {
                    return Err(invalid("Iterator callback must be callable"));
                }
            }
        }
        Ok(())
    })();
    checked(vm, &mut r, validation)?;
    get_next(vm, &mut r)?;
    let mut index = 0usize;
    while let Some(v) = vm.iterator_step(&mut r)? {
        let early = (|| {
            match op {
                Consume::Array => {
                    if list.len() >= vm.limits.max_array_length as usize {
                        return Err(JsError::new(
                            ErrorKind::ExecutionLimit,
                            "Iterator result exceeds array limit",
                        ));
                    }
                    vm.reserve_allocation(2 * size_of::<Value>())?;
                    list.push(v);
                }
                Consume::Includes => {
                    if skip > 0. {
                        skip -= 1.;
                    } else {
                        vm.charge_bigint_comparison(&callback, &v)?;
                        if v == callback
                            || matches!((&v,&callback),(Value::Number(a),Value::Number(b)) if a.is_nan() && b.is_nan())
                        {
                            return Ok(Some(true.into()));
                        }
                    }
                }
                Consume::Join => {
                    let s = if v.is_nullish() {
                        JsString::from("")
                    } else {
                        vm.to_string(&v)?
                    };
                    let sep = if index == 0 {
                        &[][..]
                    } else {
                        separator.as_utf16()
                    };
                    vm.ensure_string_allocation(
                        text.len().saturating_add(sep.len()).saturating_add(s.len()),
                    )?;
                    vm.consume_work(sep.len() + s.len())?;
                    text.extend_from_slice(sep);
                    text.extend_from_slice(s.as_utf16());
                }
                Consume::Reduce => {
                    acc = Some(if let Some(a) = acc.take() {
                        vm.invoke(
                            callback.clone(),
                            Value::Undefined,
                            &[a, v, Value::from(index)],
                        )?
                    } else {
                        v
                    });
                }
                _ => {
                    let val = vm
                        .invoke(
                            callback.clone(),
                            Value::Undefined,
                            &[v.clone(), Value::from(index)],
                        )?
                        .is_truthy();
                    match op {
                        Consume::Find if val => return Ok(Some(v)),
                        Consume::Every if !val => return Ok(Some(false.into())),
                        Consume::Some if val => return Ok(Some(true.into())),
                        _ => {}
                    }
                }
            }
            Ok(None)
        })();
        if let Some(result) = checked(vm, &mut r, early)? {
            return vm.iterator_close(&mut r, Ok(result));
        }
        index += 1;
    }
    match op {
        Consume::Every => Ok(true.into()),
        Consume::Some | Consume::Includes => Ok(false.into()),
        Consume::Array => vm.try_new_array(&list),
        Consume::Join => vm.finish_string(text),
        Consume::Reduce => acc.ok_or_else(|| invalid("Reduce of empty iterator")),
        _ => Ok(Value::Undefined),
    }
}

fn builtin_accessor(vm: &mut Vm, home: &Value, key: PropertyKey, name: &str, value: Value) {
    let g = vm.native(
        &format!("get {name}"),
        0,
        false,
        Rc::new(move |_, _, _| Ok(value.clone())),
    );
    let proto = home.clone();
    let setter_key = key.clone();
    let s = vm.native(
        &format!("set {name}"),
        1,
        false,
        Rc::new(move |vm, this, args| {
            let id = this
                .as_object()
                .filter(|_| this != proto)
                .ok_or_else(|| invalid("Invalid iterator setter receiver"))?;
            let val = arg(args, 0);
            if vm.own_descriptor_key(id, &setter_key)?.is_some() {
                vm.set_property_key(this, setter_key.clone(), val)?;
            } else {
                vm.define_key(this, setter_key.clone(), Property::data(val))?;
            }
            Ok(Value::Undefined)
        }),
    );
    vm.define_key(
        home.clone(),
        key,
        Property {
            kind: PropertyKind::Accessor {
                get: Some(g),
                set: Some(s),
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
}

pub(super) fn install(vm: &mut Vm, proto: &Value) {
    let ctor = constructor(vm, "Iterator", 0, proto.clone(), |_, _, _| {
        Err(invalid("Iterator is abstract"))
    });
    vm.root(ctor.clone());
    replace_construct(vm, ctor.clone(), |vm, active, _, target| {
        if active == target {
            return Err(invalid("Iterator is abstract"));
        }
        let prototype =
            vm.prototype_from_constructor(target, vm.realm.iterator_prototype.unwrap())?;
        vm.allocate_constructed(ObjectKind::Ordinary, prototype)
    });
    builtin_accessor(
        vm,
        proto,
        PropertyKey::String("constructor".into()),
        "constructor",
        ctor.clone(),
    );
    builtin_accessor(
        vm,
        proto,
        vm.well_known_key("toStringTag"),
        "[Symbol.toStringTag]",
        "Iterator".into(),
    );
    let dispose = vm.native(
        "[Symbol.dispose]",
        0,
        false,
        Rc::new(|vm, this, _| {
            let ret = vm.get_property(this.clone(), "return")?;
            if !ret.is_nullish() {
                vm.invoke(ret, this, &[])?;
            }
            Ok(Value::Undefined)
        }),
    );
    vm.define_data_property_key(
        proto.clone(),
        vm.well_known_key("dispose"),
        dispose,
        true,
        false,
        true,
    )
    .unwrap();
    let helper_proto = vm.new_object_with_prototype(proto.clone()).unwrap();
    vm.root(helper_proto.clone());
    vm.define_data_property_key(
        helper_proto.clone(),
        vm.well_known_key("toStringTag"),
        "Iterator Helper".into(),
        false,
        false,
        true,
    )
    .unwrap();
    method(vm, helper_proto.clone(), "next", 0, |vm, this, _| {
        resume(vm, this, false)
    });
    method(vm, helper_proto.clone(), "return", 0, |vm, this, _| {
        resume(vm, this, true)
    });
    let hid = helper_proto.as_object().unwrap();
    for (name, op) in [
        ("map", Lazy::Map),
        ("filter", Lazy::Filter),
        ("drop", Lazy::Drop),
        ("take", Lazy::Take),
        ("flatMap", Lazy::FlatMap),
        ("chunks", Lazy::Chunks),
        ("windows", Lazy::Windows),
    ] {
        method(vm, proto.clone(), name, 1, move |vm, this, args| {
            lazy(vm, this, args, op, hid)
        });
    }
    for (name, op, len) in [
        ("every", Consume::Every, 1),
        ("some", Consume::Some, 1),
        ("find", Consume::Find, 1),
        ("forEach", Consume::ForEach, 1),
        ("reduce", Consume::Reduce, 1),
        ("includes", Consume::Includes, 1),
        ("join", Consume::Join, 1),
        ("toArray", Consume::Array, 0),
    ] {
        method(vm, proto.clone(), name, len, move |vm, this, args| {
            consume(vm, this, args, op)
        });
    }
    let wrapper = vm.new_object_with_prototype(proto.clone()).unwrap();
    vm.root(wrapper.clone());
    for (name, ret) in [("next", false), ("return", true)] {
        method(vm, wrapper.clone(), name, 0, move |vm, this, _| {
            let r = match &vm
                .heap
                .object(
                    this.as_object()
                        .ok_or_else(|| invalid("Expected iterator wrapper"))?,
                )?
                .kind
            {
                ObjectKind::Iterator(IteratorData::Wrapper(r)) => r.clone(),
                _ => return Err(invalid("Expected iterator wrapper")),
            };
            if ret {
                let close = vm.get_property(r.iterator.clone(), "return")?;
                if close.is_nullish() {
                    return vm.iterator_result(Value::Undefined, true);
                }
                vm.invoke(close, r.iterator, &[])
            } else {
                vm.invoke(r.next, r.iterator, &[])
            }
        });
    }
    let wid = wrapper.as_object().unwrap();
    let prototype = proto.clone();
    method(vm, ctor.clone(), "from", 1, move |vm, _, args| {
        let r = flattenable(vm, arg(args, 0), true)?;
        if vm.has_prototype(&r.iterator, &prototype)? {
            return Ok(r.iterator);
        }
        vm.allocate_constructed(
            ObjectKind::Iterator(IteratorData::Wrapper(Box::new(r))),
            wid,
        )
    });
    zip::install(vm, &ctor, hid);
    method(vm, ctor, "concat", 0, move |vm, _, args| {
        let mut h = Helper::new(Lazy::Concat);
        vm.reserve_allocation(args.len() * size_of::<(Value, Value)>())?;
        for item in args {
            vm.tick()?;
            if item.as_object().is_none() {
                return Err(invalid("Iterator.concat expects iterable objects"));
            }
            let m = vm.get_property_key(item.clone(), vm.well_known_key("iterator"))?;
            if !vm.is_callable(&m) {
                return Err(invalid("Object is not iterable"));
            }
            h.sources.push((item.clone(), m));
        }
        helper(vm, hid, h)
    });
}
