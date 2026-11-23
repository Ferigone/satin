//! URL and URLSearchParams share one record so either view can update the
//! query without changing the identity of the other JavaScript object.
use super::{arg, getter, webidl};
use rust_browser_javascript::{ErrorKind, JsError, PropertyKey, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use url::{Url, form_urlencoded, quirks};

const INPUT_BYTES: usize = 128 * 1024;
const PARAM_BYTES: usize = 512 * 1024;
const MAX_PAIRS: usize = 8192;
const RETAINED_BYTES: usize = 4 * 1024 * 1024;
type Pairs = Vec<(String, String)>;
type Shared = Rc<RefCell<Record>>;

struct Record {
    address: Option<Url>,
    pairs: Pairs,
    resident: Rc<Cell<usize>>,
    reserved: usize,
}
impl Record {
    fn new(
        address: Option<Url>,
        pairs: Pairs,
        resident: Rc<Cell<usize>>,
    ) -> Result<Shared, JsError> {
        let mut record = Self {
            address: None,
            pairs: vec![],
            resident,
            reserved: 0,
        };
        record.replace(address, pairs)?;
        Ok(Rc::new(RefCell::new(record)))
    }

    fn replace(&mut self, address: Option<Url>, pairs: Pairs) -> Result<(), JsError> {
        let pair_bytes = pairs_size(&pairs);
        let address_bytes = address.as_ref().map_or(0, |url| url.as_str().len());
        if pairs.len() > MAX_PAIRS || pair_bytes > PARAM_BYTES || address_bytes > PARAM_BYTES {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "URL record exceeds its size budget",
            ));
        }
        let new_reserved = 256 + address_bytes * 2 + pair_bytes;
        let new_resident = self.resident.get() - self.reserved + new_reserved;
        if new_resident > RETAINED_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Retained URL data exceeds 4 MiB",
            ));
        }
        self.resident.set(new_resident);
        self.reserved = new_reserved;
        self.address = address;
        self.pairs = pairs;
        Ok(())
    }

    fn edit_params(&mut self, vm: &mut Vm, edit: impl FnOnce(&mut Pairs)) -> Result<(), JsError> {
        vm.charge_work(self.reserved)?;
        let mut pairs = self.pairs.clone();
        edit(&mut pairs);
        if pairs.len() > MAX_PAIRS || pairs_size(&pairs) > PARAM_BYTES {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "URL parameters exceed their size budget",
            ));
        }
        let mut address = self.address.clone();
        if let Some(address) = &mut address {
            let search = serialize(&pairs);
            address.set_query((!search.is_empty()).then_some(search.as_str()));
        }
        self.replace(address, pairs)
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.reserved);
    }
}

struct UrlData(Shared);
struct ParamsData(Shared);

fn pairs_size(pairs: &[(String, String)]) -> usize {
    pairs
        .iter()
        .map(|(key, value)| key.capacity() + value.capacity() + 64)
        .sum()
}

fn parse_pairs(source: &str) -> Result<Pairs, JsError> {
    let mut pairs = Vec::new();
    let mut bytes = 0;
    for (key, value) in form_urlencoded::parse(source.as_bytes()) {
        bytes += key.len() + value.len() + 64;
        if pairs.len() == MAX_PAIRS || bytes > PARAM_BYTES {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "URL parameters exceed their size budget",
            ));
        }
        pairs.push((key.into_owned(), value.into_owned()));
    }
    Ok(pairs)
}

fn serialize(pairs: &[(String, String)]) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(
        pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    serializer.finish()
}

/// Fetch's BodyInit conversion uses the internal tuples regardless of how the
/// page configures the object's toString or iterator.
pub(super) fn form_body(vm: &mut Vm, object: &Value) -> Result<Option<String>, JsError> {
    let Some(data) = vm.host_data::<ParamsData>(object) else {
        return Ok(None);
    };
    let data = data.0.borrow();
    vm.charge_work(pairs_size(&data.pairs))?;
    Ok(Some(serialize(&data.pairs)))
}

