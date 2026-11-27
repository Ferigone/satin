//! Window UIEvent, MouseEvent and KeyboardEvent interfaces.
use super::{MouseInput, Shared, arg, event_target, getter, layout_api::LayoutHost, webidl};
use crate::layout::ScrollOffset;
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, rc::Rc};

// References must be traced by the VM even when only JS keeps an event alive.
const UI_FIELDS: u32 = 201;
const MOUSE_BRAND: u32 = 202;
const WINDOW_VIEWPORT: u32 = 203;
const KEYBOARD_BRAND: u32 = 204;
const FOCUS_BRAND: u32 = 205;
const WHEEL_BRAND: u32 = 214;
const UI: &[&str] = &["detail", "view"];
const MODIFIERS: &[(&str, &str)] = &[
    ("Alt", "altKey"),
    ("Control", "ctrlKey"),
    ("Meta", "metaKey"),
    ("Shift", "shiftKey"),
    ("AltGraph", "modifierAltGraph"),
    ("CapsLock", "modifierCapsLock"),
    ("Fn", "modifierFn"),
    ("FnLock", "modifierFnLock"),
    ("Hyper", "modifierHyper"),
    ("NumLock", "modifierNumLock"),
    ("ScrollLock", "modifierScrollLock"),
    ("Super", "modifierSuper"),
    ("Symbol", "modifierSymbol"),
    ("SymbolLock", "modifierSymbolLock"),
];
const MOUSE: &[&str] = &[
    "button",
    "buttons",
    "clientX",
    "clientY",
    "relatedTarget",
    "screenX",
    "screenY",
];
const KEYBOARD: &[&str] = &["key", "code", "location", "repeat", "isComposing"];
const WHEEL: &[&str] = &["deltaMode", "deltaX", "deltaY", "deltaZ"];
const LEGACY: &[&str] = &[
    "view",
    "detail",
    "screenX",
    "screenY",
    "clientX",
    "clientY",
    "ctrlKey",
    "altKey",
    "shiftKey",
    "metaKey",
    "button",
    "relatedTarget",
];

fn default_value(name: &str) -> Value {
    match name {
        "view" | "relatedTarget" => Value::Null,
        "key" | "code" => "".into(),
        "repeat" | "isComposing" => false.into(),
        name if name.ends_with("Key") || name.starts_with("modifier") => false.into(),
        _ => 0.into(),
    }
}

fn convert(vm: &mut Vm, dom: &Shared, name: &str, value: Value) -> Result<Value, JsError> {
    if value.is_undefined() {
        return Ok(default_value(name));
    }
    Ok(match name {
        "view" if value.is_nullish() || value == dom.borrow().window => value,
        "view" => return Err(JsError::type_error("Expected a Window or null")),
        "relatedTarget"
            if value.is_nullish()
                || super::target(dom, &value).is_ok()
                || event_target::target_table_exists(vm, &value) =>
        {
            value
        }
        "relatedTarget" => return Err(JsError::type_error("Expected an EventTarget or null")),
        "key" | "code" => Value::String(vm.to_string(&value)?),
        "deltaX" | "deltaY" | "deltaZ" => {
            let value = vm.to_number(&value)?;
            if !value.is_finite() {
                return Err(JsError::type_error("WheelEvent delta must be finite"));
            }
            Value::Number(value)
        }
        "repeat" | "isComposing" => value.is_truthy().into(),
        name if name.ends_with("Key") || name.starts_with("modifier") => value.is_truthy().into(),
        _ => {
            // Web IDL integer conversion truncates and wraps, including negative inputs.
            let unsigned = vm.to_number(&value)?.trunc().rem_euclid(4294967296.0) as u32;
            match name {
                "button" => i32::from(unsigned as i16).into(),
                "buttons" => u32::from(unsigned as u16).into(),
                "location" | "deltaMode" => unsigned.into(),
                _ => (unsigned as i32).into(),
            }
        }
    })
}

fn fields(vm: &Vm, receiver: &Value, mouse: bool) -> Result<Value, JsError> {
    if mouse && vm.host_slot(receiver, MOUSE_BRAND).is_none() {
        return Err(JsError::type_error("Illegal MouseEvent receiver"));
    }
    vm.host_slot(receiver, UI_FIELDS)
        .ok_or_else(|| JsError::type_error("Illegal UIEvent receiver"))
}

