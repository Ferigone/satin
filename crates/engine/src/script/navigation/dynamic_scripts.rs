//! Script elements retain their preparation state independently of their DOM
//! parent. The DOM commits each mutation before running post-connection steps.
use super::parser_host::{
    ScriptTag, Source, connected, cors_attr, integrity_attr, resolve_resource,
};
use crate::dom::Namespace;
use crate::html::ParserScriptKind;
use crate::mime::MimeType;
use crate::script::{
    Document, ErrorKind, JsError, NodeId, NodeKind, Shared, State, Value, Vm, arg, diagnostic,
    document_base, fatal, method, node, run_classic, set_attribute, set_text_and_react, text,
};
use std::{collections::HashMap, sync::Arc};

const SCRIPT_LIMIT: usize = 192;
const SOURCE_LIMIT: usize = 1024 * 1024;
const PREPARATION_BYTES: usize = 8 * 1024 * 1024;
const INLINE_NESTING: usize = 6;

#[derive(Clone, Copy)]
pub(super) struct Flags {
    parser: bool,
    started: bool,
    force_async: bool,
}
impl Default for Flags {
    fn default() -> Self {
        Self {
            parser: false,
            started: false,
            force_async: true,
        }
    }
}
impl Flags {
    pub(super) const PARSING: Self = Self {
        parser: true,
        started: false,
        force_async: false,
    };
    pub(super) const PARSER_STARTED: Self = Self {
        started: true,
        ..Self::PARSING
    };
    const FRAGMENT_INERT: Self = Self {
        parser: false,
        started: true,
        force_async: false,
    };
}

#[derive(Default)]
pub(in crate::script) struct DynamicScripts {
    pub(super) flags: HashMap<NodeId, Flags>,
    pub(super) pending: Vec<ScriptTag>,
    pub(super) prepared: usize,
    bytes: usize,
    inline_depth: usize,
}
impl DynamicScripts {
    pub(in crate::script) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(in crate::script) fn mark_fragment_inert(&mut self, node: NodeId) {
        self.flags.insert(node, Flags::FRAGMENT_INERT);
    }

    pub(in crate::script) fn has_started(&self, source: NodeId) -> bool {
        self.flags.get(&source).is_some_and(|flags| flags.started)
    }

    pub(in crate::script) fn mark_clone_started(&mut self, clone: NodeId) {
        // Cloning copies only the already-started flag.
        self.flags.insert(
            clone,
            Flags {
                started: true,
                ..Flags::default()
            },
        );
    }

    pub(in crate::script) fn seed_prepared(&mut self, doc: &Document) {
        for node in doc.html_elements("script") {
            if has_source(doc, node) && kind(doc, node).is_some() {
                self.flags.insert(node, Flags::PARSER_STARTED);
            }
        }
    }

    fn flags(&self, node: NodeId) -> Flags {
        self.flags.get(&node).copied().unwrap_or_default()
    }
}

pub(in crate::script) fn is_script(doc: &Document, node: NodeId) -> bool {
    doc.tag(node) == "script" && doc.namespace(node) == Namespace::Html
}

pub(super) fn child_text(doc: &Document, node: NodeId) -> impl Iterator<Item = &str> {
    doc.nodes[node.0]
        .children
        .iter()
        .filter_map(|id| match &doc.nodes[id.0].kind {
            NodeKind::Text(value) => Some(value.as_str()),
            _ => None,
        })
}

pub(in crate::script) fn has_source(doc: &Document, node: NodeId) -> bool {
    doc.attr(node, "src").is_some() || child_text(doc, node).any(|text| !text.is_empty())
}

pub(super) fn classic_disabled(doc: &Document, node: NodeId) -> bool {
    doc.attr(node, "nomodule").is_some()
        || matches!(
            (doc.attr(node, "for"), doc.attr(node, "event")),
            (Some(target), Some(event)) if {
                let event = ascii_trim(event);
                !ascii_trim(target).eq_ignore_ascii_case("window")
                    || !(event.eq_ignore_ascii_case("onload")
                        || event.eq_ignore_ascii_case("onload()"))
            }
        )
}

fn ascii_trim(value: &str) -> &str {
    value.trim_matches(['\t', '\n', '\x0c', '\r', ' '])
}

pub(in crate::script) fn kind(doc: &Document, id: NodeId) -> Option<ParserScriptKind> {
    let value = match doc.attr(id, "type") {
        Some(value) if value.eq_ignore_ascii_case("module") => {
            return Some(ParserScriptKind::Module);
        }
        Some(value) if value.eq_ignore_ascii_case("importmap") => {
            return Some(ParserScriptKind::ImportMap);
        }
        Some("") => return Some(ParserScriptKind::ClassicBlocking),
        Some(value) => {
            let value = ascii_trim(value);
            if value.len() > 32 {
                return None;
            }
            value.to_ascii_lowercase()
        }
        None => match doc.attr(id, "language") {
            None | Some("") => return Some(ParserScriptKind::ClassicBlocking),
            Some(value) if value.len() <= 20 => format!("text/{}", value.to_ascii_lowercase()),
            Some(_) => return None,
        },
    };
    // The IDL type attribute uses an exact MIME essence match.
    MimeType {
        essence: value,
        parameters: vec![],
    }
    .javascript()
    .then_some(ParserScriptKind::ClassicBlocking)
}

