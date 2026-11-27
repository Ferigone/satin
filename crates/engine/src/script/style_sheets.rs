//! CSS stylesheets own a live CSSRuleList. Traced slots retain the
//! wrappers as long as a sheet, list or member rule remains reachable.
use super::*;
use rust_browser_javascript::HostIndexedProperties;

const RULE_LIST: u32 = 0x4353534c;
const RULE_MEMBERS: u32 = 0x4353534d;
const RULE_TEXT: u32 = 0x43535354;
const DISABLED: u32 = 0x43535344;
const RULE_PARENT: u32 = 0x43535350;
const OWNER_NODE: u32 = 0x4353534f;
const ELEMENT_SHEET: u32 = 0x43535345;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;

struct InlineSheet {
    state: std::rc::Weak<RefCell<State>>,
    node: NodeId,
    original: Arc<str>,
}

pub(super) fn is_style_element(document: &Document, node: NodeId) -> bool {
    document.namespace(node) == crate::dom::Namespace::Html && document.tag(node) == "style"
}

impl InlineSheet {
    fn active(&self, document: &Document) -> bool {
        document.is_connected(self.node)
            && document.attr(self.node, "type").is_none_or(|value| {
                value.trim().is_empty() || value.trim().eq_ignore_ascii_case("text/css")
            })
            && self.original.as_ref() == document.text(self.node)
    }
}

#[derive(Default)]
struct RuleList(RefCell<Vec<Value>>);

impl HostIndexedProperties for RuleList {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.0.borrow().len())
    }
    fn indexed_get(&self, _: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        Ok(self.0.borrow().get(index as usize).cloned())
    }
}

fn list_data(vm: &Vm, object: &Value) -> Result<Rc<RuleList>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("CSSRuleList receiver required"))
}

fn list(vm: &Vm, sheet: &Value) -> Result<Value, JsError> {
    vm.host_slot(sheet, RULE_LIST)
        .ok_or_else(|| JsError::type_error("CSSStyleSheet receiver required"))
}

fn index(vm: &mut Vm, value: &Value) -> Result<usize, JsError> {
    Ok(vm.to_number(value)?.trunc().rem_euclid(4294967296.0) as u32 as usize)
}

fn update(vm: &mut Vm, wrapper: Value, rules: Vec<Value>) -> Result<(), JsError> {
    let data = list_data(vm, &wrapper)?;
    let keep = vm.new_array(&rules);
    vm.set_host_slot(wrapper, RULE_MEMBERS, keep)?;
    *data.0.borrow_mut() = rules;
    Ok(())
}

fn initialize(
    vm: &mut Vm,
    sheet: &Value,
    prototype: &Value,
    disabled: bool,
) -> Result<Value, JsError> {
    vm.set_host_slot(sheet.clone(), DISABLED, disabled.into())?;
    let list = vm.new_object_with_prototype(prototype.clone())?;
    let data = Rc::new(RuleList::default());
    vm.mark_platform_object(&list)?;
    vm.set_host_data(list.clone(), data.clone())?;
    vm.set_host_indexed_properties(list.clone(), data)?;
    vm.set_host_slot(sheet.clone(), RULE_LIST, list.clone())?;
    Ok(list)
}

fn inline_update(
    vm: &mut Vm,
    sheet: &Value,
    rules: &[Value],
    disabled: bool,
) -> Result<Option<(Rc<InlineSheet>, crate::dom::InlineStyleOverride)>, JsError> {
    let Some(attached) = vm.host_data::<InlineSheet>(sheet) else {
        return Ok(None);
    };
    let Some(state) = attached.state.upgrade() else {
        return Ok(None);
    };
    if !attached.active(&state.borrow().document)
        || vm
            .host_slot(sheet, OWNER_NODE)
            .and_then(|element| vm.host_slot(&element, ELEMENT_SHEET))
            .as_ref()
            != Some(sheet)
    {
        return Ok(None);
    }
    let source = if disabled {
        None
    } else {
        let mut source = String::new();
        for member in rules {
            let text = vm
                .host_slot(member, RULE_TEXT)
                .expect("native CSS rule has source");
            let text = text
                .as_string()
                .expect("native CSS rule source is a string");
            vm.charge_buffer_work(text.len())?;
            source.push_str(&text.to_string_lossy());
            source.push('\n');
            if source.len() > MAX_SOURCE_BYTES {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "CSS stylesheet exceeds its source budget",
                ));
            }
        }
        Some(Arc::from(source))
    };
    let original = attached.original.clone();
    Ok(Some((
        attached,
        crate::dom::InlineStyleOverride { original, source },
    )))
}

fn apply_inline(update: Option<(Rc<InlineSheet>, crate::dom::InlineStyleOverride)>) {
    if let Some((attached, sheet)) = update
        && let Some(state) = attached.state.upgrade()
    {
        let mut state = state.borrow_mut();
        state
            .document
            .inline_style_overrides
            .insert(attached.node, sheet);
        mark_style_dirty(&mut state);
    }
}