fn field(vm: &mut Vm, receiver: &Value, name: &str, mouse: bool) -> Result<Value, JsError> {
    let data = fields(vm, receiver, mouse)?;
    let value = vm.get_property(data, name)?;
    Ok(if value.is_undefined() {
        default_value(name)
    } else {
        value
    })
}

fn set(vm: &mut Vm, data: &Value, name: &str, value: Value) -> Result<(), JsError> {
    vm.define_data_property(data.clone(), name, value, true, false, true)
}

pub(super) fn initialize_empty(vm: &mut Vm, event: &Value, mouse: bool) -> Result<(), JsError> {
    let data = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_slot(event.clone(), UI_FIELDS, data.clone())?;
    if mouse {
        vm.set_host_data(data.clone(), Rc::new(Cell::new(ScrollOffset::default())))?;
        vm.set_host_slot(event.clone(), MOUSE_BRAND, data)?;
    }
    Ok(())
}

pub(super) fn set_window_viewport(
    vm: &mut Vm,
    window: &Value,
    layout: LayoutHost,
) -> Result<(), JsError> {
    let holder = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_data(holder.clone(), Rc::new(layout))?;
    vm.set_host_slot(window.clone(), WINDOW_VIEWPORT, holder)
}

fn scroll_offset(vm: &mut Vm, data: &Value) -> Result<ScrollOffset, JsError> {
    let view = vm.get_property(data.clone(), "view")?;
    let layout = vm
        .host_slot(&view, WINDOW_VIEWPORT)
        .and_then(|holder| vm.host_data::<LayoutHost>(&holder));
    match layout {
        Some(layout) => layout.event_scroll_offset(vm),
        None => Ok(ScrollOffset::default()),
    }
}

pub(super) fn capture_dispatch_position(vm: &mut Vm, event: &Value) -> Result<(), JsError> {
    if let Some(data) = vm.host_slot(event, MOUSE_BRAND)
        && let Some(offset) = vm.host_data::<Cell<ScrollOffset>>(&data)
    {
        offset.set(scroll_offset(vm, &data)?);
    }
    Ok(())
}

fn page_position(vm: &mut Vm, receiver: &Value, horizontal: bool) -> Result<Value, JsError> {
    let data = fields(vm, receiver, true)?;
    let dispatch = event_target::event(vm, receiver)?.active.get();
    let offset = if dispatch {
        vm.host_data::<Cell<ScrollOffset>>(&data)
            .expect("MouseEvent has a dispatch position")
            .get()
    } else {
        scroll_offset(vm, &data)?
    };
    let (client, scroll) = if horizontal {
        ("clientX", offset.x)
    } else {
        ("clientY", offset.y)
    };
    let client = vm.get_property(data, client)?.as_number().unwrap_or(0.);
    Ok(Value::Number(client + f64::from(scroll)))
}

pub(super) fn initialize_click(vm: &mut Vm, event: &Value) -> Result<(), JsError> {
    initialize_empty(vm, event, true)?;
    let data = fields(vm, event, true)?;
    set(vm, &data, "view", vm.global())
}

pub(super) fn initialize_mouse_input(
    vm: &mut Vm,
    event: &Value,
    input: MouseInput,
    buttons: i32,
    detail: i32,
) -> Result<(), JsError> {
    initialize_click(vm, event)?;
    let data = fields(vm, event, true)?;
    for (name, value) in [
        ("clientX", (input.client_x as i32).into()),
        ("clientY", (input.client_y as i32).into()),
        ("screenX", (input.screen_x as i32).into()),
        ("screenY", (input.screen_y as i32).into()),
        ("buttons", buttons.into()),
        ("detail", detail.into()),
        ("altKey", input.alt.into()),
        ("ctrlKey", input.control.into()),
        ("metaKey", input.meta.into()),
        ("shiftKey", input.shift.into()),
    ] {
        set(vm, &data, name, value)?;
    }
    Ok(())
}