fn url_data(vm: &Vm, object: &Value) -> Result<Shared, JsError> {
    vm.host_data::<UrlData>(object)
        .map(|data| data.0.clone())
        .ok_or_else(|| JsError::type_error("Illegal URL receiver"))
}

fn params_data(vm: &Vm, object: &Value) -> Result<Shared, JsError> {
    vm.host_data::<ParamsData>(object)
        .map(|data| data.0.clone())
        .ok_or_else(|| JsError::type_error("Illegal URLSearchParams receiver"))
}

fn parse_address(vm: &mut Vm, args: &[Value]) -> Result<Option<Url>, JsError> {
    webidl::required(args, 1, "URL")?;
    let address = webidl::usv_string(vm, &args[0], INPUT_BYTES)?;
    let base = if let Some(base) = args.get(1).filter(|value| !value.is_undefined()) {
        let base = webidl::usv_string(vm, base, INPUT_BYTES)?;
        let Ok(base) = Url::parse(&base) else {
            return Ok(None);
        };
        Some(base)
    } else {
        None
    };
    Ok(Url::options()
        .base_url(base.as_ref())
        .parse(&address)
        .ok()
        .map(canonical_opaque_path))
}

fn canonical_opaque_path(url: Url) -> Url {
    if !url.cannot_be_a_base()
        || !url.path().ends_with(' ')
        || (url.query().is_none() && url.fragment().is_none())
    {
        return url;
    }
    // The final opaque-path space before '?' or '#' is percent encoded.
    // Keeping it encoded also preserves the previous spaces when the query
    // or fragment is removed.
    let path_end_start = url[..url::Position::AfterPath].len() - 1;
    let serialized = url.as_str();
    let mut adjusted = String::with_capacity(serialized.len() + 2);
    adjusted.push_str(&serialized[..path_end_start]);
    adjusted.push_str("%20");
    adjusted.push_str(&serialized[path_end_start + 1..]);
    Url::parse(&adjusted).unwrap_or(url)
}
#[derive(Clone)]
struct Host {
    url_proto: Value,
    params_proto: Value,
    resident: Rc<Cell<usize>>,
    property_is_enumerable: Value,
}
impl Host {
    fn url(&self, vm: &mut Vm, address: Url, new_target: Option<Value>) -> Result<Value, JsError> {
        let pairs = parse_pairs(address.query().unwrap_or(""))?;
        let record = Record::new(Some(address), pairs, self.resident.clone())?;
        let object = match new_target {
            Some(new_target) => webidl::instance(vm, new_target, &self.url_proto)?,
            None => vm.new_object_with_prototype(self.url_proto.clone())?,
        };
        // Allocate the wrapper lazily in the same realm, but keep it attached
        // once JavaScript can observe its identity.
        vm.set_host_data(object.clone(), Rc::new(UrlData(record)))?;
        Ok(object)
    }

