//! Shadow roots are separate DOM trees owned by an element.
use super::{
    Shared, arg, custom_elements, dom_api, element, getter, mark_dirty, node, text, unsupported,
    webidl, wrap,
};
use crate::dom::{Namespace, ShadowMode, ShadowTree};
use rust_browser_javascript::{JsError, Value, Vm};

fn can_host(tag: &str) -> bool {
    matches!(
        tag,
        "article"
            | "aside"
            | "blockquote"
            | "body"
            | "div"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "main"
            | "nav"
            | "p"
            | "section"
            | "span"
    ) || custom_elements::valid_name(tag)
}

fn options(vm: &mut Vm, init: Value) -> Result<ShadowMode, JsError> {
    if init.as_object().is_none() {
        return Err(JsError::type_error(
            "ShadowRootInit requires a dictionary with a mode",
        ));
    }
    // Web IDL dictionary conversion observes getters in lexicographic order.
    let clonable = vm.get_property(init.clone(), "clonable")?.is_truthy();
    let registry = vm.get_property(init.clone(), "customElementRegistry")?;
    let delegates = vm.get_property(init.clone(), "delegatesFocus")?.is_truthy();
    let mode = vm.get_property(init.clone(), "mode")?;
    let mode = match text(vm, mode)?.as_str() {
        "open" => ShadowMode::Open,
        "closed" => ShadowMode::Closed,
        _ => return Err(JsError::type_error("Unknown ShadowRoot mode")),
    };
    let serializable = vm.get_property(init.clone(), "serializable")?.is_truthy();
    let assignment = vm.get_property(init, "slotAssignment")?;
    let manual = if assignment.is_undefined() {
        false
    } else {
        match text(vm, assignment)?.as_str() {
            "named" => false,
            "manual" => true,
            _ => return Err(JsError::type_error("Unknown slot assignment mode")),
        }
    };
    if clonable || delegates || serializable || manual || !registry.is_undefined() {
        return Err(unsupported(
            "This ShadowRootInit configuration is not implemented",
        ));
    }
    Ok(mode)
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    errors: &dom_api::Errors,
) -> Result<(), JsError> {
    let root_proto = state.borrow().shadow_prototype.clone();
    dom_api::install_shadow_html(vm, state, root_proto.clone(), errors)?;
    let constructor = webidl::interface(vm, root_proto.clone(), "ShadowRoot", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal ShadowRoot constructor"))
    })?;
    let fragment = vm.get_global("DocumentFragment")?;
    vm.set_prototype(
        constructor.as_object().expect("ShadowRoot constructor"),
        fragment.as_object(),
    )?;

    let elem_proto = state.borrow().element_prototype.clone();
    let attached = state.clone();
    let errors = errors.clone();
    webidl::operation(
        vm,
        elem_proto.clone(),
        "attachShadow",
        1,
        move |vm, this, args| {
            let id = element(&attached, &this)?;
            webidl::required(args, 1, "Element.attachShadow")?;
            let mode = options(vm, arg(args, 0))?;
            let prototype = {
                let state = attached.borrow();
                let doc = &state.document;
                if doc.namespace(id) != Namespace::Html
                    || !can_host(doc.tag(id))
                    || state.custom_elements.borrow().shadow_disabled(doc.tag(id))
                {
                    return Err(errors.exception(
                        vm,
                        "NotSupportedError",
                        "This element cannot host a shadow root",
                    ));
                }
                if doc.shadow_roots.contains_key(&id) {
                    return Err(errors.exception(
                        vm,
                        "NotSupportedError",
                        "The element already has a shadow root",
                    ));
                }
                state.shadow_prototype.clone()
            };
            let value = dom_api::new_fragment(vm, &attached, &errors, prototype)?;
            let root = node(&attached, &value)?;
            let mut state = attached.borrow_mut();
            state
                .document
                .shadow_roots
                .insert(id, ShadowTree { root, mode });
            state.document.shadow_hosts.insert(root, id);
            mark_dirty(&mut state);
            Ok(value)
        },
    )?;
    let roots = state.clone();
    getter(vm, elem_proto, "shadowRoot", move |vm, this, _| {
        let host = element(&roots, &this)?;
        let root = roots
            .borrow()
            .document
            .shadow_roots
            .get(&host)
            .filter(|tree| tree.mode == ShadowMode::Open)
            .map(|tree| tree.root);
        wrap(vm, &roots, root)
    })?;
    for property in [
        "host",
        "mode",
        "delegatesFocus",
        "clonable",
        "serializable",
        "slotAssignment",
    ] {
        let state = state.clone();
        getter(vm, root_proto.clone(), property, move |vm, this, _| {
            let root = node(&state, &this)?;
            let (host, mode) = {
                let state = state.borrow();
                let doc = &state.document;
                let host = *doc
                    .shadow_hosts
                    .get(&root)
                    .ok_or_else(|| JsError::type_error("Expected a ShadowRoot receiver"))?;
                (host, doc.shadow_roots[&host].mode)
            };
            match property {
                "host" => wrap(vm, &state, Some(host)),
                "mode" => Ok(match mode {
                    ShadowMode::Open => "open",
                    ShadowMode::Closed => "closed",
                }
                .into()),
                "slotAssignment" => Ok("named".into()),
                _ => Ok(false.into()),
            }
        })?;
    }

    let nodes = state.clone();
    let node_proto = state.borrow().prototype.clone();
    webidl::operation(vm, node_proto, "getRootNode", 0, move |vm, this, args| {
        let mut id = node(&nodes, &this)?;
        let options = arg(args, 0);
        let composed = match options {
            Value::Null | Value::Undefined => false,
            _ if options.as_object().is_some() => vm.get_property(options, "composed")?.is_truthy(),
            _ => {
                return Err(JsError::type_error(
                    "GetRootNodeOptions must be a dictionary",
                ));
            }
        };
        {
            let state = nodes.borrow();
            let doc = &state.document;
            loop {
                let parent = doc.nodes[id.0].parent.or_else(|| {
                    composed
                        .then(|| doc.shadow_hosts.get(&id).copied())
                        .flatten()
                });
                let Some(parent) = parent else {
                    break;
                };
                vm.charge_work(1)?;
                id = parent;
            }
        }
        wrap(vm, &nodes, Some(id))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{LoadedPage, script::ScriptRuntime};
    use rust_browser_javascript::Value;

    #[test]
    fn nested_shadow_roots_are_distinct_trees_and_preserve_closed_mode() {
        let page = LoadedPage::from_html(
            "https://example.test/",
            "<body><div id=host><i id=light></i></div>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let result = runtime.eval(r#"
        const host = document.getElementById('host');
        const open = host.attachShadow({mode:'open'});
        open.innerHTML = '<section id="inner"></section>';
        const inner = open.querySelector('#inner');
        const closed = inner.attachShadow({mode:'closed'});
        const item = document.createElement('b');
        item.id = 'leaf';
        closed.appendChild(item);
        const requiredError = (f, name) => {try {f()} catch (e) {return e.name === name} return false};
        const valid = open === host.shadowRoot && inner.shadowRoot === null &&
            open.host === host && closed.host === inner && closed.mode === 'closed' &&
            closed.parentNode === null && closed.ownerDocument === document &&
            closed instanceof ShadowRoot && closed instanceof DocumentFragment &&
            Object.getPrototypeOf(ShadowRoot) === DocumentFragment && item.parentNode === closed &&
            host.querySelector('#light') === host.firstChild && open.querySelector('#light') === null &&
            document.querySelector('#inner') === null && open.querySelector('#leaf') === null &&
            closed.querySelector('#leaf') === item && item.getRootNode() === closed &&
            item.getRootNode({composed:true}) === document &&
            open.innerHTML === '<section id="inner"></section>' &&
            requiredError(() => inner.attachShadow({mode:'closed'}), 'NotSupportedError') &&
            requiredError(() => open.appendChild(host), 'HierarchyRequestError') &&
            requiredError(() => closed.cloneNode(true), 'NotSupportedError') &&
            requiredError(() => document.importNode(open, true), 'NotSupportedError') &&
            requiredError(() => document.createElement('img').attachShadow({mode:'open'}), 'NotSupportedError') &&
            requiredError(() => new ShadowRoot(), 'TypeError');
        host.remove();
        valid && item.getRootNode({composed:true}) === host && host.shadowRoot === open
        "#).unwrap();
        assert_eq!(result, Value::Bool(true));
    }
}