pub(super) fn initialize_keyboard(vm: &mut Vm, event: &Value) -> Result<Value, JsError> {
    initialize_empty(vm, event, false)?;
    let data = fields(vm, event, false)?;
    vm.set_host_slot(event.clone(), KEYBOARD_BRAND, data.clone())?;
    Ok(data)
}

pub(super) fn initialize_key_input(
    vm: &mut Vm,
    event: &Value,
    properties: &[(&str, Value)],
) -> Result<(), JsError> {
    let data = initialize_keyboard(vm, event)?;
    set(vm, &data, "view", vm.global())?;
    for (name, value) in properties {
        set(vm, &data, name, value.clone())?;
    }
    Ok(())
}

fn initialize_focus(vm: &mut Vm, event: &Value) -> Result<Value, JsError> {
    initialize_empty(vm, event, false)?;
    let data = fields(vm, event, false)?;
    vm.set_host_slot(event.clone(), FOCUS_BRAND, data.clone())?;
    Ok(data)
}

pub(super) fn initialize_focus_input(vm: &mut Vm, event: &Value, related: Value) -> Result<(), JsError> {
    let data = initialize_focus(vm, event)?;
    set(vm, &data, "view", vm.global())?;
    set(vm, &data, "relatedTarget", related)
}

fn install_focus(vm: &mut Vm, dom: &Shared, ui: &Value, set_proto: Value) -> Result<Value, JsError> {
    let prototype = vm.new_object_with_prototype(ui.clone())?;
    let intrinsic = prototype.clone();
    let state = dom.clone();
    let constructor = webidl::interface(vm, prototype.clone(), "FocusEvent", 1, move |vm, _, args, target| {
        let target = target.ok_or_else(|| JsError::type_error("FocusEvent requires new"))?;
        webidl::required(args, 1, "FocusEvent")?;
        let kind = event_target::kind(vm, &args[0])?;
        let options = arg(args, 1);
        let flags = event_target::flags(vm, &options)?;
        let event = webidl::instance(vm, target, &intrinsic)?;
        let data = initialize_focus(vm, &event)?;
        for &member in UI.iter().chain(&["relatedTarget"]) {
            let value = event_target::option(vm, &options, member)?;
            let value = convert(vm, &state, member, value)?;
            set(vm, &data, member, value)?;
        }
        event_target::init_event(vm, event, kind, flags, false)
    })?;
    let ui_constructor = vm.get_global("UIEvent")?;
    vm.call(set_proto, Value::Undefined, &[constructor, ui_constructor])?;
    getter(vm, prototype.clone(), "relatedTarget", |vm, receiver, _| {
        if vm.host_slot(&receiver, FOCUS_BRAND).is_none() {
            return Err(JsError::type_error("Illegal FocusEvent receiver"));
        }
        field(vm, &receiver, "relatedTarget", false)
    })?;
    Ok(prototype)
}

fn key_data(vm: &Vm, event: &Value) -> Result<Value, JsError> {
    vm.host_slot(event, KEYBOARD_BRAND)
        .ok_or_else(|| JsError::type_error("Illegal KeyboardEvent receiver"))
}

fn modifier_value(vm: &mut Vm, data: Value, args: &[Value]) -> Result<Value, JsError> {
    webidl::required(args, 1, "getModifierState")?;
    let modifier = vm.to_string(&args[0])?;
    if modifier.len() <= 10 {
        let modifier = modifier.to_string_lossy();
        if let Some((_, name)) = MODIFIERS.iter().find(|(key, _)| *key == modifier) {
            return Ok(vm.get_property(data, name)?.is_truthy().into());
        }
    }
    Ok(false.into())
}

