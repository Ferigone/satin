//! Window UIEvent and MouseEvent interfaces, including the legacy event factories.
use super::{Shared, arg, event_target, getter, webidl};
use rust_browser_javascript::{JsError, Value, Vm};

// References must be traced by the VM even when only JS keeps an event alive.
const UI_FIELDS: u32 = 201;
const MOUSE_BRAND: u32 = 202;
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
        name if name.ends_with("Key") || name.starts_with("modifier") => value.is_truthy().into(),
        _ => {
            // Web IDL integer conversion truncates and wraps, including negative inputs.
            let unsigned = vm.to_number(&value)?.trunc().rem_euclid(4294967296.0) as u32;
            match name {
                "button" => i32::from(unsigned as i16).into(),
                "buttons" => u32::from(unsigned as u16).into(),
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
    vm.set_host_slot(event.clone(), UI_FIELDS, data)?;
    if mouse {
        vm.set_host_slot(event.clone(), MOUSE_BRAND, true.into())?;
    }
    Ok(())
}

pub(super) fn initialize_click(vm: &mut Vm, event: &Value) -> Result<(), JsError> {
    initialize_empty(vm, event, true)?;
    let data = fields(vm, event, true)?;
    set(vm, &data, "view", vm.global())
}

pub(super) fn install(
    vm: &mut Vm,
    dom: &Shared,
    event_proto: Value,
    event_ctor: Value,
    set_proto: Value,
) -> Result<(Value, Value), JsError> {
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
                let members = UI.iter().copied().chain(
                    MODIFIERS
                        .iter()
                        .map(|(_, key)| *key)
                        .chain(MOUSE.iter().copied())
                        .filter(|_| mouse),
                );
                for member in members {
                    let value = event_target::option(vm, &options, member)?;
                    if !value.is_undefined() {
                        let value = convert(vm, &state, member, value)?;
                        set(vm, &data, member, value)?;
                    }
                }
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
    webidl::operation(
        vm,
        mouse_proto.clone(),
        "getModifierState",
        1,
        |vm, this, args| {
            fields(vm, &this, true)?;
            webidl::required(args, 1, "getModifierState")?;
            let modifier = vm.to_string(&args[0])?;
            if modifier.len() <= 10
                && let Some((_, name)) = MODIFIERS
                    .iter()
                    .find(|(key, _)| *key == modifier.to_string_lossy())
            {
                return field(vm, &this, name, true);
            }
            Ok(false.into())
        },
    )?;
    Ok((ui_proto, mouse_proto))
}
