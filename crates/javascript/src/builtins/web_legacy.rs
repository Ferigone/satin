//! Web-compatible HTML string methods and legacy object accessors (Annex B).
use super::{append_units, arg, descriptor, method};
use crate::{JsError, Value, Vm, heap::PropertyKind};

pub(super) fn install(vm: &mut Vm) {
    let strings = Value::Object(vm.realm.string_prototype);
    for (name, tag, attribute) in [
        ("anchor", "a", Some("name")),
        ("big", "big", None),
        ("blink", "blink", None),
        ("bold", "b", None),
        ("fixed", "tt", None),
        ("fontcolor", "font", Some("color")),
        ("fontsize", "font", Some("size")),
        ("italics", "i", None),
        ("link", "a", Some("href")),
        ("small", "small", None),
        ("strike", "strike", None),
        ("sub", "sub", None),
        ("sup", "sup", None),
    ] {
        method(
            vm,
            strings.clone(),
            name,
            usize::from(attribute.is_some()),
            move |vm, this, args| {
                if this.is_nullish() {
                    return Err(JsError::type_error("String method requires a receiver"));
                }
                let text = vm.to_string(&this)?;
                let mut result: Vec<u16> = format!("<{tag}").encode_utf16().collect();
                if let Some(attribute) = attribute {
                    let attr = vm.to_string(&arg(args, 0))?;
                    result.extend(format!(" {attribute}=\"").encode_utf16());
                    let quote = u16::from(b'"');
                    for (i, part) in attr.as_utf16().split(|unit| *unit == quote).enumerate() {
                        if i > 0 {
                            append_units(vm, &mut result, &[38, 113, 117, 111, 116, 59])?;
                        }
                        append_units(vm, &mut result, part)?;
                    }
                    result.push(quote);
                }
                result.push(u16::from(b'>'));
                append_units(vm, &mut result, text.as_utf16())?;
                append_units(
                    vm,
                    &mut result,
                    &format!("</{tag}>").encode_utf16().collect::<Vec<_>>(),
                )?;
                vm.finish_string(result)
            },
        );
    }
    for (alias, name) in [("trimLeft", "trimStart"), ("trimRight", "trimEnd")] {
        let function = vm
            .get_property(strings.clone(), name)
            .expect("trim method installed");
        vm.define_data_property(strings.clone(), alias, function, true, false, true)
            .unwrap();
    }

    let objects = Value::Object(vm.realm.object_prototype);
    for (name, is_getter) in [("__defineGetter__", true), ("__defineSetter__", false)] {
        method(vm, objects.clone(), name, 2, move |vm, this, args| {
            let object = vm.coerce_object(this)?;
            let accessor = arg(args, 1);
            if !vm.is_callable(&accessor) {
                return Err(JsError::type_error("Expected an accessor function"));
            }
            let key = vm.to_property_key(&arg(args, 0))?;
            let desc = vm.new_object_with_prototype(Value::Null)?;
            for (field, value) in [
                (if is_getter { "get" } else { "set" }, accessor),
                ("enumerable", true.into()),
                ("configurable", true.into()),
            ] {
                vm.define_data_property(desc.clone(), field, value, true, true, true)?;
            }
            let definition = descriptor(vm, object.as_object().unwrap(), &key, desc)?;
            definition.apply(vm, object, key, true)?;
            Ok(Value::Undefined)
        });
    }
    for (name, is_getter) in [("__lookupGetter__", true), ("__lookupSetter__", false)] {
        method(vm, objects.clone(), name, 1, move |vm, this, args| {
            let object = vm.coerce_object(this)?;
            let key = vm.to_property_key(&arg(args, 0))?;
            let mut next = object.as_object();
            while let Some(id) = next {
                vm.tick()?;
                if let Some(property) = vm.own_descriptor_key(id, &key)? {
                    return Ok(match property.kind {
                        PropertyKind::Accessor { get, set } => {
                            if is_getter { get } else { set }.unwrap_or_default()
                        }
                        PropertyKind::Data { .. } => Value::Undefined,
                    });
                }
                next = vm.object_prototype_of(id)?;
            }
            Ok(Value::Undefined)
        });
    }
}
