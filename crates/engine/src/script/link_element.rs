//! Reflected link properties used when discovering and inserting stylesheets.
use super::{Shared, State, arg, document_base, element, set_attribute, text, webidl};
use crate::dom::{Document, Namespace, NodeId};
use rust_browser_javascript::{JsError, Value, Vm};

pub(super) fn is_link(document: &Document, id: NodeId) -> bool {
    document.namespace(id) == Namespace::Html && document.tag(id) == "link"
}

pub(super) fn stylesheet_href(document: &Document, node: NodeId) -> Option<&str> {
    if is_link(document, node)
        && document.has_link_relation(node, "stylesheet")
        && !document.has_link_relation(node, "alternate")
        && document.attr(node, "disabled").is_none()
        && document.attr(node, "type").is_none_or(|ty| {
            ty.trim().is_empty() || ty.trim().eq_ignore_ascii_case("text/css")
        })
    {
        document.attr(node, "href")
    } else {
        None
    }
}

pub(super) fn attribute_changed(state: &mut State, node: NodeId, name: &str) {
    if state.navigation_attached
        && matches!(name, "href" | "rel" | "type" | "media" | "crossorigin" | "integrity" | "disabled")
        && is_link(&state.document, node)
    {
        state.link_updates.insert(node);
    }
}

fn receiver(state: &Shared, this: &Value) -> Result<NodeId, JsError> {
    let id = element(state, this)?;
    if is_link(&state.borrow().document, id) {
        Ok(id)
    } else {
        Err(JsError::type_error("Expected an HTMLLinkElement"))
    }
}

pub(super) fn install(vm: &mut Vm, state: &Shared, prototype: Value) -> Result<(), JsError> {
    webidl::interface(vm, prototype.clone(), "HTMLLinkElement", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal HTMLLinkElement constructor"))
    })?;
    for (property, attribute) in [
        ("rel", "rel"),
        ("href", "href"),
        ("type", "type"),
        ("media", "media"),
        ("crossOrigin", "crossorigin"),
        ("integrity", "integrity"),
    ] {
        let host = state.clone();
        let get = vm.new_native_function(property, move |vm, this, _| {
            let id = receiver(&host, &this)?;
            let mut state = host.borrow_mut();
            if property == "href"
                && let Some(cached) = state.absolute_link_hrefs.get(&id)
            {
                return Ok(cached.clone().into());
            }
            let Some(raw) = state.document.attr(id, attribute) else {
                return Ok(if attribute == "crossorigin" { Value::Null } else { "".into() });
            };
            vm.charge_work(raw.len())?;
            if property == "href" {
                // Absolute authority URLs are independent of the document's base.
                // Retain bounded serializations and discard them on href edits.
                const MAX_CACHED_LINKS: usize = 256;
                const MAX_CACHED_URL_BYTES: usize = 4096;
                if raw.len() <= MAX_CACHED_URL_BYTES
                    && let Some((scheme, _)) = raw.split_once("://")
                    && let Ok(url) = url::Url::parse(raw)
                    && url.has_host()
                    && url.scheme().eq_ignore_ascii_case(scheme)
                    && url.as_str().len() <= MAX_CACHED_URL_BYTES
                {
                    let value = rust_browser_javascript::JsString::from(url.as_str());
                    if state.absolute_link_hrefs.len() == MAX_CACHED_LINKS {
                        state.absolute_link_hrefs.clear();
                    }
                    state.absolute_link_hrefs.insert(id, value.clone());
                    return Ok(value.into());
                }
                return Ok(document_base(&state)
                    .join(raw)
                    .map_or_else(|_| raw.to_owned(), |url| url.to_string())
                    .into());
            }
            if attribute == "crossorigin" {
                return Ok(if raw.eq_ignore_ascii_case("use-credentials") {
                    "use-credentials".into()
                } else {
                    "anonymous".into()
                });
            }
            Ok(raw.into())
        });
        let host = state.clone();
        let set = vm.new_native_function(property, move |vm, this, args| {
            let id = receiver(&host, &this)?;
            let value = arg(args, 0);
            let value = if attribute == "crossorigin" && value.is_nullish() {
                None
            } else {
                Some(text(vm, value)?)
            };
            set_attribute(vm, &host, id, attribute, value)?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), property, Some(get), Some(set))?;
    }
    let host = state.clone();
    let get = vm.new_native_function("get disabled", move |_, this, _| {
        let node = receiver(&host, &this)?;
        Ok(host.borrow().document.attr(node, "disabled").is_some().into())
    });
    let host = state.clone();
    let set = vm.new_native_function("set disabled", move |vm, this, args| {
        let node = receiver(&host, &this)?;
        set_attribute(vm, &host, node, "disabled", arg(args, 0).is_truthy().then(String::new))?;
        Ok(Value::Undefined)
    });
    vm.define_accessor(prototype, "disabled", Some(get), Some(set))?;
    Ok(())
}
