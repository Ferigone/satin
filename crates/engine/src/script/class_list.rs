//! Element.classList reflects the current attribute, including edits from other DOM APIs.
use super::*;
use rust_browser_javascript::HostIndexedProperties;
use std::collections::{HashSet, VecDeque};

const CLASS_LIST_SLOT: u32 = 0x434c_5354;

#[derive(Default)]
struct Cache {
    revision: u64,
    entries: VecDeque<(NodeId, Rc<[String]>, usize)>,
    retained: usize,
    reflection_work: usize,
}

struct TokenList {
    state: Shared,
    element: NodeId,
    cache: Rc<RefCell<Cache>>,
}

impl TokenList {
    fn tokens(&self, vm: Option<&mut Vm>) -> Result<Rc<[String]>, JsError> {
        let state = self.state.borrow();
        let mut cache = self.cache.borrow_mut();
        if cache.revision != state.revision {
            cache.entries.clear();
            cache.retained = 0;
            cache.revision = state.revision;
        }
        if let Some((_, tokens, _)) = cache
            .entries
            .iter()
            .find(|(node, _, _)| *node == self.element)
        {
            return Ok(tokens.clone());
        }
        let value = state.document.attr(self.element, "class").unwrap_or("");
        if let Some(vm) = vm {
            vm.charge_work(value.len())?;
        } else {
            // Indexed metadata can be queried without a VM. Bound uncached native scans.
            cache.reflection_work = cache.reflection_work.saturating_add(value.len());
            if cache.reflection_work > 128 * 1024 * 1024 {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "DOMTokenList reflection budget exceeded",
                ));
            }
        }
        let mut seen = HashSet::new();
        let tokens: Rc<[String]> = value
            .split_ascii_whitespace()
            .filter(|token| seen.insert(*token))
            .map(str::to_owned)
            .collect();
        let bytes = value.len() + tokens.len() * std::mem::size_of::<String>();
        // A single large class attribute can still be indexed without retaining a copy for every view.
        while cache.entries.len() >= 12 || cache.retained + bytes > 256 * 1024 {
            let Some((_, _, evicted)) = cache.entries.pop_front() else {
                break;
            };
            cache.retained -= evicted;
        }
        cache.retained += bytes;
        cache
            .entries
            .push_back((self.element, tokens.clone(), bytes));
        Ok(tokens)
    }
    fn scan_tokens(&self, vm: &mut Vm) -> Result<Rc<[String]>, JsError> {
        let bytes = self
            .state
            .borrow()
            .document
            .attr(self.element, "class")
            .map_or(0, str::len);
        vm.charge_work(bytes)?;
        self.tokens(Some(vm))
    }
    fn raw(&self, vm: &mut Vm) -> Result<Value, JsError> {
        let state = self.state.borrow();
        let value = state.document.attr(self.element, "class").unwrap_or("");
        vm.charge_work(value.len())?;
        Ok(value.into())
    }
    fn update(&self, vm: &mut Vm, tokens: &[&str]) -> Result<(), JsError> {
        if !tokens.is_empty()
            || self
                .state
                .borrow()
                .document
                .attr(self.element, "class")
                .is_some()
        {
            let bytes =
                tokens.iter().map(|s| s.len()).sum::<usize>() + tokens.len().saturating_sub(1);
            if bytes > DOM_BYTES_LIMIT {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "DOM attribute exceeds its byte budget",
                ));
            }
            vm.charge_work(bytes)?;
            set_attribute(
                vm,
                &self.state,
                self.element,
                "class",
                Some(tokens.join(" ")),
            )?;
        }
        Ok(())
    }
}

impl HostIndexedProperties for TokenList {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.tokens(None)?.len())
    }
    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        let tokens = self.tokens(Some(vm))?;
        if let Some(token) = tokens.get(index as usize) {
            vm.charge_work(token.len())?;
            Ok(Some(token.as_str().into()))
        } else {
            Ok(None)
        }
    }
}

fn receiver(vm: &Vm, this: &Value) -> Result<Rc<TokenList>, JsError> {
    vm.host_data::<TokenList>(this)
        .ok_or_else(|| JsError::type_error("Expected a DOMTokenList"))
}