    fn init_params(&self, vm: &mut Vm, init: Value) -> Result<Pairs, JsError> {
        if init.is_undefined() {
            return Ok(vec![]);
        }
        if init.as_object().is_none() {
            let value = webidl::usv_string(vm, &init, INPUT_BYTES)?;
            return parse_pairs(value.strip_prefix('?').unwrap_or(&value));
        }
        let iterator = webidl::iterator_method(vm, &init)?;
        let mut bytes = 0;
        if !iterator.is_nullish() {
            return webidl::sequence(vm, init, iterator, MAX_PAIRS, |vm, pair| {
                if pair.as_object().is_none() {
                    return Err(JsError::type_error(
                        "Each URL parameter must be an iterable pair",
                    ));
                }
                let iterator = webidl::iterator_method(vm, &pair)?;
                let converted = webidl::sequence(vm, pair, iterator, MAX_PAIRS, |vm, part| {
                    let part = webidl::usv_string(vm, &part, INPUT_BYTES)?;
                    bytes += part.capacity() + 32;
                    if bytes > PARAM_BYTES {
                        return Err(JsError::new(
                            ErrorKind::ExecutionLimit,
                            "URL parameters exceed their byte budget",
                        ));
                    }
                    Ok(part)
                })?;
                let mut converted = converted.into_iter();
                match (converted.next(), converted.next(), converted.next()) {
                    (Some(name), Some(value), None) => Ok((name, value)),
                    _ => Err(JsError::type_error(
                        "Each URL parameter must contain two items",
                    )),
                }
            });
        }
        let mut pairs = Pairs::new();
        let mut positions: BTreeMap<String, usize> = BTreeMap::new();
        for key in vm.own_property_keys(init.clone())? {
            let key_value = match &key {
                PropertyKey::String(string) => Value::String(string.clone()),
                PropertyKey::Symbol(symbol) => Value::Symbol(symbol.clone()),
            };
            if !vm
                .call(
                    self.property_is_enumerable.clone(),
                    init.clone(),
                    std::slice::from_ref(&key_value),
                )?
                .is_truthy()
            {
                continue;
            }
            let key_string = webidl::usv_string(vm, &key_value, INPUT_BYTES)?;
            let value = vm.get_property_key(init.clone(), key)?;
            let value = webidl::usv_string(vm, &value, INPUT_BYTES)?;
            bytes += key_string.capacity() + value.capacity() + 64;
            if pairs.len() == MAX_PAIRS || bytes > PARAM_BYTES {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "URL parameters exceed their size budget",
                ));
            }
            // USVString conversion can coalesce keys that differ only by a
            // lone surrogate. An IDL record keeps the first key's position.
            if let Some(&position) = positions.get(&key_string) {
                pairs[position].1 = value;
            } else {
                positions.insert(key_string.clone(), pairs.len());
                pairs.push((key_string, value));
            }
        }
        Ok(pairs)
    }
}

#[derive(Clone, Copy)]
enum Field {
    Href,
    Protocol,
    Username,
    Password,
    Host,
    Hostname,
    Port,
    Pathname,
    Search,
    Hash,
}
impl Field {
    fn get(self, url: &Url) -> String {
        match self {
            Self::Href => quirks::href(url).into(),
            Self::Protocol => quirks::protocol(url).into(),
            Self::Username => quirks::username(url).into(),
            Self::Password => quirks::password(url).into(),
            Self::Host => quirks::host(url).into(),
            Self::Hostname => quirks::hostname(url).into(),
            Self::Port => quirks::port(url).into(),
            Self::Pathname => quirks::pathname(url).into(),
            Self::Search => quirks::search(url).into(),
            Self::Hash => quirks::hash(url).into(),
        }
    }

    fn set(self, url: &mut Url, value: &str) -> Result<(), JsError> {
        match self {
            Self::Href => {
                let new_url = Url::parse(value).map_err(|_| JsError::type_error("Invalid URL"))?;
                *url = canonical_opaque_path(new_url);
            }
            Self::Protocol => {
                let _ = quirks::set_protocol(url, value);
            }
            Self::Username => {
                let _ = quirks::set_username(url, value);
            }
            Self::Password => {
                let _ = quirks::set_password(url, value);
            }
            Self::Host => {
                let _ = quirks::set_host(url, value);
            }
            Self::Hostname => {
                let _ = quirks::set_hostname(url, value);
            }
            Self::Port => {
                let _ = quirks::set_port(url, value);
            }
            Self::Pathname => quirks::set_pathname(url, value),
            Self::Search => quirks::set_search(url, value),
            Self::Hash => quirks::set_hash(url, value),
        }
        Ok(())
    }
}

pub(super) fn install(vm: &mut Vm) -> Result<(), JsError> {
    let object = vm.get_global("Object")?;
    let object_proto = vm.get_property(object.clone(), "prototype")?;
    let property_is_enumerable = vm.get_property(object_proto, "propertyIsEnumerable")?;
    let host = Host {
        url_proto: vm.new_object(),
        params_proto: vm.new_object(),
        resident: Rc::new(Cell::new(0)),
        property_is_enumerable,
    };
    // Constructor callbacks and native factories use these captured intrinsics
    // even if a page replaces the corresponding globals.
    vm.root(host.url_proto.clone());
    vm.root(host.params_proto.clone());
    vm.root(host.property_is_enumerable.clone());
    install_url(vm, &host)?;
    install_params(vm, &host)?;
    install_iterators(vm, &host, object)
}

