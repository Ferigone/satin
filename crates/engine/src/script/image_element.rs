//! JavaScript access to the DOM image element and its shared document resources.
use super::{
    Shared, State, arg, document_base, element, getter, new_detached, set_attribute, text, webidl,
    wrap,
};
use crate::dom::{Document, Namespace, NodeId, NodeKind};
use rust_browser_javascript::{JsError, Value, Vm};

pub(super) fn is_image(document: &Document, id: NodeId) -> bool {
    document.namespace(id) == Namespace::Html && document.tag(id) == "img"
}

pub(super) fn attribute_changed(state: &mut State, id: NodeId, name: &str) {
    if state.navigation_attached
        && matches!(name, "src" | "crossorigin")
        && is_image(&state.document, id)
    {
        state.invalid_images.remove(&id);
        state.image_updates.insert(id);
    }
}

pub(super) fn install(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    webidl::interface(
        vm,
        prototype.clone(),
        "HTMLImageElement",
        0,
        |_, _, _, _| Err(JsError::type_error("Illegal HTMLImageElement constructor")),
    )?;
    let cx = state.clone();
    let image =
        vm.new_native_constructor_with_new_target("Image", move |vm, _, args, new_target| {
            if new_target.is_none() {
                return Err(JsError::type_error("Image must be constructed with new"));
            }
            let mut attrs = crate::dom::Attributes::new();
            for (index, name) in ["width", "height"].into_iter().enumerate() {
                if args.get(index).is_some_and(|value| !value.is_undefined()) {
                    attrs.insert(name.into(), dimension(vm, &args[index])?.to_string());
                }
            }
            vm.charge_work(1)?;
            let id = new_detached(
                &cx,
                NodeKind::Element {
                    tag: "img".into(),
                    attrs,
                },
            )?;
            wrap(vm, &cx, Some(id))
        });
    vm.define_data_property(
        image.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )?;
    vm.define_data_property(image.clone(), "length", 0.into(), false, false, true)?;
    vm.set_global("Image", image)?;

    for (property, attr) in [
        ("src", "src"),
        ("alt", "alt"),
        ("crossOrigin", "crossorigin"),
    ] {
        let cx = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = receiver(&cx, &this)?;
            let state = cx.borrow();
            let value = state.document.attr(id, attr);
            let Some(value) = value else {
                return Ok(if attr == "crossorigin" {
                    Value::Null
                } else {
                    "".into()
                });
            };
            vm.charge_work(value.len())?;
            if attr == "src" {
                let url = document_base(&state)
                    .join(value)
                    .map_or_else(|_| value.to_owned(), |url| url.to_string());
                Ok(url.into())
            } else if attr == "crossorigin" {
                Ok(if value.eq_ignore_ascii_case("use-credentials") {
                    "use-credentials"
                } else {
                    "anonymous"
                }
                .into())
            } else {
                Ok(value.into())
            }
        });
        let cx = state.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let id = receiver(&cx, &this)?;
            let value = arg(args, 0);
            let attr_value =
                if attr == "crossorigin" && matches!(value, Value::Null | Value::Undefined) {
                    None
                } else {
                    Some(text(vm, value)?)
                };
            set_attribute(vm, &cx, id, attr, attr_value)?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), property, Some(get), Some(set))?;
    }
    for (axis, name) in ["width", "height"].into_iter().enumerate() {
        let cx = state.clone();
        let get = vm.new_native_function(name, move |_, this, _| {
            let id = receiver(&cx, &this)?;
            let state = cx.borrow();
            Ok(state
                .document
                .attr(id, name)
                .and_then(nonnegative_integer)
                .unwrap_or_else(|| super::canvas::image_metadata(&state, id).size[axis])
                .into())
        });
        let cx = state.clone();
        let set = vm.new_native_function(name, move |vm, this, args| {
            let id = receiver(&cx, &this)?;
            let value = dimension(vm, &arg(args, 0))?;
            set_attribute(vm, &cx, id, name, Some(value.to_string()))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), name, Some(get), Some(set))?;
    }
    for (axis, name) in ["naturalWidth", "naturalHeight"].into_iter().enumerate() {
        let cx = state.clone();
        getter(vm, prototype.clone(), name, move |_, this, _| {
            let id = receiver(&cx, &this)?;
            Ok(super::canvas::image_metadata(&cx.borrow(), id).size[axis].into())
        })?;
    }
    let cx = state.clone();
    getter(vm, prototype.clone(), "complete", move |_, this, _| {
        let id = receiver(&cx, &this)?;
        let state = cx.borrow();
        let no_source = state
            .document
            .attr(id, "src")
            .is_none_or(|src| src.trim().is_empty());
        Ok(Value::Bool(
            no_source || super::canvas::image_metadata(&state, id).settled,
        ))
    })?;
    let cx = state.clone();
    getter(vm, prototype, "currentSrc", move |vm, this, _| {
        let id = receiver(&cx, &this)?;
        let state = cx.borrow();
        let metadata = super::canvas::image_metadata(&state, id);
        let src = if metadata.settled {
            metadata.url.unwrap_or_default()
        } else {
            String::new()
        };
        vm.charge_work(src.len())?;
        Ok(src.into())
    })
}

fn receiver(state: &Shared, value: &Value) -> Result<NodeId, JsError> {
    let id = element(state, value)?;
    if !is_image(&state.borrow().document, id) {
        return Err(JsError::type_error("Illegal HTMLImageElement receiver"));
    }
    Ok(id)
}

fn dimension(vm: &mut Vm, value: &Value) -> Result<u32, JsError> {
    let number = vm.to_number(value)?;
    Ok(if number.is_finite() {
        number.trunc().rem_euclid(4294967296.) as u32
    } else {
        0
    })
}

fn nonnegative_integer(value: &str) -> Option<u32> {
    let value = value.trim_start();
    let value = value.strip_prefix('+').unwrap_or(value);
    let end = value.bytes().take_while(u8::is_ascii_digit).count();
    value[..end].parse().ok()
}