fn install_keyboard(
    vm: &mut Vm,
    dom: &Shared,
    ui_proto: &Value,
    set_proto: Value,
) -> Result<Value, JsError> {
    let proto = vm.new_object_with_prototype(ui_proto.clone())?;
    let intrinsic = proto.clone();
    let state = dom.clone();
    let constructor = webidl::interface(vm, proto.clone(), "KeyboardEvent", 1, move |vm, _, args, target| {
        let target = target.ok_or_else(|| JsError::type_error("KeyboardEvent requires new"))?;
        webidl::required(args, 1, "KeyboardEvent")?;
        let kind = event_target::kind(vm, &args[0])?;
        let options = arg(args, 1);
        let flags = event_target::flags(vm, &options)?;
        let event = webidl::instance(vm, target, &intrinsic)?;
        let data = initialize_keyboard(vm, &event)?;
        let members = UI.iter().copied()
            .chain(MODIFIERS.iter().map(|(_, member)| *member))
            .chain(KEYBOARD.iter().copied());
        for member in members {
            let value = event_target::option(vm, &options, member)?;
            if !value.is_undefined() {
                let value = convert(vm, &state, member, value)?;
                set(vm, &data, member, value)?;
            }
        }
        event_target::init_event(vm, event, kind, flags, false)
    })?;
    let ui = vm.get_global("UIEvent")?;
    vm.call(set_proto, Value::Undefined, &[constructor.clone(), ui])?;
    for (constant, value) in [
        ("DOM_KEY_LOCATION_STANDARD", 0),
        ("DOM_KEY_LOCATION_LEFT", 1),
        ("DOM_KEY_LOCATION_RIGHT", 2),
        ("DOM_KEY_LOCATION_NUMPAD", 3),
    ] {
        for target in [&constructor, &proto] {
            vm.define_data_property(target.clone(), constant, value.into(), false, true, false)?;
        }
    }
    for &name in KEYBOARD.iter().chain(&["altKey", "ctrlKey", "metaKey", "shiftKey", "charCode", "keyCode", "which"]) {
        getter(vm, proto.clone(), name, move |vm, this, _| {
            key_data(vm, &this)?;
            field(vm, &this, name, false)
        })?;
    }
    webidl::operation(vm, proto.clone(), "getModifierState", 1, |vm, this, args| {
        modifier_value(vm, key_data(vm, &this)?, args)
    })?;
    Ok(proto)
}

pub(super) fn install(
    vm: &mut Vm,
    dom: &Shared,
    event_proto: Value,
    event_ctor: Value,
    set_proto: Value,
) -> Result<(Value, Value, Value, Value), JsError> {
    let ui_proto = vm.new_object_with_prototype(event_proto)?;
    let mouse_proto = vm.new_object_with_prototype(ui_proto.clone())?;
    let mut parent = event_ctor;
    for (name, proto, mouse) in [
        ("UIEvent", &ui_proto, false),
        ("MouseEvent", &mouse_proto, true),
    ] {
        let intrinsic = proto.clone();
        let state = dom.clone();
        let constructor =
            webidl::interface(vm, proto.clone(), name, 1, move |vm, _, args, target| {
                let target =
                    target.ok_or_else(|| JsError::type_error(format!("{name} requires new")))?;
                webidl::required(args, 1, name)?;
                let kind = event_target::kind(vm, &args[0])?;
                let options = arg(args, 1);
                let flags = event_target::flags(vm, &options)?;
                let event = webidl::instance(vm, target, &intrinsic)?;
                initialize_empty(vm, &event, mouse)?;
                let data = fields(vm, &event, mouse)?;
                set_event_options(vm, &state, &data, &options, mouse, false)?;
                event_target::init_event(vm, event, kind, flags, false)
            })?;
        vm.call(
            set_proto.clone(),
            Value::Undefined,
            &[constructor.clone(), parent],
        )?;
        parent = constructor;
        let exposed: &[&str] = if mouse {
            &[
                "screenX",
                "screenY",
                "clientX",
                "clientY",
                "x",
                "y",
                "button",
                "buttons",
                "altKey",
                "ctrlKey",
                "shiftKey",
                "metaKey",
                "relatedTarget",
            ]
        } else {
            UI
        };
        for &property in exposed {
            let name = match property {
                "x" => "clientX",
                "y" => "clientY",
                _ => property,
            };
            getter(vm, proto.clone(), property, move |vm, this, _| {
                field(vm, &this, name, mouse)
            })?;
        }
        let state = dom.clone();
        let op = if mouse {
            "initMouseEvent"
        } else {
            "initUIEvent"
        };
        webidl::operation(vm, proto.clone(), op, 1, move |vm, this, args| {
            let data = fields(vm, &this, mouse)?;
            webidl::required(args, 1, op)?;
            let kind = event_target::kind(vm, &args[0])?;
            let fields = if mouse { LEGACY } else { &LEGACY[..2] };
            let mut values = Vec::with_capacity(fields.len());
            for (idx, name) in fields.iter().enumerate() {
                values.push(convert(vm, &state, name, arg(args, idx + 3))?);
            }
            let current = event_target::event(vm, &this)?;
            if !current.active.get() {
                vm.set_host_slot(this.clone(), event_target::EVENT_TARGET_SLOT, Value::Null)?;
                event_target::init_event(
                    vm,
                    this,
                    kind,
                    [
                        arg(args, 1).is_truthy(),
                        arg(args, 2).is_truthy(),
                        current.composed,
                    ],
                    false,
                )?;
                for (name, value) in fields.iter().zip(values) {
                    set(vm, &data, name, value)?;
                }
            }
            Ok(Value::Undefined)
        })?;
    }
    for (name, horizontal) in [("pageX", true), ("pageY", false)] {
        getter(vm, mouse_proto.clone(), name, move |vm, this, _| {
            page_position(vm, &this, horizontal)
        })?;
    }
    webidl::operation(
        vm,
        mouse_proto.clone(),
        "getModifierState",
        1,
        |vm, this, args| {
            modifier_value(vm, fields(vm, &this, true)?, args)
        },
    )?;
    install_wheel(vm, dom, &mouse_proto, parent, &set_proto)?;
    let keyboard_proto = install_keyboard(vm, dom, &ui_proto, set_proto.clone())?;
    let focus_proto = install_focus(vm, dom, &ui_proto, set_proto)?;
    Ok((ui_proto, mouse_proto, keyboard_proto, focus_proto))
}