pub(in crate::script) fn attribute_changed(
    vm: &mut Vm,
    state: &Shared,
    id: NodeId,
    name: &str,
    present: bool,
) -> Result<(), JsError> {
    if present && matches!(name, "async" | "src") && is_script(&state.borrow().document, id) {
        if name == "async" {
            state
                .borrow_mut()
                .dynamic_scripts
                .flags
                .entry(id)
                .or_default()
                .force_async = false;
        } else {
            prepare(vm, state, id)?;
        }
    }
    Ok(())
}

/// DOM insertion collects a static list before running any user code. Each
/// candidate checks connectivity again because another script can detach it.
pub(in crate::script) fn inserted(
    state: &State,
    parent: NodeId,
    roots: Vec<NodeId>,
    mut candidates: Vec<NodeId>,
) -> Vec<NodeId> {
    let doc = &state.document;
    if is_script(doc, parent) {
        candidates.push(parent);
    }
    if connected(doc, parent) {
        let mut pending = roots;
        pending.reverse();
        while let Some(id) = pending.pop() {
            if is_script(doc, id) {
                candidates.push(id);
            }
            pending.extend(doc.nodes[id.0].children.iter().rev().copied());
        }
    }
    candidates
}

pub(in crate::script) fn post_connection(
    vm: &mut Vm,
    state: &Shared,
    candidates: impl IntoIterator<Item = NodeId>,
) -> Result<(), JsError> {
    super::super::frames::sweep(vm, state);
    let result = candidates.into_iter().try_for_each(|id| prepare(vm, state, id));
    let reactions = super::super::custom_elements::flush(vm, state);
    result.and(reactions)
}

fn prepare(vm: &mut Vm, host: &Shared, id: NodeId) -> Result<(), JsError> {
    let inline = {
        let mut state = host.borrow_mut();
        let flags = state.dynamic_scripts.flags(id);
        let doc = &state.document;
        if flags.started || flags.parser || !is_script(doc, id) || !connected(doc, id) {
            return Ok(());
        }
        vm.charge_work(doc.attr(id, "type").map_or(0, str::len))?;
        let Some(kind) = kind(doc, id) else {
            return Ok(());
        };
        let src = doc.attr(id, "src");
        if src.is_none() {
            vm.charge_work(doc.nodes[id.0].children.len())?;
        }
        let size = src.map_or_else(|| child_text(doc, id).map(str::len).sum(), str::len);
        if size == 0 && src.is_none() {
            return Ok(());
        }
        let disabled = kind == ParserScriptKind::ClassicBlocking && classic_disabled(doc, id);
        let asynchronous = flags.force_async || doc.attr(id, "async").is_some();
        state.dynamic_scripts.flags.entry(id).or_default().started = true;
        if disabled {
            return Ok(());
        }
        if kind == ParserScriptKind::ImportMap {
            drop(state);
            diagnostic(host, "Dynamic import maps are not implemented".into());
            return Ok(());
        }
        if state.dynamic_scripts.prepared >= SCRIPT_LIMIT {
            return Err(limit("Dynamic script count limit exceeded"));
        }
        let doc = &state.document;
        let external = doc.attr(id, "src");
        let inline_classic = external.is_none() && kind == ParserScriptKind::ClassicBlocking;
        if external.is_none() && size > SOURCE_LIMIT {
            return Err(limit("Inline dynamic script source exceeds 1 MiB"));
        }
        if inline_classic && state.dynamic_scripts.inline_depth >= INLINE_NESTING {
            return Err(limit("Nested dynamic inline script limit exceeded"));
        }
        let mut bytes = size;
        if !inline_classic {
            bytes = bytes.saturating_add(doc.attr(id, "integrity").map_or(0, str::len));
        }
        vm.charge_work(doc.nodes.len())?;
        if state.dynamic_scripts.bytes.saturating_add(bytes) > PREPARATION_BYTES {
            return Err(limit("Dynamic script preparation data exceeds 8 MiB"));
        }
        vm.charge_work(bytes)?;
        let base = Arc::new(document_base(&state));
        bytes = bytes.saturating_add(base.as_str().len());
        if state.dynamic_scripts.bytes.saturating_add(bytes) > PREPARATION_BYTES {
            return Err(limit("Dynamic script preparation data exceeds 8 MiB"));
        }
        vm.charge_work(base.as_str().len())?;
        let inline = if inline_classic {
            Some((child_text(doc, id).collect::<String>(), base))
        } else {
            let source = match external {
                Some(src) => Source::External(resolve_resource(&state.url, &base, src)),
                None => Source::Inline(child_text(doc, id).collect()),
            };
            if let Source::External(Ok(url)) = &source {
                bytes = bytes.saturating_add(url.as_str().len());
            }
            if state.dynamic_scripts.bytes.saturating_add(bytes) > PREPARATION_BYTES {
                return Err(limit("Dynamic script preparation data exceeds 8 MiB"));
            }
            let tag = ScriptTag {
                node: id,
                kind,
                // Invalid src queues an error before the script could join
                // the ordered execution list.
                asynchronous: asynchronous || matches!(source, Source::External(Err(_))),
                base,
                source,
                cors: cors_attr(doc, id),
                integrity: integrity_attr(doc, id),
            };
            state.dynamic_scripts.pending.push(tag);
            None
        };
        state.dynamic_scripts.prepared += 1;
        state.dynamic_scripts.bytes += bytes;
        state.dynamic_scripts.inline_depth += usize::from(inline.is_some());
        inline
    };
    if let Some((source, base)) = inline {
        let result = run_classic(vm, host, id, &source, base.as_str(), false, false);
        host.borrow_mut().dynamic_scripts.inline_depth -= 1;
        if let Err(error) = result {
            if fatal(&error) {
                return Err(error);
            }
            diagnostic(host, format!("Dynamic inline script: {error}"));
        }
    }
    Ok(())
}

