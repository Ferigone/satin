//! ECMA-402 bindings. Locale data and algorithms come from ICU4X and CLDR.
use crate::{ErrorKind, JsError, ObjectId, Value, Vm, builtins::{method, replace_construct}, heap::{ObjectKind, Property, PropertyKind}};
use icu_locale::Locale;
use std::rc::Rc;

mod locale;
mod data;
mod datetime;
pub(crate) use datetime::format_date;
mod services;

/// Resolve the case-preserved IANA identifier and its primary identifier.
pub(crate) fn named_time_zone(name: &str) -> Option<(&'static str, &'static str)> {
    let lower = name.to_ascii_lowercase();
    let primary = locale::table(data::ZONE_ALIASES, &lower)?;
    let identifier = locale::table(data::ZONE_LINK_NAMES, &lower).unwrap_or(primary);
    Some((identifier, primary))
}

pub(super) fn install(vm: &mut Vm) {
    let intl = vm.new_object();
    prop(vm, vm.global(), "Intl", intl.clone());
    tag(vm, intl.clone(), "Intl");
    method(vm, intl.clone(), "getCanonicalLocales", 1, |vm, _, args| {
        let locales = locale::list(vm, arg(args, 0))?;
        strings(vm, locales.iter().map(ToString::to_string))
    });
    method(vm, intl.clone(), "supportedValuesOf", 1, |vm, _, args| {
        let name = vm.to_string(&arg(args, 0))?.to_string_lossy();
        if name == "timeZone" {
            return strings(vm, data::ZONES.iter().copied());
        }
        let values = match name.as_str() {
            "calendar" => data::CALENDARS,
            "collation" => data::COLLATIONS,
            "currency" => data::CURRENCIES,
            "numberingSystem" => data::NUMBERING,
            "unit" => data::UNITS,
            _ => return Err(range("Invalid Intl enumeration key")),
        };
        strings(vm, values.iter().copied())
    });
    locale::install(vm, intl.clone());
    services::install(vm, intl.clone());
    datetime::install(vm, intl);
}

type ResultValue = Result<Value, JsError>;
type Initializer = fn(&mut Vm, &[Value], ObjectId) -> ResultValue;

fn range(msg: &str) -> JsError { JsError::new(ErrorKind::RangeError, msg) }
fn arg(args: &[Value], at: usize) -> Value { args.get(at).cloned().unwrap_or_default() }
fn strings<I, S>(vm: &mut Vm, items: I) -> ResultValue where I: IntoIterator<Item=S>, S: Into<String> {
    let values: Vec<_> = items.into_iter().map(|s| Value::from(s.into())).collect();
    vm.try_new_array(&values)
}
fn prop(vm: &mut Vm, obj: Value, key: &str, value: Value) {
    vm.define_data_property(obj,key,value,true,false,true).expect("installing Intl intrinsic");
}
fn field(vm: &mut Vm, obj: &Value, key: &str, value: impl Into<Value>) -> Result<(), JsError> {
    vm.define_data_property(obj.clone(),key,value.into(),true,true,true)
}
fn tag(vm: &mut Vm, obj: Value, name: &str) {
    vm.define_data_property_key(obj, vm.well_known_key("toStringTag"), name.into(), false, false, true).unwrap();
}
fn prototype(vm: &Vm, name: &str) -> ObjectId {
    vm.realm.intl_prototypes.iter().find(|(n,_)| *n==name).unwrap().1
}
fn class(vm: &mut Vm, intl: Value, name: &'static str, length: usize, callable: bool, init: Initializer) -> (Value, Value) {
    let proto=vm.new_object();
    vm.realm.intl_prototypes.push((name,proto.as_object().unwrap()));
    let ctor=vm.native(name,length,true,Rc::new(move |vm,_,args| {
        if !callable {return Err(JsError::type_error("Intl constructor requires new"))}
        init(vm,args,prototype(vm,name))
    }));
    replace_construct(vm,ctor.clone(),move |vm,_,args,target| {
        let proto = vm.get_property(target.clone(),"prototype")?.as_object();
        let proto=match proto {Some(p)=>p,None=>vm.with_function_realm(target.as_object().unwrap(),|vm|Ok(prototype(vm,name)))?};
        init(vm,args,proto)
    });
    vm.define_data_property(ctor.clone(),"prototype",proto.clone(),false,false,false).unwrap();
    prop(vm,proto.clone(),"constructor",ctor.clone());
    tag(vm, proto.clone(),&format!("Intl.{name}"));
    prop(vm,intl,name,ctor.clone());
    (ctor,proto)
}
fn accessor<F>(vm: &mut Vm, obj: &Value, name: &str, callback: F)
where F: Fn(&mut Vm, Value, &[Value])->ResultValue+'static {
    let getter=vm.native(&format!("get {name}"),0,false,Rc::new(callback));
    vm.define_property(obj.clone(), name.into(), Property {kind:PropertyKind::Accessor{get:Some(getter),set:None},enumerable:false,configurable:true}).unwrap();
}
fn create<T:'static>(vm: &mut Vm, proto:ObjectId, slots:T)->ResultValue {
    let obj=vm.allocate_constructed(ObjectKind::Intl,proto)?;
    vm.set_host_data(obj.clone(),Rc::new(slots))?;
    Ok(obj)
}
fn slots<T:'static>(vm:&Vm, object:&Value)->Result<Rc<T>,JsError> {
    vm.host_data(object).ok_or_else(||JsError::type_error("Incompatible Intl receiver"))
}
fn options(vm:&mut Vm, value:Value, coerce:bool)->ResultValue {
    if matches!(value,Value::Undefined) {return Ok(value)}
    if coerce {return vm.coerce_object(value)}
    if value.as_object().is_none() {return Err(JsError::type_error("Options must be an object"))}
    Ok(value)
}
fn get(vm:&mut Vm,obj:&Value,key:&str)->ResultValue {
    if matches!(obj,Value::Undefined) {Ok(Value::Undefined)} else {vm.get_property(obj.clone(),key)}
}
fn opt(vm:&mut Vm,obj:&Value,key:&str,valid:&[&str])->Result<Option<String>,JsError>{
    let v=get(vm,obj,key)?;
    if matches!(v,Value::Undefined) {return Ok(None)}
    let s=vm.to_string(&v)?.to_string_lossy();
    if !valid.is_empty() && !valid.contains(&s.as_str()){return Err(range(&format!("Invalid {key} option")))}
    Ok(Some(s))
}
fn supported(vm:&mut Vm, ctor:&Value) {
    method(vm,ctor.clone(),"supportedLocalesOf",1,|vm,_,args|{
        let locales=locale::list(vm,arg(args,0))?;
        let opts=options(vm,arg(args,1),true)?;
        opt(vm,&opts,"localeMatcher",&["lookup","best fit"])?;
        strings(vm,locales.into_iter().filter(|l| locale::lookup(l).is_some()).map(|l|l.to_string()))
    });
}
fn resolve(requested:&[Locale])->Locale {
    requested.iter().find(|l|locale::lookup(l).is_some()).cloned().unwrap_or_else(||icu_locale::locale!("en-US"))
}