fn rule(vm: &mut Vm, prototype: &Value, sheet: &Value, source: String) -> Result<Value, JsError> {
    let rule = vm.new_object_with_prototype(prototype.clone())?;
    vm.mark_platform_object(&rule)?;
    vm.set_host_slot(rule.clone(), RULE_TEXT, source.into())?;
    vm.set_host_slot(rule.clone(), RULE_PARENT, sheet.clone())?;
    Ok(rule)
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    element_proto: Value,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    let style = vm.new_object();
    let sheet_proto = vm.new_object_with_prototype(style.clone())?;
    let rule_proto = vm.new_object();
    let list_proto = vm.new_object();
    for (name, proto) in [
        ("StyleSheet", &style),
        ("CSSRule", &rule_proto),
        ("CSSRuleList", &list_proto),
    ] {
        webidl::interface(vm, proto.clone(), name, 0, |_, _, _, _| {
            Err(JsError::type_error("Illegal constructor"))
        })?;
    }
    let array = vm.new_array(&[]);
    let iterator = vm.get_property(array, "values")?;
    vm.define_data_property_key(
        list_proto.clone(),
        webidl::well_known_key(vm, "iterator"),
        iterator,
        true,
        false,
        true,
    )?;
    getter(vm, list_proto.clone(), "length", |vm, this, _| {
        Ok((list_data(vm, &this)?.indexed_len()? as u32).into())
    })?;
    webidl::operation(vm, list_proto.clone(), "item", 1, |vm, this, args| {
        let list = list_data(vm, &this)?;
        webidl::required(args, 1, "item")?;
        let i = index(vm, &args[0])? as u32;
        Ok(list.indexed_get(vm, i)?.unwrap_or(Value::Null))
    })?;
    for (name, slot) in [("cssText", RULE_TEXT), ("parentStyleSheet", RULE_PARENT)] {
        getter(vm, rule_proto.clone(), name, move |vm, this, _| {
            vm.host_slot(&this, slot)
                .ok_or_else(|| JsError::type_error("CSSRule receiver required"))
        })?;
    }
    for name in ["cssRules", "rules"] {
        getter(vm, sheet_proto.clone(), name, |vm, this, _| list(vm, &this))?;
    }
    for name in ["href", "parentStyleSheet", "title"] {
        getter(vm, style.clone(), name, |vm, this, _| {
            list(vm, &this)?;
            Ok(Value::Null)
        })?;
    }
    getter(vm, style.clone(), "ownerNode", |vm, this, _| {
        list(vm, &this)?;
        let Some(attached) = vm.host_data::<InlineSheet>(&this) else {
            return Ok(Value::Null);
        };
        let active = attached
            .state
            .upgrade()
            .is_some_and(|state| attached.active(&state.borrow().document));
        Ok(if active {
            vm.host_slot(&this, OWNER_NODE).unwrap_or(Value::Null)
        } else {
            Value::Null
        })
    })?;
    let read = vm.new_native_function("get disabled", |vm, this, _| {
        vm.host_slot(&this, DISABLED)
            .ok_or_else(|| JsError::type_error("StyleSheet receiver required"))
    });
    let write = vm.new_native_function("set disabled", |vm, this, args| {
        let wrapper = list(vm, &this)?;
        let disabled = arg(args, 0).is_truthy();
        let data = list_data(vm, &wrapper)?;
        let next = inline_update(vm, &this, &data.0.borrow(), disabled)?;
        vm.set_host_slot(this, DISABLED, disabled.into())?;
        apply_inline(next);
        Ok(Value::Undefined)
    });
    vm.define_accessor(style.clone(), "disabled", Some(read), Some(write))?;
    getter(vm, sheet_proto.clone(), "ownerRule", |vm, this, _| {
        list(vm, &this)?;
        Ok(Value::Null)
    })?;
    getter(vm, style, "type", |vm, this, _| {
        list(vm, &this)?;
        Ok("text/css".into())
    })?;
    let intrinsic = sheet_proto.clone();
    let constructed_list = list_proto.clone();
    webidl::interface(
        vm,
        sheet_proto.clone(),
        "CSSStyleSheet",
        0,
        move |vm, _, args, target| {
            let target = target.ok_or_else(|| JsError::type_error("CSSStyleSheet requires new"))?;
            let options = arg(args, 0);
            let disabled = if options.is_nullish() {
                false
            } else if options.as_object().is_some() {
                vm.get_property(options, "disabled")?.is_truthy()
            } else {
                return Err(JsError::type_error("Expected CSSStyleSheetInit dictionary"));
            };
            let sheet = webidl::instance(vm, target, &intrinsic)?;
            initialize(vm, &sheet, &constructed_list, disabled)?;
            Ok(sheet)
        },
    )?;
    webidl::interface(
        vm,
        element_proto.clone(),
        "HTMLStyleElement",
        0,
        |_, _, _, _| Err(JsError::type_error("Illegal constructor")),
    )?;
    let sheet_type = sheet_proto.clone();
    let rule_type = rule_proto.clone();
    let host = Rc::downgrade(state);
    getter(vm, element_proto, "sheet", move |vm, element, _| {
        let state = host
            .upgrade()
            .ok_or_else(|| JsError::type_error("Style element has no document"))?;
        let id = node(&state, &element)?;
        let original: Arc<str> = {
            let state = state.borrow();
            let document = &state.document;
            if !is_style_element(document, id) {
                return Err(JsError::type_error("HTMLStyleElement receiver required"));
            }
            if !document.is_connected(id)
                || document.attr(id, "type").is_some_and(|value| {
                    !value.trim().is_empty() && !value.trim().eq_ignore_ascii_case("text/css")
                })
            {
                return Ok(Value::Null);
            }
            Arc::from(document.text(id))
        };
        vm.charge_buffer_work(original.len())?;
        if let Some(cached) = vm.host_slot(&element, ELEMENT_SHEET)
            && vm
                .host_data::<InlineSheet>(&cached)
                .is_some_and(|attached| attached.original == original)
        {
            return Ok(cached);
        }
        let sheet = vm.new_object_with_prototype(sheet_type.clone())?;
        vm.mark_platform_object(&sheet)?;
        let list = initialize(vm, &sheet, &list_proto, false)?;
        let (sources, _) = css::constructed_rules(&original);
        let members = sources
            .into_iter()
            .map(|source| rule(vm, &rule_type, &sheet, source))
            .collect::<Result<Vec<_>, _>>()?;
        update(vm, list, members)?;
        vm.set_host_data(
            sheet.clone(),
            Rc::new(InlineSheet {
                state: host.clone(),
                node: id,
                original,
            }),
        )?;
        vm.set_host_slot(sheet.clone(), OWNER_NODE, element.clone())?;
        vm.set_host_slot(element, ELEMENT_SHEET, sheet.clone())?;
        Ok(sheet)
    })?;
    for name in ["replaceSync", "insertRule", "deleteRule"] {
        let proto = rule_proto.clone();
        let errors = errors.clone();
        webidl::operation(vm, sheet_proto.clone(), name, 1, move |vm, this, args| {
            let wrapper = list(vm, &this)?;
            webidl::required(args, 1, name)?;
            let disabled = vm
                .host_slot(&this, DISABLED)
                .is_some_and(|state| state.is_truthy());
            if name == "deleteRule" {
                let i = index(vm, &args[0])?;
                let data = list_data(vm, &wrapper)?;
                let mut existing = data.0.borrow().clone();
                if i >= existing.len() {
                    return Err(errors.exception(vm, "IndexSizeError", "Rule index out of bounds"));
                }
                let removed = existing.remove(i);
                let next = inline_update(vm, &this, &existing, disabled)?;
                vm.set_host_slot(removed, RULE_PARENT, Value::Null)?;
                update(vm, wrapper, existing)?;
                apply_inline(next);
                return Ok(Value::Undefined);
            }
            let source = text(vm, args[0].clone())?;
            if source.len() > MAX_SOURCE_BYTES {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "CSS stylesheet exceeds its source budget",
                ));
            }
            vm.charge_work(source.len())?;
            let (parsed, single) = css::constructed_rules(&source);
            if name == "insertRule" {
                let i = if let Some(offset) = args.get(1) {
                    index(vm, offset)?
                } else {
                    0
                };
                if !single || parsed.len() != 1 {
                    return Err(errors.exception(vm, "SyntaxError", "Expected one CSS rule"));
                }
                let data = list_data(vm, &wrapper)?;
                let mut existing = data.0.borrow().clone();
                if i > existing.len() {
                    return Err(errors.exception(vm, "IndexSizeError", "Rule index out of bounds"));
                }
                if existing.len() >= 8192 {
                    return Err(JsError::new(
                        ErrorKind::HeapLimit,
                        "CSS rule budget exceeded",
                    ));
                }
                let member = rule(vm, &proto, &this, parsed.into_iter().next().unwrap())?;
                existing.insert(i, member);
                let next = inline_update(vm, &this, &existing, disabled)?;
                update(vm, wrapper, existing)?;
                apply_inline(next);
                return Ok((i as u32).into());
            }
            if vm.host_data::<InlineSheet>(&this).is_some() {
                return Err(errors.exception(
                    vm,
                    "NotAllowedError",
                    "Only constructed sheets can be replaced",
                ));
            }
            let mut replacement = Vec::with_capacity(parsed.len());
            for src in parsed {
                replacement.push(rule(vm, &proto, &this, src)?);
            }
            let data = list_data(vm, &wrapper)?;
            for old in data.0.borrow().iter() {
                vm.set_host_slot(old.clone(), RULE_PARENT, Value::Null)?;
            }
            update(vm, wrapper, replacement)?;
            Ok(Value::Undefined)
        })?;
    }
    Ok(())
}