fn limit(message: &str) -> JsError {
    JsError::new(ErrorKind::ExecutionLimit, message)
}

fn script_node(state: &Shared, receiver: &Value) -> Result<NodeId, JsError> {
    let id = node(state, receiver)?;
    if is_script(&state.borrow().document, id) {
        Ok(id)
    } else {
        Err(JsError::type_error("Expected an HTMLScriptElement"))
    }
}

pub(in crate::script) fn install(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
) -> Result<(), JsError> {
    let constructor = vm.new_native_constructor("HTMLScriptElement", |_, _, _, _| {
        Err(JsError::type_error("Illegal constructor"))
    });
    vm.define_data_property(
        constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )?;
    vm.define_data_property(
        prototype.clone(),
        "constructor",
        constructor.clone(),
        true,
        false,
        true,
    )?;
    method(vm, constructor.clone(), "supports", |vm, _, args| {
        if args.is_empty() {
            return Err(JsError::type_error(
                "HTMLScriptElement.supports requires one argument",
            ));
        }
        let value = text(vm, arg(args, 0))?;
        Ok(matches!(value.as_str(), "classic" | "module" | "importmap").into())
    })?;
    vm.set_global("HTMLScriptElement", constructor)?;

    for (property, attribute) in [
        ("src", "src"),
        ("type", "type"),
        ("async", "async"),
        ("defer", "defer"),
        ("noModule", "nomodule"),
        ("integrity", "integrity"),
        ("crossOrigin", "crossorigin"),
        ("text", ""),
        ("innerText", ""),
    ] {
        let get_state = state.clone();
        let get = vm.new_native_function(property, move |vm, receiver, _| {
            let id = script_node(&get_state, &receiver)?;
            let state = get_state.borrow();
            let doc = &state.document;
            let attr = doc.attr(id, attribute);
            Ok(match property {
                "async" => (state.dynamic_scripts.flags(id).force_async || attr.is_some()).into(),
                "defer" | "noModule" => attr.is_some().into(),
                "src" if attr.is_some() => {
                    let source = attr.expect("checked");
                    vm.charge_work(doc.nodes.len().saturating_add(source.len()))?;
                    document_base(&state)
                        .join(source)
                        .map_or_else(|_| source.to_owned(), |url| url.to_string())
                        .into()
                }
                "text" | "innerText" => {
                    vm.charge_work(
                        doc.nodes[id.0].children.len()
                            + child_text(doc, id).map(str::len).sum::<usize>(),
                    )?;
                    child_text(doc, id).collect::<String>().into()
                }
                "crossOrigin" => match attr {
                    None => Value::Null,
                    Some(value) if value.eq_ignore_ascii_case("use-credentials") => {
                        "use-credentials".into()
                    }
                    Some(_) => "anonymous".into(),
                },
                _ => {
                    let value = attr.unwrap_or("");
                    vm.charge_work(value.len())?;
                    value.into()
                }
            })
        });
        let set_state = state.clone();
        let set = vm.new_native_function(property, move |vm, receiver, args| {
            let id = script_node(&set_state, &receiver)?;
            let value = arg(args, 0);
            if attribute.is_empty() {
                let value = if property == "innerText" && value == Value::Null {
                    String::new()
                } else {
                    text(vm, value)?
                };
                set_text_and_react(vm, &set_state, id, value)?;
            } else {
                let value = match property {
                    "async" | "defer" | "noModule" => {
                        if property == "async" {
                            set_state
                                .borrow_mut()
                                .dynamic_scripts
                                .flags
                                .entry(id)
                                .or_default()
                                .force_async = false;
                        }
                        value.is_truthy().then(String::new)
                    }
                    "crossOrigin" if value.is_nullish() => None,
                    _ => Some(text(vm, value)?),
                };
                set_attribute(vm, &set_state, id, attribute, value)?;
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), property, Some(get), Some(set))?;
    }
    Ok(())
}