fn set_event_options(
    vm: &mut Vm,
    state: &Shared,
    data: &Value,
    options: &Value,
    mouse: bool,
    wheel: bool,
) -> Result<(), JsError> {
    let members = UI.iter().copied().chain(
        MODIFIERS.iter().map(|(_, key)| *key).chain(MOUSE.iter().copied()).filter(|_| mouse),
    ).chain(WHEEL.iter().copied().filter(|_| wheel));
    for member in members {
        let value = event_target::option(vm, options, member)?;
        if !value.is_undefined() {
            let value = convert(vm, state, member, value)?;
            set(vm, data, member, value)?;
        }
    }
    Ok(())
}

fn install_wheel(
    vm: &mut Vm,
    dom: &Shared,
    mouse_proto: &Value,
    mouse_constructor: Value,
    set_proto: &Value,
) -> Result<(), JsError> {
    let prototype = vm.new_object_with_prototype(mouse_proto.clone())?;
    let intrinsic = prototype.clone();
    let state = dom.clone();
    let constructor = webidl::interface(vm, prototype.clone(), "WheelEvent", 1,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("WheelEvent requires new"))?;
            webidl::required(args, 1, "WheelEvent")?;
            let kind = event_target::kind(vm, &args[0])?;
            let options = arg(args, 1);
            let flags = event_target::flags(vm, &options)?;
            let event = webidl::instance(vm, target, &intrinsic)?;
            initialize_empty(vm, &event, true)?;
            let data = fields(vm, &event, true)?;
            vm.set_host_slot(event.clone(), WHEEL_BRAND, data.clone())?;
            set_event_options(vm, &state, &data, &options, true, true)?;
            event_target::init_event(vm, event, kind, flags, false)
        },
    )?;
    vm.call(set_proto.clone(), Value::Undefined, &[constructor.clone(), mouse_constructor])?;
    for (index, name) in ["DOM_DELTA_PIXEL", "DOM_DELTA_LINE", "DOM_DELTA_PAGE"].into_iter().enumerate() {
        for object in [&constructor, &prototype] {
            vm.define_data_property(object.clone(), name, (index as u32).into(), false, true, false)?;
        }
    }
    for &name in WHEEL {
        getter(vm, prototype.clone(), name, move |vm, event, _| {
            if vm.host_slot(&event, WHEEL_BRAND).is_none() {
                return Err(JsError::type_error("Illegal WheelEvent receiver"));
            }
            field(vm, &event, name, true)
        })?;
    }
    Ok(())
}