fn validate(vm: &mut Vm, errors: &dom_api::Errors, token: &str) -> Result<(), JsError> {
    if token.is_empty() {
        Err(errors.exception(vm, "SyntaxError", "A token must not be empty"))
    } else if token.bytes().any(|b| b.is_ascii_whitespace()) {
        Err(errors.exception(
            vm,
            "InvalidCharacterError",
            "A token must not contain ASCII whitespace",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    element_proto: &Value,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    let proto = vm.new_object();
    webidl::interface(vm, proto.clone(), "DOMTokenList", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal DOMTokenList constructor"))
    })?;
    vm.root(proto.clone());
    let owner_state = state.clone();
    let list_proto = proto.clone();
    let cache = Rc::new(RefCell::new(Cache::default()));
    let get_list = vm.new_native_function("get classList", move |vm, owner, _| {
        let element = element(&owner_state, &owner)?;
        if let Some(view) = vm.host_slot(&owner, CLASS_LIST_SLOT) {
            return Ok(view);
        }
        let view = vm.new_object_with_prototype(list_proto.clone())?;
        vm.mark_platform_object(&view)?;
        let list = Rc::new(TokenList {
            state: owner_state.clone(),
            element,
            cache: cache.clone(),
        });
        vm.set_host_data(view.clone(), list.clone())?;
        vm.set_host_indexed_properties(view.clone(), list)?;
        vm.set_host_slot(owner, CLASS_LIST_SLOT, view.clone())?;
        Ok(view)
    });
    vm.root(get_list.clone());
    let get = get_list.clone();
    let set_list = vm.new_native_function("set classList", move |vm, owner, args| {
        let view = vm.call(get.clone(), owner, &[])?;
        vm.set_property(view, "value", arg(args, 0))?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(
        element_proto.clone(),
        "classList",
        Some(get_list),
        Some(set_list),
    )?;
    getter(vm, proto.clone(), "length", |vm, this, _| {
        Ok((receiver(vm, &this)?.tokens(Some(vm))?.len() as f64).into())
    })?;
    let get = vm.new_native_function("get value", |vm, this, _| receiver(vm, &this)?.raw(vm));
    let set = vm.new_native_function("set value", |vm, this, args| {
        let list = receiver(vm, &this)?;
        let value = text(vm, arg(args, 0))?;
        set_attribute(vm, &list.state, list.element, "class", Some(value))?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(proto.clone(), "value", Some(get), Some(set))?;
    webidl::operation(vm, proto.clone(), "toString", 0, |vm, this, _| {
        receiver(vm, &this)?.raw(vm)
    })?;
    webidl::operation(vm, proto.clone(), "item", 1, |vm, this, args| {
        let list = receiver(vm, &this)?;
        webidl::required(args, 1, "item")?;
        let index = vm.to_number(&args[0])?.trunc().rem_euclid(4294967296.0) as u32;
        Ok(list.indexed_get(vm, index)?.unwrap_or(Value::Null))
    })?;
    webidl::operation(vm, proto.clone(), "contains", 1, |vm, this, args| {
        let list = receiver(vm, &this)?;
        webidl::required(args, 1, "contains")?;
        let token = text(vm, args[0].clone())?;
        let current = list.scan_tokens(vm)?;
        Ok(current.contains(&token).into())
    })?;
    for name in ["add", "remove"] {
        let errors = errors.clone();
        webidl::operation(vm, proto.clone(), name, 0, move |vm, this, args| {
            let list = receiver(vm, &this)?;
            let supplied = args
                .iter()
                .map(|arg| text(vm, arg.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            for token in &supplied {
                validate(vm, &errors, token)?;
            }
            let current = list.scan_tokens(vm)?;
            let mut tokens: Vec<_> = current.iter().map(String::as_str).collect();
            if name == "add" {
                let mut seen: HashSet<_> = tokens.iter().copied().collect();
                tokens.extend(
                    supplied
                        .iter()
                        .map(String::as_str)
                        .filter(|token| seen.insert(*token)),
                );
            } else {
                let removed: HashSet<_> = supplied.iter().map(String::as_str).collect();
                tokens.retain(|token| !removed.contains(token));
            }
            list.update(vm, &tokens)?;
            Ok(Value::Undefined)
        })?;
    }
    let toggle_errors = errors.clone();
    webidl::operation(vm, proto.clone(), "toggle", 1, move |vm, this, args| {
        let list = receiver(vm, &this)?;
        webidl::required(args, 1, "toggle")?;
        let token = text(vm, args[0].clone())?;
        validate(vm, &toggle_errors, &token)?;
        let current = list.scan_tokens(vm)?;
        let mut tokens: Vec<_> = current.iter().map(String::as_str).collect();
        let position = tokens.iter().position(|a| *a == token);
        let desired = args
            .get(1)
            .filter(|v| !v.is_undefined())
            .map_or(position.is_none(), Value::is_truthy);
        if desired != position.is_some() {
            if let Some(i) = position {
                tokens.remove(i);
            } else {
                tokens.push(&token);
            }
            list.update(vm, &tokens)?;
        }
        Ok(desired.into())
    })?;
    let replace_errors = errors.clone();
    webidl::operation(vm, proto.clone(), "replace", 2, move |vm, this, args| {
        let list = receiver(vm, &this)?;
        webidl::required(args, 2, "replace")?;
        let old = text(vm, args[0].clone())?;
        let new = text(vm, args[1].clone())?;
        // replace checks both tokens for emptiness before testing either for whitespace.
        if new.is_empty() {
            return Err(replace_errors.exception(vm, "SyntaxError", "A token must not be empty"));
        }
        validate(vm, &replace_errors, &old)?;
        validate(vm, &replace_errors, &new)?;
        let current = list.scan_tokens(vm)?;
        let mut tokens: Vec<_> = current.iter().map(String::as_str).collect();
        let Some(i) = tokens.iter().position(|token| *token == old) else {
            return Ok(false.into());
        };
        if let Some(j) = tokens.iter().position(|token| *token == new) {
            if i != j {
                tokens.remove(i.max(j));
                tokens[i.min(j)] = &new;
            }
        } else {
            tokens[i] = &new;
        }
        list.update(vm, &tokens)?;
        Ok(true.into())
    })?;
    webidl::operation(vm, proto.clone(), "supports", 1, |vm, this, args| {
        receiver(vm, &this)?;
        webidl::required(args, 1, "supports")?;
        text(vm, args[0].clone())?;
        Err(JsError::type_error(
            "The class attribute has no supported tokens",
        ))
    })?;
    let array = vm.get_global("Array")?;
    let array = vm.get_property(array, "prototype")?;
    for name in ["keys", "values", "entries", "forEach"] {
        let operation = vm.get_property(array.clone(), name)?;
        vm.define_data_property(proto.clone(), name, operation.clone(), true, true, true)?;
        if name == "values" {
            vm.define_data_property_key(
                proto.clone(),
                webidl::well_known_key(vm, "iterator"),
                operation,
                true,
                false,
                true,
            )?;
        }
    }
    Ok(())
}