fn install_url(vm: &mut Vm, host: &Host) -> Result<(), JsError> {
    let create = host.clone();
    let ctor = webidl::interface(
        vm,
        host.url_proto.clone(),
        "URL",
        1,
        move |vm, _, args, new_target| {
            let new_target = new_target.ok_or_else(|| JsError::type_error("URL requires new"))?;
            let address =
                parse_address(vm, args)?.ok_or_else(|| JsError::type_error("Invalid URL"))?;
            create.url(vm, address, Some(new_target))
        },
    )?;
    webidl::operation(vm, ctor.clone(), "canParse", 1, |vm, _, args| {
        Ok(parse_address(vm, args)?.is_some().into())
    })?;
    let create = host.clone();
    webidl::operation(
        vm,
        ctor,
        "parse",
        1,
        move |vm, _, args| match parse_address(vm, args)? {
            Some(address) => create.url(vm, address, None),
            None => Ok(Value::Null),
        },
    )?;
    for (name, field) in [
        ("href", Field::Href),
        ("protocol", Field::Protocol),
        ("username", Field::Username),
        ("password", Field::Password),
        ("host", Field::Host),
        ("hostname", Field::Hostname),
        ("port", Field::Port),
        ("pathname", Field::Pathname),
        ("search", Field::Search),
        ("hash", Field::Hash),
    ] {
        let get = vm.new_native_function(&format!("get {name}"), move |vm, this, _| {
            let record = url_data(vm, &this)?;
            let record = record.borrow();
            let value = field.get(record.address.as_ref().unwrap());
            vm.charge_work(value.len())?;
            Ok(value.into())
        });
        let set = vm.new_native_function(&format!("set {name}"), move |vm, this, args| {
            let record = url_data(vm, &this)?;
            let value = webidl::usv_string(vm, &arg(args, 0), INPUT_BYTES)?;
            let mut record = record.borrow_mut();
            vm.charge_work(record.reserved)?;
            let mut url = record.address.clone().unwrap();
            field.set(&mut url, &value)?;
            let pairs = if matches!(field, Field::Href | Field::Search) {
                parse_pairs(url.query().unwrap_or(""))?
            } else {
                record.pairs.clone()
            };
            record.replace(Some(url), pairs)?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(host.url_proto.clone(), name, Some(get), Some(set))?;
    }
    getter(vm, host.url_proto.clone(), "origin", |vm, this, _| {
        let record = url_data(vm, &this)?;
        let record = record.borrow();
        vm.charge_work(record.address.as_ref().unwrap().as_str().len())?;
        Ok(quirks::origin(record.address.as_ref().unwrap()).into())
    })?;
    let params_proto = host.params_proto.clone();
    getter(
        vm,
        host.url_proto.clone(),
        "searchParams",
        move |vm, this, _| {
            let record = url_data(vm, &this)?;
            if let Some(existing) = vm.host_slot(&this, 0) {
                return Ok(existing);
            }
            let params = vm.new_object_with_prototype(params_proto.clone())?;
            vm.set_host_data(params.clone(), Rc::new(ParamsData(record)))?;
            vm.set_host_slot(this, 0, params.clone())?;
            Ok(params)
        },
    )?;
    for name in ["toString", "toJSON"] {
        webidl::operation(vm, host.url_proto.clone(), name, 0, |vm, this, _| {
            let record = url_data(vm, &this)?;
            let record = record.borrow();
            let address = record.address.as_ref().unwrap().as_str();
            vm.charge_work(address.len())?;
            Ok(address.into())
        })?;
    }
    Ok(())
}

fn name_and_filter(vm: &mut Vm, args: &[Value]) -> Result<(String, Option<String>), JsError> {
    webidl::required(args, 1, "URLSearchParams operation")?;
    let name = webidl::usv_string(vm, &args[0], INPUT_BYTES)?;
    let value = args
        .get(1)
        .filter(|value| !value.is_undefined())
        .map(|value| webidl::usv_string(vm, value, INPUT_BYTES))
        .transpose()?;
    Ok((name, value))
}

fn install_params(vm: &mut Vm, host: &Host) -> Result<(), JsError> {
    let create = host.clone();
    let proto = &host.params_proto;
    webidl::interface(
        vm,
        proto.clone(),
        "URLSearchParams",
        0,
        move |vm, _, args, new_target| {
            let target =
                new_target.ok_or_else(|| JsError::type_error("URLSearchParams requires new"))?;
            let pairs = create.init_params(vm, arg(args, 0))?;
            let object = webidl::instance(vm, target, &create.params_proto)?;
            let record = Record::new(None, pairs, create.resident.clone())?;
            vm.set_host_data(object.clone(), Rc::new(ParamsData(record)))?;
            Ok(object)
        },
    )?;
    getter(vm, proto.clone(), "size", |vm, this, _| {
        Ok((params_data(vm, &this)?.borrow().pairs.len() as u32).into())
    })?;
    for (name, append) in [("append", true), ("set", false)] {
        webidl::operation(vm, proto.clone(), name, 2, move |vm, this, args| {
            let data = params_data(vm, &this)?;
            webidl::required(args, 2, name)?;
            let key = webidl::usv_string(vm, &args[0], INPUT_BYTES)?;
            let value = webidl::usv_string(vm, &args[1], INPUT_BYTES)?;
            data.borrow_mut().edit_params(vm, |pairs| {
                let first = (!append)
                    .then(|| pairs.iter().position(|(k, _)| *k == key))
                    .flatten();
                if let Some(first) = first {
                    pairs[first].1 = value;
                    let mut index = 0;
                    pairs.retain(|(k, _)| {
                        let keep = index <= first || *k != key;
                        index += 1;
                        keep
                    });
                } else {
                    pairs.push((key, value));
                }
            })?;
            Ok(Value::Undefined)
        })?;
    }
    webidl::operation(vm, proto.clone(), "delete", 1, |vm, this, args| {
        let data = params_data(vm, &this)?;
        let (key, value) = name_and_filter(vm, args)?;
        data.borrow_mut().edit_params(vm, |pairs| {
            pairs.retain(|(k, v)| *k != key || value.as_ref().is_some_and(|filter| filter != v));
        })?;
        Ok(Value::Undefined)
    })?;
    webidl::operation(vm, proto.clone(), "has", 1, |vm, this, args| {
        let data = params_data(vm, &this)?;
        let (key, value) = name_and_filter(vm, args)?;
        let data = data.borrow();
        vm.charge_work(pairs_size(&data.pairs))?;
        Ok(data
            .pairs
            .iter()
            .any(|(k, v)| *k == key && value.as_ref().is_none_or(|value| value == v))
            .into())
    })?;
    for (name, all) in [("get", false), ("getAll", true)] {
        webidl::operation(vm, proto.clone(), name, 1, move |vm, this, args| {
            let data = params_data(vm, &this)?;
            webidl::required(args, 1, name)?;
            let key = webidl::usv_string(vm, &args[0], INPUT_BYTES)?;
            let data = data.borrow();
            vm.charge_work(pairs_size(&data.pairs))?;
            let mut values = data
                .pairs
                .iter()
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| Value::from(v.as_str()));
            if all {
                Ok(vm.new_array(&values.collect::<Vec<_>>()))
            } else {
                Ok(values.next().unwrap_or(Value::Null))
            }
        })?;
    }
    webidl::operation(vm, proto.clone(), "sort", 0, |vm, this, _| {
        let data = params_data(vm, &this)?;
        let mut data = data.borrow_mut();
        let work = data
            .pairs
            .iter()
            .map(|(key, _)| key.len() + 1)
            .sum::<usize>();
        vm.charge_work(work.saturating_mul(data.pairs.len().max(1).ilog2() as usize))?;
        data.edit_params(vm, |pairs| {
            pairs.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()))
        })?;
        Ok(Value::Undefined)
    })?;
    webidl::operation(vm, proto.clone(), "toString", 0, |vm, this, _| {
        Ok(form_body(vm, &this)?
            .ok_or_else(|| JsError::type_error("Illegal URLSearchParams receiver"))?
            .into())
    })?;
    webidl::operation(vm, proto.clone(), "forEach", 1, |vm, this, args| {
        let data = params_data(vm, &this)?;
        webidl::required(args, 1, "URLSearchParams.forEach")?;
        if !vm.is_callable(&args[0]) {
            return Err(JsError::type_error(
                "URLSearchParams.forEach callback is not callable",
            ));
        }
        let mut index = 0;
        loop {
            let pair = {
                let data = data.borrow();
                data.pairs
                    .get(index)
                    .map(|(key, value)| {
                        vm.charge_work(key.len() + value.len() + 1)?;
                        Ok::<_, JsError>((Value::from(key.as_str()), Value::from(value.as_str())))
                    })
                    .transpose()?
            };
            let Some((key, value)) = pair else {
                break;
            };
            vm.call(args[0].clone(), arg(args, 1), &[value, key, this.clone()])?;
            index += 1;
        }
        Ok(Value::Undefined)
    })?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Iteration {
    Entries,
    Keys,
    Values,
}
struct ParamsIterator {
    record: Shared,
    index: Cell<usize>,
    kind: Iteration,
}

fn install_iterators(vm: &mut Vm, host: &Host, object_ctor: Value) -> Result<(), JsError> {
    let get_proto = vm.get_property(object_ctor, "getPrototypeOf")?;
    let array_ctor = vm.get_global("Array")?;
    let array_proto = vm.get_property(array_ctor, "prototype")?;
    let values = vm.get_property(array_proto, "values")?;
    let array = vm.new_array(&[]);
    let sample = vm.call(values, array, &[])?;
    let array_iterator_proto = vm.call(get_proto.clone(), Value::Undefined, &[sample])?;
    let inherited = vm.call(get_proto, Value::Undefined, &[array_iterator_proto])?;
    let prototype = vm.new_object_with_prototype(inherited)?;
    vm.root(prototype.clone());
    webidl::tag(vm, prototype.clone(), "URLSearchParams Iterator")?;
    webidl::operation(vm, prototype.clone(), "next", 0, |vm, this, _| {
        let iterator = vm
            .host_data::<ParamsIterator>(&this)
            .ok_or_else(|| JsError::type_error("Illegal URLSearchParams iterator"))?;
        let index = iterator.index.get();
        let record = iterator.record.borrow();
        let mut value = Value::Undefined;
        let done = if let Some((key, param)) = record.pairs.get(index) {
            vm.charge_work(key.len() + param.len() + 1)?;
            value = match iterator.kind {
                Iteration::Entries => vm.new_array(&[key.as_str().into(), param.as_str().into()]),
                Iteration::Keys => key.as_str().into(),
                Iteration::Values => param.as_str().into(),
            };
            iterator.index.set(index + 1);
            false
        } else {
            true
        };
        drop(record);
        let result = vm.new_object();
        vm.define_data_property(result.clone(), "value", value, true, true, true)?;
        vm.define_data_property(result.clone(), "done", done.into(), true, true, true)?;
        Ok(result)
    })?;
    for (name, kind) in [
        ("entries", Iteration::Entries),
        ("keys", Iteration::Keys),
        ("values", Iteration::Values),
    ] {
        let prototype = prototype.clone();
        let function = webidl::operation(
            vm,
            host.params_proto.clone(),
            name,
            0,
            move |vm, this, _| {
                let record = params_data(vm, &this)?;
                let iterator = vm.new_object_with_prototype(prototype.clone())?;
                vm.set_host_data(
                    iterator.clone(),
                    Rc::new(ParamsIterator {
                        record,
                        index: Cell::new(0),
                        kind,
                    }),
                )?;
                vm.set_host_slot(iterator.clone(), 0, this)?;
                Ok(iterator)
            },
        )?;
        if matches!(kind, Iteration::Entries) {
            vm.define_data_property_key(
                host.params_proto.clone(),
                webidl::well_known_key(vm, "iterator"),
                function,
                true,
                false,
                true,
            )?;
        }
    }
    Ok(())
}
