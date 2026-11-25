//! Native form state and defaults over the shared DOM/event implementation.
//! Values live separately from content attributes, as HTML's dirty flags require.
mod file_inputs;
use super::file_list::FileListHost;
use super::form_data::{self, Encoding, EntryList, FormDataHost};
use super::*;
use crate::{dom::Namespace, history::NavigationMode};
use file_inputs::FileInputs;
pub use file_inputs::{
    FilePickerRequest, MAX_PICKER_BYTES, MAX_PICKER_FILE_BYTES, MAX_PICKER_FILES, SelectedFile,
};
use rust_browser_javascript::JsString;
use std::collections::{HashSet, VecDeque};
use url::Url;

const VALUE_UNITS: usize = 1024 * 1024;
const LIVE_VALUE_BYTES: usize = 8 * 1024 * 1024;
const QUEUED_SUBMISSIONS: usize = 8;
const SNAPSHOT_UNITS: usize = 8192;

#[derive(Clone, Debug, PartialEq)]
pub struct FormSubmission {
    pub form: NodeId,
    pub submitter: Option<NodeId>,
    pub url: String,
    pub method: String,
    pub content_type: Option<String>,
    pub body: Option<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct OptionSnapshot {
    pub node: NodeId,
    pub label: String,
    pub value: String,
    pub selected: bool,
    pub disabled: bool,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlSnapshot {
    pub kind: String,
    pub value: String,
    pub checked: bool,
    pub selected: bool,
    pub disabled: bool,
    pub read_only: bool,
    pub placeholder_shown: bool,
    pub focused: bool,
    pub selection_start: usize,
    pub selection_end: usize,
    pub options: Vec<OptionSnapshot>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormKey {
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    Backspace,
    Delete,
    Enter,
    Tab,
    Space,
    SelectAll,
}
#[derive(Clone, Default)]
struct Control {
    value: Option<JsString>,
    checked: Option<bool>,
    checked_dirty: bool,
    selected: Option<bool>,
    selected_dirty: bool,
    indeterminate: bool,
    start: usize,
    end: usize,
    anchor: usize,
}
struct Data {
    controls: HashMap<NodeId, Control>,
    select_none: HashSet<NodeId>,
    focused: Option<NodeId>,
    focus_value: Option<JsString>,
    user_edited: bool,
    activating: HashSet<NodeId>,
    submitting: HashSet<NodeId>,
    constructing_entry_list: HashSet<NodeId>,
    resetting: HashSet<NodeId>,
    submissions: VecDeque<FormSubmission>,
    exception_constructor: Option<RootId>,
    form_data: Option<FormDataHost>,
    file_inputs: FileInputs,
}
#[derive(Clone)]
pub(super) struct FormHost {
    dom: Shared,
    data: Rc<RefCell<Data>>,
    file_lists: FileListHost,
}

fn html(doc: &Document, id: NodeId, tag: &str) -> bool {
    doc.namespace(id) == Namespace::Html && doc.tag(id) == tag
}
fn is_hyperlink(doc: &Document, id: NodeId) -> bool {
    html(doc, id, "a") && attr(doc, id, "href")
}
fn focus_candidate(doc: &Document, id: NodeId) -> bool {
    is_hyperlink(doc, id) || (is_labelable(doc, id) && !disabled(doc, id))
}
fn is_control(doc: &Document, id: NodeId) -> bool {
    doc.namespace(id) == Namespace::Html
        && matches!(
            doc.tag(id),
            "input"
                | "textarea"
                | "select"
                | "button"
                | "option"
                | "optgroup"
                | "fieldset"
                | "output"
        )
}
fn associated(doc: &Document, id: NodeId) -> bool {
    doc.namespace(id) == Namespace::Html
        && matches!(
            doc.tag(id),
            "input" | "textarea" | "select" | "button" | "fieldset" | "output" | "object"
        )
}
fn attr(doc: &Document, id: NodeId, key: &str) -> bool {
    doc.attr(id, key).is_some()
}
fn enum_value(
    value: Option<&str>,
    allowed: &[&'static str],
    default: &'static str,
) -> &'static str {
    let value = value.unwrap_or("");
    allowed
        .iter()
        .copied()
        .find(|kind| value.eq_ignore_ascii_case(kind))
        .unwrap_or(default)
}
fn hidden(doc: &Document, id: NodeId) -> bool {
    let mut current = Some(id);
    while let Some(id) = current {
        if attr(doc, id, "hidden")
            || doc.attr(id, "style").is_some_and(|style| {
                css::declarations(style)
                    .iter()
                    .any(|d| d.name == "display" && d.value.trim().eq_ignore_ascii_case("none"))
            })
        {
            return true;
        }
        current = doc.nodes[id.0].parent;
    }
    false
}
fn connected(doc: &Document, mut id: NodeId) -> bool {
    while let Some(parent) = doc.nodes[id.0].parent {
        id = parent;
    }
    id == doc.root
}
fn tree_root(doc: &Document, mut id: NodeId) -> NodeId {
    while let Some(parent) = doc.nodes[id.0].parent {
        id = parent;
    }
    id
}
fn ancestor(doc: &Document, mut id: NodeId, target: NodeId) -> bool {
    loop {
        if id == target {
            return true;
        }
        let Some(parent) = doc.nodes[id.0].parent else {
            return false;
        };
        id = parent;
    }
}
fn owner(doc: &Document, id: NodeId) -> Option<NodeId> {
    if html(doc, id, "option") {
        return select_owner(doc, id).and_then(|id| owner(doc, id));
    }
    if !associated(doc, id) {
        return None;
    }
    if let Some(name) = doc.attr(id, "form")
        && connected(doc, id)
    {
        if name.is_empty() {
            return None;
        }
        return doc
            .connected_nodes()
            .find(|&n| doc.attr(n, "id") == Some(name))
            .filter(|&n| html(doc, n, "form"));
    }
    let mut current = doc.nodes[id.0].parent;
    while let Some(id) = current {
        if html(doc, id, "form") {
            return Some(id);
        }
        current = doc.nodes[id.0].parent;
    }
    None
}
fn select_owner(doc: &Document, id: NodeId) -> Option<NodeId> {
    let mut current = doc.nodes[id.0].parent;
    while let Some(id) = current {
        if html(doc, id, "select") {
            return Some(id);
        }
        if html(doc, id, "option") {
            return None;
        }
        current = doc.nodes[id.0].parent;
    }
    None
}
fn owned_controls(doc: &Document, data: &Data, form: NodeId) -> Vec<NodeId> {
    if connected(doc, form) {
        let index = FormIndex::new(doc, data);
        doc.connected_nodes()
            .filter(|&id| associated(doc, id) && index.owners[id.0] == Some(form))
            .collect()
    } else {
        descendants(doc, form)
            .filter(|&id| associated(doc, id) && owner(doc, id) == Some(form))
            .collect()
    }
}
fn options(doc: &Document, select: NodeId) -> Vec<NodeId> {
    descendants(doc, select)
        .filter(|&id| html(doc, id, "option") && select_owner(doc, id) == Some(select))
        .collect()
}
fn disabled(doc: &Document, id: NodeId) -> bool {
    if attr(doc, id, "disabled") {
        return true;
    }
    if html(doc, id, "option") {
        return doc.nodes[id.0]
            .parent
            .is_some_and(|p| html(doc, p, "optgroup") && attr(doc, p, "disabled"));
    }
    if !associated(doc, id) {
        return false;
    }
    let mut current = doc.nodes[id.0].parent;
    while let Some(parent) = current {
        if html(doc, parent, "fieldset") && attr(doc, parent, "disabled") {
            let legend = doc.nodes[parent.0]
                .children
                .iter()
                .copied()
                .find(|&n| html(doc, n, "legend"));
            if !legend.is_some_and(|legend| ancestor(doc, id, legend)) {
                return true;
            }
        }
        current = doc.nodes[parent.0].parent;
    }
    false
}
fn input_type(doc: &Document, id: NodeId) -> String {
    let value = doc.attr(id, "type").unwrap_or("text");
    [
        "hidden",
        "text",
        "search",
        "tel",
        "url",
        "email",
        "password",
        "date",
        "month",
        "week",
        "time",
        "datetime-local",
        "number",
        "range",
        "color",
        "checkbox",
        "radio",
        "file",
        "submit",
        "image",
        "reset",
        "button",
    ]
    .into_iter()
    .find(|kind| value.eq_ignore_ascii_case(kind))
    .unwrap_or("text")
    .into()
}
fn kind(doc: &Document, id: NodeId) -> String {
    match doc.tag(id) {
        "input" => input_type(doc, id),
        "button" => {
            let value = doc.attr(id, "type").unwrap_or("");
            if value.eq_ignore_ascii_case("reset") {
                "reset".into()
            } else if value.eq_ignore_ascii_case("button") {
                "button".into()
            } else {
                "submit".into()
            }
        }
        "select" => {
            if attr(doc, id, "multiple") {
                "select-multiple".into()
            } else {
                "select-one".into()
            }
        }
        tag => tag.into(),
    }
}
fn text_editable(doc: &Document, id: NodeId) -> bool {
    html(doc, id, "textarea")
        || (html(doc, id, "input")
            && matches!(
                input_type(doc, id).as_str(),
                "text" | "search" | "tel" | "url" | "email" | "password" | "number"
            ))
}
fn placeholder_shown(doc: &Document, id: NodeId, control: Option<&Control>) -> bool {
    if !attr(doc, id, "placeholder") || !text_editable(doc, id) {
        return false;
    }
    if let Some(value) = control.and_then(|control| control.value.as_ref()) {
        return value.is_empty();
    }
    if html(doc, id, "textarea") {
        return !doc.nodes[id.0]
            .children
            .iter()
            .any(|n| matches!(&doc.nodes[n.0].kind, NodeKind::Text(text) | NodeKind::Cdata(text) if !text.is_empty()));
    }
    doc.attr(id, "value")
        .is_none_or(|value| value.is_empty() || sanitize(doc, id, value.into()).is_empty())
}
pub(crate) fn default_placeholder_shown(doc: &Document, id: NodeId) -> bool {
    placeholder_shown(doc, id, None)
}
fn selection_supported(doc: &Document, id: NodeId) -> bool {
    html(doc, id, "textarea")
        || (html(doc, id, "input")
            && matches!(
                input_type(doc, id).as_str(),
                "text" | "search" | "tel" | "url" | "password"
            ))
}
fn default_mode(doc: &Document, id: NodeId) -> bool {
    html(doc, id, "input")
        && matches!(
            input_type(doc, id).as_str(),
            "hidden" | "submit" | "image" | "reset" | "button" | "checkbox" | "radio"
        )
}
fn newline_units(units: &[u16], strip: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if unit == 13 {
            if index + 1 < units.len() && units[index + 1] == 10 {
                index += 1;
            }
            if !strip {
                out.push(10);
            }
        } else if unit != 10 || !strip {
            out.push(unit);
        }
        index += 1;
    }
    out
}
fn sanitize(doc: &Document, id: NodeId, value: JsString) -> JsString {
    if html(doc, id, "textarea") {
        return JsString::from_utf16(newline_units(value.as_utf16(), false));
    }
    if !html(doc, id, "input") {
        return value;
    }
    let ty = input_type(doc, id);
    let mut units = if matches!(
        ty.as_str(),
        "text" | "search" | "tel" | "url" | "email" | "password"
    ) {
        newline_units(value.as_utf16(), true)
    } else {
        value.as_utf16().to_vec()
    };
    if matches!(ty.as_str(), "url" | "email") {
        let trim = |u: &u16| matches!(*u, 9 | 10 | 12 | 13 | 32);
        let start = units.iter().position(|u| !trim(u)).unwrap_or(units.len());
        let end = units
            .iter()
            .rposition(|u| !trim(u))
            .map_or(start, |i| i + 1);
        units = units[start..end].to_vec();
    }
    if ty == "number" {
        let text = String::from_utf16_lossy(&units);
        if !text.is_empty() && (!valid_number(&text)) {
            units.clear();
        }
    }
    if ty == "color" {
        let text = String::from_utf16_lossy(&units);
        return if text.len() == 7
            && text.starts_with('#')
            && text[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            text.to_ascii_lowercase().into()
        } else {
            "#000000".into()
        };
    }
    JsString::from_utf16(units)
}
fn valid_number(value: &str) -> bool {
    !value.starts_with('+')
        && !value.ends_with('.')
        && !value.chars().any(|c| c.is_ascii_whitespace())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || b"-.eE+".contains(&b))
        && value.parse::<f64>().is_ok_and(|n| n.is_finite())
}
fn option_text(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    let mut pending = doc.nodes[id.0]
        .children
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if html(doc, id, "script") {
            continue;
        }
        if let Some(text) = doc.nodes[id.0].kind.text_data() {
            out.push_str(text);
        }
        pending.extend(doc.nodes[id.0].children.iter().rev().copied());
    }
    out.split(['\t', '\n', '\x0c', '\r', ' '])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
fn option_value(doc: &Document, id: NodeId) -> JsString {
    doc.attr(id, "value")
        .map(JsString::from)
        .unwrap_or_else(|| option_text(doc, id).into())
}
fn selected(doc: &Document, data: &Data, select: NodeId) -> Vec<NodeId> {
    let list = options(doc, select);
    let mut picked = list
        .iter()
        .copied()
        .filter(|id| {
            data.controls
                .get(id)
                .and_then(|c| c.selected)
                .unwrap_or_else(|| attr(doc, *id, "selected"))
        })
        .collect::<Vec<_>>();
    if !attr(doc, select, "multiple") {
        if picked.len() > 1 {
            picked.drain(..picked.len() - 1);
        }
        if picked.is_empty()
            && !data.select_none.contains(&select)
            && doc
                .attr(select, "size")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0)
                <= 1
            && let Some(id) = list.into_iter().find(|&id| !disabled(doc, id))
        {
            picked.push(id);
        }
    }
    picked
}
fn current_value(doc: &Document, data: &Data, id: NodeId) -> JsString {
    if html(doc, id, "select") {
        return selected(doc, data, id)
            .first()
            .map(|&id| option_value(doc, id))
            .unwrap_or_default();
    }
    if html(doc, id, "option") {
        return option_value(doc, id);
    }
    if file_inputs::is_file(doc, id) {
        return data.file_inputs.value(id);
    }
    if default_mode(doc, id) || html(doc, id, "button") {
        return doc
            .attr(id, "value")
            .unwrap_or_else(|| {
                if html(doc, id, "input")
                    && matches!(input_type(doc, id).as_str(), "checkbox" | "radio")
                {
                    "on"
                } else {
                    ""
                }
            })
            .into();
    }
    if let Some(value) = data.controls.get(&id).and_then(|c| c.value.clone()) {
        return value;
    }
    let value = if html(doc, id, "textarea") {
        doc.text(id)
    } else {
        doc.attr(id, "value").unwrap_or("").into()
    };
    sanitize(doc, id, value.into())
}
fn radio_group(doc: &Document, id: NodeId) -> Vec<NodeId> {
    let name = doc.attr(id, "name").unwrap_or("");
    if name.is_empty() {
        return vec![id];
    }
    let root = tree_root(doc, id);
    let form = owner(doc, id);
    let mut pending = vec![root];
    let mut out = vec![];
    while let Some(node) = pending.pop() {
        if html(doc, node, "input")
            && input_type(doc, node) == "radio"
            && doc.attr(node, "name") == Some(name)
            && owner(doc, node) == form
        {
            out.push(node);
        }
        pending.extend(doc.nodes[node.0].children.iter().rev().copied());
    }
    out
}
fn checked(doc: &Document, data: &Data, id: NodeId) -> bool {
    if let Some(value) = data.controls.get(&id).and_then(|c| c.checked) {
        return value;
    }
    if !attr(doc, id, "checked") {
        return false;
    }
    if html(doc, id, "input") && input_type(doc, id) == "radio" {
        let group = radio_group(doc, id);
        if group
            .iter()
            .any(|id| data.controls.get(id).and_then(|c| c.checked) == Some(true))
        {
            return false;
        }
        return group.into_iter().rev().find(|id| {
            data.controls.get(id).and_then(|c| c.checked).is_none() && attr(doc, *id, "checked")
        }) == Some(id);
    }
    true
}
fn set_checked(doc: &Document, data: &mut Data, id: NodeId, value: bool) {
    if value && html(doc, id, "input") && input_type(doc, id) == "radio" {
        for other in radio_group(doc, id) {
            if other != id {
                data.controls.entry(other).or_default().checked = Some(false);
            }
        }
    }
    data.controls.entry(id).or_default().checked = Some(value);
    data.controls.entry(id).or_default().checked_dirty = true;
}
fn reflect(vm: &mut Vm, dom: &Shared, id: NodeId, name: &str, value: Value) -> Result<(), JsError> {
    let value = text(vm, value)?;
    set_attribute(vm, dom, id, name, Some(value))
}

fn value_read_work(doc: &Document, data: &Data, id: NodeId) -> usize {
    if data.controls.get(&id).is_some_and(|c| c.value.is_some()) && !default_mode(doc, id) {
        return 0;
    }
    if html(doc, id, "select") {
        return selected(doc, data, id)
            .first()
            .map_or(0, |&option| value_read_work(doc, data, option));
    }
    if html(doc, id, "textarea") || (html(doc, id, "option") && !attr(doc, id, "value")) {
        let mut work = 0usize;
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            work = work.saturating_add(1);
            if let Some(text) = doc.nodes[id.0].kind.text_data() {
                work = work.saturating_add(text.len());
            }
            pending.extend(doc.nodes[id.0].children.iter().copied());
        }
        return work;
    }
    doc.attr(id, "value").map_or(0, str::len)
}
impl FormHost {
    pub(super) fn install(
        vm: &mut Vm,
        dom: &Shared,
        prototype: Value,
        document: Value,
        file_lists: FileListHost,
    ) -> Result<Self, JsError> {
        let constructor = vm.get_global("DOMException")?;
        let data = Rc::new(RefCell::new(Data {
            controls: HashMap::new(),
            select_none: HashSet::new(),
            focused: None,
            focus_value: None,
            user_edited: false,
            activating: HashSet::new(),
            submitting: HashSet::new(),
            constructing_entry_list: HashSet::new(),
            resetting: HashSet::new(),
            submissions: VecDeque::new(),
            exception_constructor: Some(vm.root(constructor)),
            form_data: None,
            file_inputs: FileInputs::default(),
        }));
        let hook = data.clone();
        dom.borrow_mut().form_attribute_hook = Some(Rc::new(move |vm, doc, id, name| {
            attribute_changed(vm, doc, &mut hook.borrow_mut(), id, name)
        }));
        let hook = data.clone();
        let clone_lists = file_lists.clone();
        dom.borrow_mut().form_clone_hook = Some(Rc::new(move |vm, sources, base| {
            let mut data = hook.borrow_mut();
            data.file_inputs
                .clone_selections(vm, &clone_lists, sources, base)?;
            for (index, source) in sources.iter().enumerate() {
                if let Some(value) = data.controls.get(source).cloned() {
                    data.controls.insert(NodeId(base + index), value);
                }
            }
            Ok(())
        }));
        let host = Self {
            dom: dom.clone(),
            data,
            file_lists,
        };
        host.install_accessors(vm, prototype.clone())?;
        host.install_methods(vm, prototype, document)?;
        Ok(host)
    }
    fn charge(&self, vm: &mut Vm, extra: usize) -> Result<(), JsError> {
        vm.charge_work(
            self.dom
                .borrow()
                .document
                .nodes
                .len()
                .saturating_mul(4)
                .saturating_add(extra),
        )
    }
    fn exception(&self, vm: &mut Vm, name: &str, message: &str) -> JsError {
        let constructor = self
            .data
            .borrow()
            .exception_constructor
            .and_then(|root| vm.root_value(root))
            .expect("installed form exception constructor");
        match vm.construct(constructor, &[message.into(), name.into()]) {
            Ok(thrown) => JsError {
                kind: ErrorKind::Error,
                message: format!("{name}: {message}"),
                line: None,
                column: None,
                thrown: Some(thrown),
                inspected_calls: None,
            },
            Err(error) => error,
        }
    }
    pub(super) fn set_form_data(&self, host: FormDataHost) {
        self.data.borrow_mut().form_data = Some(host);
    }
    pub(super) fn form_data_args(
        &self,
        vm: &mut Vm,
        form: &Value,
        submitter: &Value,
    ) -> Result<Option<(NodeId, Option<NodeId>)>, JsError> {
        let form = if form.is_undefined() {
            None
        } else {
            let id = node(&self.dom, form)?;
            if !html(&self.dom.borrow().document, id, "form") {
                return Err(JsError::type_error("FormData requires an HTML form"));
            }
            Some(id)
        };
        let submitter = if submitter.is_nullish() {
            None
        } else {
            let id = element(&self.dom, submitter)?;
            if self.dom.borrow().document.namespace(id) != Namespace::Html {
                return Err(JsError::type_error(
                    "FormData submitter must be an HTMLElement",
                ));
            }
            Some(id)
        };
        let Some(form) = form else {
            return Ok(None);
        };
        if let Some(submitter) = submitter {
            self.charge(vm, 0)?;
            let dom = self.dom.borrow();
            if !is_submit_button(&dom.document, submitter) {
                return Err(JsError::type_error(
                    "FormData submitter is not a submit button",
                ));
            }
            if owner(&dom.document, submitter) != Some(form) {
                drop(dom);
                return Err(self.exception(
                    vm,
                    "NotFoundError",
                    "Submitter belongs to another form",
                ));
            }
        }
        Ok(Some((form, submitter)))
    }
    pub(super) fn entry_list_in_progress(&self, vm: &mut Vm) -> JsError {
        self.exception(
            vm,
            "InvalidStateError",
            "The form is constructing its entry list",
        )
    }
    pub(super) fn form_data(
        &self,
        vm: &mut Vm,
        form: NodeId,
        submitter: Option<NodeId>,
    ) -> Result<Option<Value>, JsError> {
        let host = {
            let mut data = self.data.borrow_mut();
            if !data.constructing_entry_list.insert(form) {
                return Ok(None);
            }
            data.form_data.clone().expect("installed FormData host")
        };
        let result = (|| {
            self.charge(vm, 0)?;
            let entries = {
                let dom = self.dom.borrow();
                collect_entries(
                    vm,
                    &dom.document,
                    &self.data.borrow(),
                    form,
                    submitter,
                    &host,
                )?
            };
            host.construct_form(vm, &self.dom, form, entries).map(Some)
        })();
        self.data.borrow_mut().constructing_entry_list.remove(&form);
        result
    }
    fn mark(&self) {
        mark_dirty(&mut self.dom.borrow_mut());
    }
    fn set_value(&self, vm: &mut Vm, id: NodeId, value: JsString) -> Result<(), JsError> {
        self.charge(vm, value.len())?;
        if value.len() > VALUE_UNITS {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Control value exceeds 1 Mi UTF-16 units",
            ));
        }
        let tag = self.dom.borrow().document.tag(id).to_owned();
        if tag == "select" {
            let (list, found) = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                let list = options(doc, id);
                let found = list
                    .iter()
                    .copied()
                    .find(|&n| option_value(doc, n) == value);
                (list, found)
            };
            let mut data = self.data.borrow_mut();
            for option in list {
                data.controls.entry(option).or_default().selected = Some(found == Some(option));
                if found == Some(option) {
                    data.controls.entry(option).or_default().selected_dirty = true;
                }
            }
            if found.is_none() {
                data.select_none.insert(id);
            } else {
                data.select_none.remove(&id);
            }
            drop(data);
            self.mark();
            return Ok(());
        }
        if tag == "option" || tag == "button" || default_mode(&self.dom.borrow().document, id) {
            return set_attribute(vm, &self.dom, id, "value", Some(value.to_string_lossy()));
        }
        if file_inputs::is_file(&self.dom.borrow().document, id) {
            return if value.is_empty() {
                self.data.borrow_mut().file_inputs.clear(vm, id)?;
                self.mark();
                Ok(())
            } else {
                Err(self.exception(
                    vm,
                    "InvalidStateError",
                    "File input values can only be cleared",
                ))
            };
        }
        let value = sanitize(&self.dom.borrow().document, id, value);
        let mut data = self.data.borrow_mut();
        let mut seen = HashSet::new();
        let mut bytes = 0usize;
        for (&node, control) in &data.controls {
            if node != id
                && let Some(value) = &control.value
                && seen.insert(value.as_utf16().as_ptr() as usize)
            {
                bytes = bytes.saturating_add(value.len() * 2 + 64);
            }
        }
        if seen.insert(value.as_utf16().as_ptr() as usize) {
            bytes = bytes.saturating_add(value.len() * 2 + 64);
        }
        if bytes > LIVE_VALUE_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Dirty form values exceed 8 MiB",
            ));
        }
        let control = data.controls.entry(id).or_default();
        let changed = control.value.as_ref() != Some(&value);
        control.value = Some(value.clone());
        if changed {
            control.start = value.len();
            control.end = value.len();
            control.anchor = value.len();
        }
        drop(data);
        self.mark();
        Ok(())
    }
    pub(super) fn matches_form_pseudo(
        &self,
        doc: &Document,
        node: NodeId,
        pseudo: css::FormPseudoClass,
    ) -> bool {
        let data = self.data.borrow();
        match pseudo {
            css::FormPseudoClass::Focus => data.focused == Some(node) && connected(doc, node),
            css::FormPseudoClass::CheckedInput => checked(doc, &data, node),
            css::FormPseudoClass::PlaceholderShown => {
                placeholder_shown(doc, node, data.controls.get(&node))
            }
        }
    }
    pub(super) fn focused(&self) -> Option<NodeId> {
        self.data
            .borrow()
            .focused
            .filter(|&id| connected(&self.dom.borrow().document, id))
    }
    pub(super) fn focus_next(&self, vm: &mut Vm, reverse: bool) -> Result<bool, JsError> {
        self.charge(vm, 0)?;
        let mut candidates = {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            doc.connected_nodes()
                .enumerate()
                .filter_map(|(order, id)| {
                    if !focus_candidate(doc, id) {
                        return None;
                    }
                    let tab = doc
                        .attr(id, "tabindex")
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(0);
                    if tab < 0 || hidden(doc, id) {
                        return None;
                    }
                    Some((if tab > 0 { tab } else { i32::MAX }, order, id))
                })
                .collect::<Vec<_>>()
        };
        candidates.sort_by_key(|(tab, order, _)| (*tab, *order));
        if candidates.is_empty() {
            self.focus(vm, None)?;
            return Ok(false);
        }
        let active = self.focused();
        let current = candidates.iter().position(|(_, _, id)| Some(*id) == active);
        let next = match (current, reverse) {
            (Some(0), true) => candidates.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % candidates.len(),
            (None, true) => candidates.len() - 1,
            (None, false) => 0,
        };
        self.focus(vm, Some(candidates[next].2))
    }
    pub(super) fn choose_option(
        &self,
        vm: &mut Vm,
        select: NodeId,
        option: NodeId,
        toggle: bool,
        extend: bool,
    ) -> Result<bool, JsError> {
        self.charge(vm, 0)?;
        {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            if !html(doc, select, "select")
                || select_owner(doc, option) != Some(select)
                || disabled(doc, select)
                || disabled(doc, option)
            {
                return Ok(false);
            }
        }
        self.focus(vm, Some(select))?;
        let (list, old, multiple) = {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            if select_owner(doc, option) != Some(select)
                || disabled(doc, select)
                || disabled(doc, option)
            {
                return Ok(true);
            }
            (
                options(doc, select),
                selected(doc, &self.data.borrow(), select),
                attr(doc, select, "multiple"),
            )
        };
        let old_set = old.iter().copied().collect::<HashSet<_>>();
        let range = if multiple && extend {
            let anchor = self
                .data
                .borrow()
                .controls
                .get(&select)
                .map_or(option.0, |c| c.anchor);
            let a = list
                .iter()
                .position(|id| id.0 == anchor)
                .unwrap_or_else(|| list.iter().position(|id| old.contains(id)).unwrap_or(0));
            let b = list.iter().position(|id| *id == option).unwrap();
            Some((a.min(b), a.max(b)))
        } else {
            None
        };
        {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            let mut data = self.data.borrow_mut();
            for (index, &id) in list.iter().enumerate() {
                let selected = if !multiple || (!toggle && !extend) {
                    id == option
                } else if disabled(doc, id) {
                    old_set.contains(&id)
                } else if let Some((a, b)) = range {
                    (index >= a && index <= b) || (toggle && old_set.contains(&id))
                } else if multiple && toggle {
                    if id == option {
                        !old_set.contains(&id)
                    } else {
                        old_set.contains(&id)
                    }
                } else {
                    id == option
                };
                let c = data.controls.entry(id).or_default();
                c.selected = Some(selected);
                if id == option {
                    c.selected_dirty = true;
                }
            }
            if !extend {
                data.controls.entry(select).or_default().anchor = option.0;
            }
            data.select_none.remove(&select);
        }
        let new = selected(&self.dom.borrow().document, &self.data.borrow(), select);
        self.mark();
        if old != new {
            dispatch_checked(vm, &self.dom, Some(select), "input", true, false, &[])?;
            dispatch_checked(vm, &self.dom, Some(select), "change", true, false, &[])?;
        }
        Ok(true)
    }
    pub(super) fn take_submissions(&self) -> Vec<FormSubmission> {
        self.data.borrow_mut().submissions.drain(..).collect()
    }
    pub(super) fn has_submissions(&self) -> bool {
        !self.data.borrow().submissions.is_empty()
    }
    pub(super) fn snapshot(&self) -> HashMap<NodeId, ControlSnapshot> {
        snapshots(&self.dom.borrow().document, &self.data.borrow())
    }
}

fn attribute_changed(vm: &mut Vm, doc: &Document, data: &mut Data, id: NodeId, name: &str) {
    if !is_control(doc, id) {
        return;
    }
    if name == "type" && html(doc, id, "input") {
        if file_inputs::is_file(doc, id) {
            if let Some(control) = data.controls.get_mut(&id) {
                control.value = None;
            }
        } else {
            data.file_inputs.remove(vm, id);
        }
    }
    if name == "checked"
        && html(doc, id, "input")
        && !data.controls.get(&id).is_some_and(|c| c.checked_dirty)
    {
        data.controls.entry(id).or_default().checked = Some(attr(doc, id, "checked"));
        if attr(doc, id, "checked") && input_type(doc, id) == "radio" {
            for other in radio_group(doc, id) {
                if other != id {
                    data.controls.entry(other).or_default().checked = Some(false);
                }
            }
        }
    }
    if name == "selected"
        && html(doc, id, "option")
        && !data.controls.get(&id).is_some_and(|c| c.selected_dirty)
        && let Some(select) = select_owner(doc, id)
    {
        data.controls.entry(id).or_default().selected = Some(attr(doc, id, "selected"));
        data.select_none.remove(&select);
        if attr(doc, id, "selected") && !attr(doc, select, "multiple") {
            for other in options(doc, select) {
                if other != id {
                    data.controls.entry(other).or_default().selected = Some(false);
                }
            }
        }
    }
    if name == "type"
        && let Some(value) = data.controls.get(&id).and_then(|c| c.value.clone())
    {
        data.controls.entry(id).or_default().value = Some(sanitize(doc, id, value));
    }
}

fn preview(value: &JsString) -> String {
    String::from_utf16_lossy(&value.as_utf16()[..value.len().min(SNAPSHOT_UNITS)])
}
// One indexed walk keeps the VM-free initial rendering path linear in the DOM.
// Rendering previews are bounded independently from the actual DOM/value APIs.
type RadioWinners<'a> = HashMap<(Option<NodeId>, &'a str), (Option<NodeId>, Option<NodeId>)>;
struct FormIndex<'a> {
    owners: Vec<Option<NodeId>>,
    selects: HashMap<NodeId, Vec<NodeId>>,
    radios: RadioWinners<'a>,
    radio_required: HashSet<(Option<NodeId>, &'a str)>,
}
impl<'a> FormIndex<'a> {
    fn new(doc: &'a Document, data: &Data) -> Self {
        let count = doc.nodes.len();
        let mut nearest_form = vec![None; count];
        let mut nearest_select = vec![None; count];
        let mut in_option = vec![false; count];
        let mut ids = HashMap::new();
        for id in doc.connected_nodes() {
            if let Some(name) = doc.attr(id, "id") {
                ids.entry(name).or_insert(id);
            }
        }
        let mut result = Self {
            owners: vec![None; count],
            selects: HashMap::new(),
            radios: HashMap::new(),
            radio_required: HashSet::new(),
        };
        for id in doc.connected_nodes() {
            if let Some(parent) = doc.nodes[id.0].parent {
                nearest_form[id.0] = if html(doc, parent, "form") {
                    Some(parent)
                } else {
                    nearest_form[parent.0]
                };
                nearest_select[id.0] = if html(doc, parent, "select") {
                    Some(parent)
                } else {
                    nearest_select[parent.0]
                };
                in_option[id.0] = !html(doc, parent, "select")
                    && (html(doc, parent, "option") || in_option[parent.0]);
            }
            if associated(doc, id) {
                result.owners[id.0] = if let Some(name) = doc.attr(id, "form") {
                    if name.is_empty() {
                        None
                    } else {
                        ids.get(name).copied().filter(|&id| html(doc, id, "form"))
                    }
                } else {
                    nearest_form[id.0]
                };
            }
            if html(doc, id, "option")
                && !in_option[id.0]
                && let Some(select) = nearest_select[id.0]
            {
                result.selects.entry(select).or_default().push(id);
            }
            if html(doc, id, "input")
                && input_type(doc, id) == "radio"
                && let Some(name) = doc.attr(id, "name")
                && !name.is_empty()
            {
                if attr(doc, id, "required") {
                    result.radio_required.insert((result.owners[id.0], name));
                }
                let group = result
                    .radios
                    .entry((result.owners[id.0], name))
                    .or_default();
                match data.controls.get(&id).and_then(|control| control.checked) {
                    Some(true) => group.0 = Some(id),
                    Some(false) => {}
                    None if attr(doc, id, "checked") => group.1 = Some(id),
                    _ => {}
                }
            }
        }
        result
    }
    fn radio_missing(&self, doc: &Document, data: &Data, id: NodeId) -> bool {
        let name = doc.attr(id, "name").unwrap_or("");
        if name.is_empty() {
            return attr(doc, id, "required") && !checked(doc, data, id);
        }
        let key = (self.owners[id.0], name);
        self.radio_required.contains(&key)
            && self
                .radios
                .get(&key)
                .is_none_or(|(current, default)| current.or(*default).is_none())
    }
    fn radio_cache(&self, doc: &Document, data: &Data) -> HashMap<NodeId, bool> {
        doc.connected_nodes()
            .filter(|&id| html(doc, id, "input") && input_type(doc, id) == "radio")
            .map(|id| (id, !self.radio_missing(doc, data, id)))
            .collect()
    }
    fn picked(&self, doc: &Document, data: &Data, id: NodeId) -> Vec<NodeId> {
        let list = self.selects.get(&id).map(Vec::as_slice).unwrap_or_default();
        let mut result = list
            .iter()
            .copied()
            .filter(|id| {
                data.controls
                    .get(id)
                    .and_then(|c| c.selected)
                    .unwrap_or_else(|| attr(doc, *id, "selected"))
            })
            .collect::<Vec<_>>();
        if !attr(doc, id, "multiple") {
            if result.len() > 1 {
                result.drain(..result.len() - 1);
            }
            if result.is_empty()
                && !data.select_none.contains(&id)
                && doc
                    .attr(id, "size")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0)
                    <= 1
                && let Some(&first) = list.iter().find(|&&id| !disabled(doc, id))
            {
                result.push(first);
            }
        }
        result
    }
}
fn bounded_text(doc: &Document, id: NodeId, limit: usize, work: &mut usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    let mut pending = vec![id];
    'outer: while let Some(id) = pending.pop() {
        if *work == 0 {
            break;
        }
        *work -= 1;
        if html(doc, id, "script") {
            continue;
        }
        if let Some(value) = doc.nodes[id.0].kind.text_data() {
            for ch in value.chars() {
                if *work == 0 || units + ch.len_utf16() > limit {
                    break 'outer;
                }
                *work -= 1;
                units += ch.len_utf16();
                out.push(ch);
            }
        }
        pending.extend(doc.nodes[id.0].children.iter().rev().copied());
    }
    out
}
fn bounded_str(value: &str, limit: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= limit
        })
        .collect()
}
fn option_preview(doc: &Document, id: NodeId, work: &mut usize) -> String {
    doc.attr(id, "value")
        .map(|value| bounded_str(value, SNAPSHOT_UNITS))
        .unwrap_or_else(|| {
            bounded_text(doc, id, SNAPSHOT_UNITS, work)
                .split(['\t', '\n', '\x0c', '\r', ' '])
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
}
fn snapshots(doc: &Document, data: &Data) -> HashMap<NodeId, ControlSnapshot> {
    let index = FormIndex::new(doc, data);
    let mut out = HashMap::new();
    let mut remaining = 1024 * 1024usize;
    let mut work = 1024 * 1024usize;
    for id in doc.connected_nodes().filter(|&id| {
        is_control(doc, id) && matches!(doc.tag(id), "input" | "textarea" | "select" | "button")
    }) {
        let control = data.controls.get(&id);
        let list = index
            .selects
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let picked = if html(doc, id, "select") {
            index.picked(doc, data, id)
        } else {
            vec![]
        };
        let picked = picked.into_iter().collect::<HashSet<_>>();
        let value_limit = if data.focused == Some(id) {
            SNAPSHOT_UNITS
        } else {
            remaining.min(SNAPSHOT_UNITS)
        };
        let value = if value_limit == 0 {
            String::new()
        } else if let Some(value) = control.and_then(|c| c.value.as_ref()) {
            String::from_utf16_lossy(&value.as_utf16()[..value.len().min(value_limit)])
        } else if html(doc, id, "select") {
            list.iter()
                .find(|id| picked.contains(id))
                .map(|&id| option_preview(doc, id, &mut work))
                .unwrap_or_default()
        } else if html(doc, id, "textarea") {
            preview(&sanitize(
                doc,
                id,
                bounded_text(doc, id, value_limit, &mut work).into(),
            ))
        } else if file_inputs::is_file(doc, id) {
            bounded_str(&data.file_inputs.label(id), value_limit)
        } else {
            let value = bounded_str(
                doc.attr(id, "value").unwrap_or_else(|| {
                    if matches!(input_type(doc, id).as_str(), "checkbox" | "radio") {
                        "on"
                    } else {
                        ""
                    }
                }),
                value_limit,
            );
            if html(doc, id, "input") {
                preview(&sanitize(doc, id, value.into()))
            } else {
                value
            }
        };
        let mut choices = Vec::new();
        for &node in list.iter().take(4096) {
            if remaining == 0 || work == 0 {
                break;
            }
            let label = doc
                .attr(node, "label")
                .map(|label| bounded_str(label, 1024))
                .unwrap_or_else(|| bounded_text(doc, node, 1024, &mut work));
            let value = option_preview(doc, node, &mut work);
            if value.len() + label.len() > remaining {
                break;
            }
            remaining -= value.len() + label.len();
            choices.push(OptionSnapshot {
                node,
                label,
                value,
                selected: picked.contains(&node),
                disabled: disabled(doc, node),
            });
        }
        let length = value.encode_utf16().count();
        let value = if value.len() <= remaining || data.focused == Some(id) {
            remaining = remaining.saturating_sub(value.len());
            value
        } else {
            remaining = 0;
            String::new()
        };
        let checked = if input_type(doc, id) == "radio" {
            let name = doc.attr(id, "name").unwrap_or("");
            if name.is_empty() {
                control
                    .and_then(|c| c.checked)
                    .unwrap_or_else(|| attr(doc, id, "checked"))
            } else {
                index
                    .radios
                    .get(&(index.owners[id.0], name))
                    .and_then(|(current, default)| current.or(*default))
                    == Some(id)
            }
        } else {
            control
                .and_then(|c| c.checked)
                .unwrap_or_else(|| attr(doc, id, "checked"))
        };
        out.insert(
            id,
            ControlSnapshot {
                kind: kind(doc, id),
                value,
                checked,
                selected: false,
                disabled: disabled(doc, id),
                read_only: attr(doc, id, "readonly"),
                placeholder_shown: placeholder_shown(doc, id, control),
                focused: data.focused == Some(id),
                selection_start: control.map_or(length, |c| c.start),
                selection_end: control.map_or(length, |c| c.end),
                options: choices,
            },
        );
    }
    out
}
pub(crate) fn initial_snapshots(document: &Document) -> HashMap<NodeId, ControlSnapshot> {
    let data = Data {
        controls: HashMap::new(),
        select_none: HashSet::new(),
        focused: None,
        focus_value: None,
        user_edited: false,
        activating: HashSet::new(),
        submitting: HashSet::new(),
        constructing_entry_list: HashSet::new(),
        resetting: HashSet::new(),
        submissions: VecDeque::new(),
        exception_constructor: None,
        form_data: None,
        file_inputs: FileInputs::default(),
    };
    snapshots(document, &data)
}

impl FormHost {
    fn install_accessors(&self, vm: &mut Vm, prototype: Value) -> Result<(), JsError> {
        for property in [
            "value",
            "defaultValue",
            "checked",
            "defaultChecked",
            "selected",
            "defaultSelected",
            "indeterminate",
            "name",
            "type",
            "disabled",
            "readOnly",
            "multiple",
            "required",
            "noValidate",
            "formNoValidate",
            "action",
            "method",
            "enctype",
            "formAction",
            "formMethod",
            "formEnctype",
            "target",
            "formTarget",
            "form",
            "elements",
            "options",
            "selectedOptions",
            "selectedIndex",
            "length",
            "selectionStart",
            "selectionEnd",
            "selectionDirection",
            "maxLength",
            "minLength",
        ] {
            let host = self.clone();
            let get = vm.new_native_function(property, move |vm, this, _| {
                host.get_property(vm, this, property)
            });
            let host = self.clone();
            let set = vm.new_native_function(property, move |vm, this, args| {
                host.set_property(vm, this, property, arg(args, 0))
            });
            let readonly = matches!(
                property,
                "form" | "elements" | "options" | "selectedOptions" | "length"
            );
            vm.define_accessor(
                prototype.clone(),
                property,
                Some(get),
                (!readonly).then_some(set),
            )?;
        }
        Ok(())
    }
    fn get_property(&self, vm: &mut Vm, this: Value, property: &str) -> Result<Value, JsError> {
        let id = node(&self.dom, &this)?;
        self.charge(vm, 0)?;
        if matches!(
            property,
            "value" | "selectionStart" | "selectionEnd" | "selectionDirection"
        ) {
            let work = {
                let dom = self.dom.borrow();
                value_read_work(&dom.document, &self.data.borrow(), id)
            };
            vm.charge_work(work)?;
        }
        if property == "form" {
            let owner = {
                let dom = self.dom.borrow();
                owner(&dom.document, id)
            };
            return wrap(vm, &self.dom, owner);
        }
        if matches!(
            property,
            "form" | "elements" | "options" | "selectedOptions"
        ) {
            let ids = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                match property {
                    "form" => return wrap(vm, &self.dom, owner(doc, id)),
                    "elements" if html(doc, id, "form") => {
                        owned_controls(doc, &self.data.borrow(), id)
                    }
                    "elements" if html(doc, id, "fieldset") => descendants(doc, id)
                        .filter(|&n| associated(doc, n))
                        .collect(),
                    "options" if html(doc, id, "select") => options(doc, id),
                    "selectedOptions" if html(doc, id, "select") => {
                        selected(doc, &self.data.borrow(), id)
                    }
                    _ => return Ok(Value::Undefined),
                }
            };
            let result = wrap_array(vm, &self.dom, ids.clone())?;
            let dom = self.dom.clone();
            let item_ids = ids.clone();
            method(vm, result.clone(), "item", move |vm, _, args| {
                let index = vm.to_number(&arg(args, 0))? as usize;
                wrap(vm, &dom, item_ids.get(index).copied())
            })?;
            let dom = self.dom.clone();
            method(vm, result.clone(), "namedItem", move |vm, _, args| {
                let key = text(vm, arg(args, 0))?;
                let id = {
                    let state = dom.borrow();
                    ids.iter().copied().find(|&id| {
                        !key.is_empty()
                            && (state.document.attr(id, "id") == Some(&key)
                                || state.document.attr(id, "name") == Some(&key))
                    })
                };
                wrap(vm, &dom, id)
            })?;
            return Ok(result);
        }
        let dom = self.dom.borrow();
        let doc = &dom.document;
        if !is_control(doc, id) && !html(doc, id, "form") {
            return Ok(Value::Undefined);
        }
        let data = self.data.borrow();
        let value = match property {
            "value" => Value::String(current_value(doc, &data, id)),
            "defaultValue" if html(doc, id, "textarea") => Value::from(doc.text(id)),
            "defaultValue" => Value::from(doc.attr(id, "value").unwrap_or("")),
            "checked" => checked(doc, &data, id).into(),
            "defaultChecked" => attr(doc, id, "checked").into(),
            "selected" => select_owner(doc, id)
                .map_or_else(
                    || {
                        data.controls
                            .get(&id)
                            .and_then(|c| c.selected)
                            .unwrap_or_else(|| attr(doc, id, "selected"))
                    },
                    |select| selected(doc, &data, select).contains(&id),
                )
                .into(),
            "defaultSelected" => attr(doc, id, "selected").into(),
            "indeterminate" => data
                .controls
                .get(&id)
                .is_some_and(|c| c.indeterminate)
                .into(),
            "name" => doc.attr(id, "name").unwrap_or("").into(),
            "type" => kind(doc, id).into(),
            "disabled" => attr(doc, id, "disabled").into(),
            "readOnly" => attr(doc, id, "readonly").into(),
            "multiple" => attr(doc, id, "multiple").into(),
            "required" => attr(doc, id, "required").into(),
            "noValidate" => attr(doc, id, "novalidate").into(),
            "formNoValidate" => attr(doc, id, "formnovalidate").into(),
            "action" | "formAction" => {
                let key = if property == "action" {
                    "action"
                } else {
                    "formaction"
                };
                let reference = doc.attr(id, key).unwrap_or("");
                if reference.is_empty() {
                    dom.url.as_str().into()
                } else {
                    document_base(&dom).join(reference).map_or_else(
                        |_| Value::from(reference),
                        |url| Value::from(url.to_string()),
                    )
                }
            }
            "method" | "formMethod" => {
                let key = if property == "method" {
                    "method"
                } else {
                    "formmethod"
                };
                enum_value(doc.attr(id, key), &["get", "post", "dialog"], "get").into()
            }
            "enctype" | "formEnctype" => {
                let key = if property == "enctype" {
                    "enctype"
                } else {
                    "formenctype"
                };
                enum_value(
                    doc.attr(id, key),
                    &[
                        "multipart/form-data",
                        "text/plain",
                        "application/x-www-form-urlencoded",
                    ],
                    "application/x-www-form-urlencoded",
                )
                .into()
            }
            "target" => doc.attr(id, "target").unwrap_or("").into(),
            "formTarget" => doc.attr(id, "formtarget").unwrap_or("").into(),
            "selectedIndex" if html(doc, id, "select") => {
                let list = options(doc, id);
                let picked = selected(doc, &data, id);
                Value::Number(
                    list.iter()
                        .position(|id| picked.contains(id))
                        .map_or(-1., |i| i as f64),
                )
            }
            "length" if html(doc, id, "select") => Value::from(options(doc, id).len()),
            "length" if html(doc, id, "form") => Value::from(owned_controls(doc, &data, id).len()),
            "selectionStart" | "selectionEnd" if selection_supported(doc, id) => {
                let value = current_value(doc, &data, id);
                let c = data.controls.get(&id);
                Value::from(if property == "selectionStart" {
                    c.map_or(value.len(), |c| c.start.min(value.len()))
                } else {
                    c.map_or(value.len(), |c| c.end.min(value.len()))
                })
            }
            "selectionStart" | "selectionEnd" => Value::Null,
            "selectionDirection" if selection_supported(doc, id) => "none".into(),
            "selectionDirection" => Value::Null,
            "maxLength" | "minLength" => Value::Number(
                doc.attr(
                    id,
                    if property == "maxLength" {
                        "maxlength"
                    } else {
                        "minlength"
                    },
                )
                .and_then(|s| s.parse::<u32>().ok())
                .map_or(-1., |n| n as f64),
            ),
            _ => Value::Undefined,
        };
        if let Value::String(s) = &value {
            vm.charge_work(s.len())?;
            if s.len() > VALUE_UNITS {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Form value read exceeds alpha length budget",
                ));
            }
        }
        Ok(value)
    }
    fn set_property(
        &self,
        vm: &mut Vm,
        this: Value,
        property: &str,
        value: Value,
    ) -> Result<Value, JsError> {
        let id = node(&self.dom, &this)?;
        self.charge(vm, 0)?;
        let applicable = {
            let dom = self.dom.borrow();
            is_control(&dom.document, id) || html(&dom.document, id, "form")
        };
        if !applicable {
            vm.define_data_property(this, property, value, true, true, true)?;
            return Ok(Value::Undefined);
        }
        match property {
            "value" => {
                let value = if value == Value::Null
                    && (self.dom.borrow().document.tag(id) == "input"
                        || self.dom.borrow().document.tag(id) == "textarea")
                {
                    JsString::default()
                } else {
                    vm.to_string(&value)?
                };
                self.set_value(vm, id, value)?;
            }
            "defaultValue" => {
                let value = vm.to_string(&value)?;
                vm.charge_work(value.len())?;
                let string = value.to_string_lossy();
                if self.dom.borrow().document.tag(id) == "textarea" {
                    set_text(vm, &self.dom, id, string)?;
                } else {
                    set_attribute(vm, &self.dom, id, "value", Some(string))?;
                }
            }
            "checked" => {
                let mut data = self.data.borrow_mut();
                set_checked(
                    &self.dom.borrow().document,
                    &mut data,
                    id,
                    value.is_truthy(),
                );
                drop(data);
                self.mark();
            }
            "selected" => {
                {
                    let dom = self.dom.borrow();
                    let doc = &dom.document;
                    let mut data = self.data.borrow_mut();
                    let selected_value = value.is_truthy();
                    if let Some(select) = select_owner(doc, id) {
                        data.select_none.remove(&select);
                        if selected_value && !attr(doc, select, "multiple") {
                            for other in options(doc, select) {
                                if other != id {
                                    data.controls.entry(other).or_default().selected = Some(false);
                                }
                            }
                        }
                    }
                    data.controls.entry(id).or_default().selected = Some(selected_value);
                    data.controls.entry(id).or_default().selected_dirty = true;
                }
                self.mark();
            }
            "indeterminate" => {
                self.data
                    .borrow_mut()
                    .controls
                    .entry(id)
                    .or_default()
                    .indeterminate = value.is_truthy();
                self.mark();
            }
            "disabled" | "readOnly" | "multiple" | "required" | "noValidate" | "formNoValidate"
            | "defaultChecked" | "defaultSelected" => {
                let name = match property {
                    "readOnly" => "readonly",
                    "noValidate" => "novalidate",
                    "formNoValidate" => "formnovalidate",
                    "defaultChecked" => "checked",
                    "defaultSelected" => "selected",
                    other => other,
                };
                set_attribute(vm, &self.dom, id, name, value.is_truthy().then(String::new))?;
            }
            "selectedIndex" => {
                let number = vm.to_number(&value)?;
                let list = options(&self.dom.borrow().document, id);
                let mut data = self.data.borrow_mut();
                let index = if number.is_finite() {
                    number.trunc() as i64
                } else {
                    0
                };
                let option_count = list.len();
                for (i, option) in list.into_iter().enumerate() {
                    data.controls.entry(option).or_default().selected = Some(i as i64 == index);
                    if i as i64 == index {
                        data.controls.entry(option).or_default().selected_dirty = true;
                    }
                }
                if index < 0 || index as usize >= option_count {
                    data.select_none.insert(id);
                } else {
                    data.select_none.remove(&id);
                }
                drop(data);
                self.mark();
            }
            "selectionStart" | "selectionEnd" | "selectionDirection" => {
                let supported = selection_supported(&self.dom.borrow().document, id);
                if !supported {
                    return Err(self.exception(
                        vm,
                        "InvalidStateError",
                        "This control has no text selection",
                    ));
                }
                let number = if property == "selectionDirection" {
                    0
                } else {
                    vm.to_number(&value)?.max(0.).min(u32::MAX as f64) as usize
                };
                let length =
                    current_value(&self.dom.borrow().document, &self.data.borrow(), id).len();
                let mut data = self.data.borrow_mut();
                let c = data.controls.entry(id).or_default();
                if property == "selectionStart" {
                    c.start = number.min(length);
                    c.end = c.end.max(c.start).min(length);
                } else if property == "selectionEnd" {
                    c.end = number.min(length);
                    c.start = c.start.min(c.end);
                }
                c.anchor = c.start;
                drop(data);
                self.mark();
            }
            "maxLength" | "minLength" => {
                let number = vm.to_number(&value)?;
                if !number.is_finite() || number < 0. {
                    return Err(self.exception(vm, "IndexSizeError", "Length must be nonnegative"));
                }
                set_attribute(
                    vm,
                    &self.dom,
                    id,
                    if property == "maxLength" {
                        "maxlength"
                    } else {
                        "minlength"
                    },
                    Some((number.trunc() as u32).to_string()),
                )?;
            }
            _ => {
                let name = match property {
                    "formAction" => "formaction",
                    "formMethod" => "formmethod",
                    "formEnctype" => "formenctype",
                    "formTarget" => "formtarget",
                    other => other,
                };
                reflect(vm, &self.dom, id, name, value)?;
            }
        }
        Ok(Value::Undefined)
    }
    fn install_methods(
        &self,
        vm: &mut Vm,
        prototype: Value,
        document: Value,
    ) -> Result<(), JsError> {
        for name in [
            "submit",
            "requestSubmit",
            "reset",
            "checkValidity",
            "reportValidity",
            "focus",
            "blur",
            "click",
            "select",
            "setSelectionRange",
        ] {
            let host = self.clone();
            method(vm, prototype.clone(), name, move |vm, this, args| {
                let id = node(&host.dom, &this)?;
                match name {
                    "submit" | "requestSubmit" => {
                        if !html(&host.dom.borrow().document, id, "form") {
                            return Err(JsError::type_error("Expected an HTML form"));
                        }
                        let submitter = if name == "requestSubmit" && !arg(args, 0).is_nullish() {
                            Some(element(&host.dom, &arg(args, 0))?)
                        } else {
                            None
                        };
                        if let Some(submitter) = submitter {
                            let (is_submit, same_owner) = {
                                let dom = host.dom.borrow();
                                let doc = &dom.document;
                                (
                                    is_submit_button(doc, submitter),
                                    owner(doc, submitter) == Some(id),
                                )
                            };
                            if !is_submit {
                                return Err(JsError::type_error(
                                    "requestSubmit requires a submit button",
                                ));
                            }
                            if !same_owner {
                                return Err(host.exception(
                                    vm,
                                    "NotFoundError",
                                    "Submitter is not owned by this form",
                                ));
                            }
                        }
                        host.submit(vm, id, submitter, name == "submit")?;
                        Ok(Value::Undefined)
                    }
                    "reset" => {
                        if !html(&host.dom.borrow().document, id, "form") {
                            return Err(JsError::type_error("Expected an HTML form"));
                        }
                        host.reset(vm, id)?;
                        Ok(Value::Undefined)
                    }
                    "checkValidity" | "reportValidity" => {
                        Ok(host.validate(vm, id, name == "reportValidity")?.into())
                    }
                    "focus" => {
                        host.focus(vm, Some(id))?;
                        Ok(Value::Undefined)
                    }
                    "blur" => {
                        if host.focused() == Some(id) {
                            host.focus(vm, None)?;
                        }
                        Ok(Value::Undefined)
                    }
                    "click" => {
                        host.activate(vm, id)?;
                        Ok(Value::Undefined)
                    }
                    "select" => {
                        host.select_range(vm, id, 0, usize::MAX)?;
                        dispatch_checked(vm, &host.dom, Some(id), "select", true, false, &[])?;
                        Ok(Value::Undefined)
                    }
                    _ => {
                        if args.len() < 2 {
                            return Err(JsError::type_error(
                                "setSelectionRange requires start and end",
                            ));
                        }
                        let start = vm.to_number(&args[0])?.max(0.) as usize;
                        let end = vm.to_number(&args[1])?.max(0.) as usize;
                        host.select_range(vm, id, start, end)?;
                        Ok(Value::Undefined)
                    }
                }
            })?;
        }
        let host = self.clone();
        getter(vm, document.clone(), "activeElement", move |vm, _, _| {
            let id = host
                .focused()
                .or_else(|| document_part(&host.dom.borrow().document, "body"));
            wrap(vm, &host.dom, id)
        })?;
        let host = self.clone();
        getter(vm, document, "forms", move |vm, _, _| {
            host.charge(vm, 0)?;
            let ids = {
                let dom = host.dom.borrow();
                dom.document.html_elements("form").collect::<Vec<_>>()
            };
            wrap_array(vm, &host.dom, ids)
        })?;
        Ok(())
    }
    fn select_range(
        &self,
        vm: &mut Vm,
        id: NodeId,
        start: usize,
        end: usize,
    ) -> Result<(), JsError> {
        self.charge(vm, 0)?;
        if !selection_supported(&self.dom.borrow().document, id) {
            return Err(self.exception(
                vm,
                "InvalidStateError",
                "This control has no text selection",
            ));
        }
        let length = current_value(&self.dom.borrow().document, &self.data.borrow(), id).len();
        let end = end.min(length);
        let start = start.min(end);
        let mut data = self.data.borrow_mut();
        let c = data.controls.entry(id).or_default();
        c.start = start;
        c.end = end;
        c.anchor = start;
        drop(data);
        self.mark();
        Ok(())
    }
    pub(super) fn focus(&self, vm: &mut Vm, target: Option<NodeId>) -> Result<bool, JsError> {
        self.charge(vm, 0)?;
        if let Some(id) = target {
            let work = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                if is_labelable(doc, id) {
                    value_read_work(doc, &self.data.borrow(), id)
                } else {
                    0
                }
            };
            vm.charge_work(work)?;
        }
        let target = target.filter(|&id| {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            focus_candidate(doc, id) && connected(doc, id) && !hidden(doc, id)
        });
        let old = self.data.borrow().focused;
        if old == target {
            return Ok(target.is_some());
        }
        let changed = {
            let data = self.data.borrow();
            old.is_some_and(|id| {
                data.user_edited
                    && data.focus_value.as_ref()
                        != Some(&current_value(&self.dom.borrow().document, &data, id))
            })
        };
        {
            let mut data = self.data.borrow_mut();
            data.focused = None;
            data.focus_value = None;
            data.user_edited = false;
        }
        if let Some(old) = old {
            self.mark();
            if changed {
                dispatch_checked(vm, &self.dom, Some(old), "change", true, false, &[])?;
            }
            dispatch_checked(vm, &self.dom, Some(old), "blur", false, false, &[])?;
            dispatch_checked(vm, &self.dom, Some(old), "focusout", true, false, &[])?;
        }
        // A blur handler may have synchronously focused another element.
        if self.data.borrow().focused.is_some() {
            return Ok(true);
        }
        if let Some(id) = target {
            let value = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                is_labelable(doc, id).then(|| current_value(doc, &self.data.borrow(), id))
            };
            {
                let mut data = self.data.borrow_mut();
                data.focused = Some(id);
                if let Some(value) = &value {
                    let c = data.controls.entry(id).or_default();
                    if c.value.is_none() {
                        c.start = value.len();
                        c.end = value.len();
                        c.anchor = value.len();
                    }
                }
                data.focus_value = value;
            }
            self.mark();
            dispatch_checked(vm, &self.dom, Some(id), "focus", false, false, &[])?;
            dispatch_checked(vm, &self.dom, Some(id), "focusin", true, false, &[])?;
        }
        Ok(target.is_some())
    }
    pub(super) fn selected_text(&self) -> Option<String> {
        let id = self.focused()?;
        let dom = self.dom.borrow();
        let doc = &dom.document;
        if !selection_supported(doc, id) || input_type(doc, id) == "password" {
            return None;
        }
        let data = self.data.borrow();
        let value = current_value(doc, &data, id);
        let c = data.controls.get(&id)?;
        let start = c.start.min(value.len());
        let end = c.end.min(value.len());
        if start >= end {
            return None;
        }
        Some(String::from_utf16_lossy(
            &value.as_utf16()[start..end.min(start + 65536)],
        ))
    }
    pub(super) fn cut_selection(&self, vm: &mut Vm) -> Result<bool, JsError> {
        self.edit_selection(vm, JsString::default(), "deleteByCut")
    }
    pub(super) fn insert_text(&self, vm: &mut Vm, value: &str) -> Result<bool, JsError> {
        self.edit_selection(vm, value.into(), "insertText")
    }
    fn edit_selection(
        &self,
        vm: &mut Vm,
        insert: JsString,
        input_type: &str,
    ) -> Result<bool, JsError> {
        let Some(id) = self.focused() else {
            return Ok(false);
        };
        self.charge(vm, insert.len())?;
        let (editable, multiline, max_length) = {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            (
                text_editable(doc, id) && !disabled(doc, id) && !attr(doc, id, "readonly"),
                html(doc, id, "textarea"),
                doc.attr(id, "maxlength")
                    .and_then(|s| s.parse::<usize>().ok()),
            )
        };
        if !editable {
            return Ok(false);
        }
        let insert = JsString::from_utf16(newline_units(insert.as_utf16(), !multiline));
        let (old, start, end) = {
            let data = self.data.borrow();
            let value = current_value(&self.dom.borrow().document, &data, id);
            let c = data.controls.get(&id);
            let start = c.map_or(value.len(), |c| c.start.min(value.len()));
            let end = c.map_or(value.len(), |c| c.end.min(value.len())).max(start);
            (value, start, end)
        };
        let limit = max_length.unwrap_or(VALUE_UNITS).min(VALUE_UNITS);
        let room = limit.saturating_sub(old.len() - (end - start));
        let take = insert.len().min(room);
        let mut take = take;
        if take > 0
            && take < insert.len()
            && (0xd800..=0xdbff).contains(&insert.as_utf16()[take - 1])
            && (0xdc00..=0xdfff).contains(&insert.as_utf16()[take])
        {
            take -= 1;
        }
        let inserted = JsString::from_utf16(insert.as_utf16()[..take].to_vec());
        if !dispatch_checked(
            vm,
            &self.dom,
            Some(id),
            "beforeinput",
            true,
            true,
            &[
                (
                    "data",
                    if input_type.starts_with("delete") {
                        Value::Null
                    } else {
                        inserted.clone().into()
                    },
                ),
                ("inputType", input_type.into()),
            ],
        )? {
            return Ok(true);
        }
        // beforeinput handlers may edit the same control: use its current range.
        let (old, start, end) = {
            let data = self.data.borrow();
            let value = current_value(&self.dom.borrow().document, &data, id);
            let c = data.controls.get(&id);
            let start = c.map_or(start, |c| c.start.min(value.len()));
            let end = c.map_or(end, |c| c.end.min(value.len())).max(start);
            (value, start, end)
        };
        if old.len() - (end - start) + inserted.len() > VALUE_UNITS {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Edited control exceeds value budget",
            ));
        }
        let mut units = Vec::with_capacity(old.len() - (end - start) + inserted.len());
        units.extend_from_slice(&old.as_utf16()[..start]);
        units.extend_from_slice(inserted.as_utf16());
        units.extend_from_slice(&old.as_utf16()[end..]);
        let value = JsString::from_utf16(units);
        if value == old {
            return Ok(true);
        }
        self.set_value(vm, id, value)?;
        {
            let mut data = self.data.borrow_mut();
            let c = data.controls.entry(id).or_default();
            c.start = start + inserted.len();
            c.end = c.start;
            c.anchor = c.start;
            data.user_edited = true;
        }
        dispatch_checked(
            vm,
            &self.dom,
            Some(id),
            "input",
            true,
            false,
            &[
                (
                    "data",
                    if input_type.starts_with("delete") {
                        Value::Null
                    } else {
                        inserted.into()
                    },
                ),
                ("inputType", input_type.into()),
            ],
        )?;
        self.mark();
        Ok(true)
    }
    pub(super) fn keydown(
        &self,
        vm: &mut Vm,
        key: FormKey,
        shift: bool,
        control: bool,
    ) -> Result<bool, JsError> {
        let Some(id) = self.focused() else {
            return if key == FormKey::Tab {
                self.focus_next(vm, shift)
            } else {
                Ok(false)
            };
        };
        self.charge(vm, 0)?;
        let key_name = match key {
            FormKey::ArrowLeft => "ArrowLeft",
            FormKey::ArrowRight => "ArrowRight",
            FormKey::ArrowUp => "ArrowUp",
            FormKey::ArrowDown => "ArrowDown",
            FormKey::Home => "Home",
            FormKey::End => "End",
            FormKey::Backspace => "Backspace",
            FormKey::Delete => "Delete",
            FormKey::Enter => "Enter",
            FormKey::Tab => "Tab",
            FormKey::Space => " ",
            FormKey::SelectAll => "a",
        };
        if !dispatch_checked(
            vm,
            &self.dom,
            Some(id),
            "keydown",
            true,
            true,
            &[
                ("key", key_name.into()),
                ("shiftKey", shift.into()),
                ("ctrlKey", control.into()),
            ],
        )? {
            return Ok(true);
        }
        if key == FormKey::Tab {
            return self.focus_next(vm, shift);
        }
        if key == FormKey::Enter
            && !shift
            && !control
            && is_hyperlink(&self.dom.borrow().document, id)
        {
            // Click listeners can cancel the default, change href, or remove the link.
            if self.activate(vm, id)? {
                let url = {
                    let dom = self.dom.borrow();
                    let doc = &dom.document;
                    doc.attr(id, "href")
                        .filter(|_| connected(doc, id))
                        .and_then(|href| document_base(&dom).join(href).ok())
                };
                if let Some(url) = url {
                    self.dom
                        .borrow_mut()
                        .routing
                        .submit(ScriptNavigation::Navigate {
                            url,
                            mode: NavigationMode::Push,
                        })?;
                }
            }
            return Ok(true);
        }
        let ty = kind(&self.dom.borrow().document, id);
        if key == FormKey::Space
            && matches!(
                ty.as_str(),
                "checkbox" | "radio" | "submit" | "reset" | "button" | "file"
            )
        {
            self.activate(vm, id)?;
            return Ok(true);
        }
        if ty.starts_with("select-")
            && matches!(
                key,
                FormKey::ArrowUp | FormKey::ArrowDown | FormKey::Home | FormKey::End
            )
        {
            let (list, picked) = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                (
                    options(doc, id)
                        .into_iter()
                        .filter(|&n| !disabled(doc, n))
                        .collect::<Vec<_>>(),
                    selected(doc, &self.data.borrow(), id),
                )
            };
            if list.is_empty() {
                return Ok(true);
            }
            let index = list.iter().position(|n| picked.contains(n)).unwrap_or(0);
            let index = match key {
                FormKey::Home => 0,
                FormKey::End => list.len() - 1,
                FormKey::ArrowUp => index.saturating_sub(1),
                _ => (index + 1).min(list.len() - 1),
            };
            {
                let mut data = self.data.borrow_mut();
                for &n in &list {
                    data.controls.entry(n).or_default().selected = Some(n == list[index]);
                }
                data.select_none.remove(&id);
            }
            self.mark();
            dispatch_checked(vm, &self.dom, Some(id), "input", true, false, &[])?;
            dispatch_checked(vm, &self.dom, Some(id), "change", true, false, &[])?;
            return Ok(true);
        }
        if key == FormKey::Enter {
            if html(&self.dom.borrow().document, id, "textarea") && !control {
                return self.edit_selection(vm, "\n".into(), "insertLineBreak");
            }
            if matches!(ty.as_str(), "submit" | "reset" | "button" | "file") {
                self.activate(vm, id)?;
                return Ok(true);
            }
            let form = {
                let dom = self.dom.borrow();
                owner(&dom.document, id)
            };
            if let Some(form) = form {
                let button = {
                    let dom = self.dom.borrow();
                    let doc = &dom.document;
                    doc.connected_nodes()
                        .find(|&n| is_submit_button(doc, n) && owner(doc, n) == Some(form))
                };
                if let Some(button) = button {
                    self.activate(vm, button)?;
                } else {
                    let blockers = {
                        let dom = self.dom.borrow();
                        let doc = &dom.document;
                        doc.connected_nodes()
                            .filter(|&n| {
                                html(doc, n, "input")
                                    && text_editable(doc, n)
                                    && owner(doc, n) == Some(form)
                            })
                            .count()
                    };
                    if blockers <= 1 {
                        self.submit(vm, form, None, false)?;
                    }
                }
                return Ok(true);
            }
        }
        if !text_editable(&self.dom.borrow().document, id) {
            return Ok(false);
        }
        if key == FormKey::Space && !control {
            return self.insert_text(vm, " ");
        }
        let value = current_value(&self.dom.borrow().document, &self.data.borrow(), id);
        let (start, end, anchor) = {
            let data = self.data.borrow();
            let c = data.controls.get(&id);
            (
                c.map_or(value.len(), |c| c.start.min(value.len())),
                c.map_or(value.len(), |c| c.end.min(value.len())),
                c.map_or(value.len(), |c| c.anchor.min(value.len())),
            )
        };
        let previous = |pos: usize| {
            let mut p = pos.saturating_sub(1);
            if p > 0
                && (0xdc00..=0xdfff).contains(&value.as_utf16()[p])
                && (0xd800..=0xdbff).contains(&value.as_utf16()[p - 1])
            {
                p -= 1;
            }
            p
        };
        let next = |pos: usize| {
            let mut p = (pos + 1).min(value.len());
            if p < value.len()
                && p > 0
                && (0xd800..=0xdbff).contains(&value.as_utf16()[p - 1])
                && (0xdc00..=0xdfff).contains(&value.as_utf16()[p])
            {
                p += 1;
            }
            p
        };
        match key {
            FormKey::SelectAll => self.select_range(vm, id, 0, usize::MAX)?,
            FormKey::ArrowLeft | FormKey::ArrowRight | FormKey::Home | FormKey::End => {
                let position = if key == FormKey::Home {
                    0
                } else if key == FormKey::End {
                    value.len()
                } else if key == FormKey::ArrowLeft {
                    if !shift && start != end {
                        start
                    } else {
                        previous(if shift && anchor == end { start } else { end })
                    }
                } else if !shift && start != end {
                    end
                } else {
                    next(if shift && anchor == end { start } else { end })
                };
                let mut data = self.data.borrow_mut();
                let c = data.controls.entry(id).or_default();
                if shift {
                    c.start = anchor.min(position);
                    c.end = anchor.max(position);
                    c.anchor = anchor;
                } else {
                    c.start = position;
                    c.end = position;
                    c.anchor = position;
                }
                drop(data);
                self.mark();
            }
            FormKey::Backspace | FormKey::Delete => {
                if start == end {
                    let mut data = self.data.borrow_mut();
                    let c = data.controls.entry(id).or_default();
                    if key == FormKey::Backspace {
                        c.start = previous(start);
                        c.end = end;
                    } else {
                        c.start = start;
                        c.end = next(end);
                    }
                }
                return self.edit_selection(
                    vm,
                    JsString::default(),
                    if key == FormKey::Backspace {
                        "deleteContentBackward"
                    } else {
                        "deleteContentForward"
                    },
                );
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    pub(super) fn activate(&self, vm: &mut Vm, target: NodeId) -> Result<bool, JsError> {
        self.charge(vm, 0)?;
        let control = {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            let mut current = Some(target);
            let mut found = None;
            while let Some(id) = current {
                if is_control(doc, id)
                    && matches!(doc.tag(id), "input" | "button" | "select" | "textarea")
                {
                    found = Some(id);
                    break;
                }
                current = doc.nodes[id.0].parent;
            }
            found
        };
        if control.is_some_and(|id| disabled(&self.dom.borrow().document, id)) {
            return Ok(false);
        }
        if !self.data.borrow_mut().activating.insert(target) {
            return Ok(false);
        }
        let result = (|| {
            let toggled = control.filter(|&id| {
                html(&self.dom.borrow().document, id, "input")
                    && matches!(
                        input_type(&self.dom.borrow().document, id).as_str(),
                        "checkbox" | "radio"
                    )
            });
            let mut rollback = Vec::new();
            let mut changed = false;
            if let Some(id) = toggled {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                let mut data = self.data.borrow_mut();
                let old = checked(doc, &data, id);
                let radio = input_type(doc, id) == "radio";
                for other in if radio {
                    radio_group(doc, id)
                } else {
                    vec![id]
                } {
                    rollback.push((other, data.controls.get(&other).cloned()));
                }
                changed = !radio || !old;
                set_checked(doc, &mut data, id, if radio { true } else { !old });
                data.controls.entry(id).or_default().indeterminate = false;
            }
            self.mark();
            let outcome = dispatch_checked(vm, &self.dom, Some(target), "click", true, true, &[]);
            let allowed = outcome.as_ref().is_ok_and(|value| *value);
            if !allowed {
                let mut data = self.data.borrow_mut();
                for (id, old) in rollback {
                    if let Some(old) = old {
                        data.controls.insert(id, old);
                    } else {
                        data.controls.remove(&id);
                    }
                }
                drop(data);
                self.mark();
                return outcome;
            }
            vm.charge_work(1)?;
            if let Some(id) = toggled
                && changed
            {
                dispatch_checked(vm, &self.dom, Some(id), "input", true, false, &[])?;
                dispatch_checked(vm, &self.dom, Some(id), "change", true, false, &[])?;
            }
            if let Some(id) = control {
                if disabled(&self.dom.borrow().document, id) {
                    return Ok(true);
                }
                let action = kind(&self.dom.borrow().document, id);
                if action == "file" {
                    self.request_file_picker(vm, id, false)?;
                }
                let form = owner(&self.dom.borrow().document, id);
                if let Some(form) = form {
                    if action == "submit" || action == "image" {
                        self.submit(vm, form, Some(id), false)?;
                    } else if action == "reset" {
                        self.reset(vm, form)?;
                    }
                }
            } else {
                let label = {
                    let dom = self.dom.borrow();
                    let doc = &dom.document;
                    let mut current = Some(target);
                    let mut label = None;
                    while let Some(id) = current {
                        if html(doc, id, "a") && attr(doc, id, "href") {
                            break;
                        }
                        if html(doc, id, "label") {
                            label = Some(id);
                            break;
                        }
                        current = doc.nodes[id.0].parent;
                    }
                    label
                };
                if let Some(label) = label {
                    let associated = {
                        let dom = self.dom.borrow();
                        let doc = &dom.document;
                        if let Some(name) = doc.attr(label, "for") {
                            doc.connected_nodes()
                                .find(|&id| doc.attr(id, "id") == Some(name))
                                .filter(|&id| is_labelable(doc, id))
                        } else {
                            descendants(doc, label).find(|&id| is_labelable(doc, id))
                        }
                    };
                    if let Some(id) = associated {
                        self.focus(vm, Some(id))?;
                        self.activate(vm, id)?;
                    }
                }
            }
            Ok(true)
        })();
        self.data.borrow_mut().activating.remove(&target);
        result
    }
    fn validate(&self, vm: &mut Vm, node: NodeId, report: bool) -> Result<bool, JsError> {
        self.charge(vm, 0)?;
        let (controls, mut radio_valid) = {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            let data = self.data.borrow();
            let index = FormIndex::new(doc, &data);
            let controls = if html(doc, node, "form") {
                owned_controls(doc, &data, node)
            } else {
                vec![node]
            };
            (controls, index.radio_cache(doc, &data))
        };
        let mut invalid = Vec::new();
        for id in controls {
            vm.charge_work(1)?;
            let work = {
                let dom = self.dom.borrow();
                value_read_work(&dom.document, &self.data.borrow(), id)
            };
            vm.charge_work(work)?;
            let valid = {
                let dom = self.dom.borrow();
                valid_control(
                    &dom.document,
                    &self.data.borrow(),
                    id,
                    radio_valid.get(&id).copied(),
                )
            };
            if !valid {
                invalid.push(id);
                dispatch_checked(vm, &self.dom, Some(id), "invalid", false, true, &[])?;
                self.charge(vm, 0)?;
                radio_valid = {
                    let dom = self.dom.borrow();
                    let data = self.data.borrow();
                    FormIndex::new(&dom.document, &data).radio_cache(&dom.document, &data)
                };
            }
        }
        if report && let Some(&id) = invalid.first() {
            self.focus(vm, Some(id))?;
        }
        Ok(invalid.is_empty())
    }
    fn reset(&self, vm: &mut Vm, form: NodeId) -> Result<(), JsError> {
        self.charge(vm, 0)?;
        if !self.data.borrow_mut().resetting.insert(form) {
            return Ok(());
        }
        let result = (|| {
            if !dispatch_checked(vm, &self.dom, Some(form), "reset", true, true, &[])? {
                return Ok(());
            }
            vm.charge_work(1)?;
            let nodes = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                let index = FormIndex::new(doc, &self.data.borrow());
                let controls = owned_controls(doc, &self.data.borrow(), form);
                let mut nodes = controls.clone();
                for id in controls {
                    if let Some(options) = index.selects.get(&id) {
                        nodes.extend(options);
                    } else if html(doc, id, "select") {
                        nodes.extend(options(doc, id));
                    }
                }
                nodes
            };
            {
                let mut data = self.data.borrow_mut();
                for id in nodes {
                    data.file_inputs.clear(vm, id)?;
                    let indeterminate = data
                        .controls
                        .get(&id)
                        .is_some_and(|control| control.indeterminate);
                    data.controls.remove(&id);
                    if indeterminate {
                        data.controls.entry(id).or_default().indeterminate = true;
                    }
                    data.select_none.remove(&id);
                }
            }
            let focused = self.focused();
            if let Some(focused) = focused {
                let value = {
                    let dom = self.dom.borrow();
                    if owner(&dom.document, focused) == Some(form) {
                        Some(current_value(&dom.document, &self.data.borrow(), focused))
                    } else {
                        None
                    }
                };
                if let Some(value) = value {
                    let mut data = self.data.borrow_mut();
                    data.user_edited = false;
                    data.focus_value = Some(value);
                }
            }
            self.mark();
            Ok(())
        })();
        self.data.borrow_mut().resetting.remove(&form);
        result
    }
    fn submit(
        &self,
        vm: &mut Vm,
        form: NodeId,
        submitter: Option<NodeId>,
        direct: bool,
    ) -> Result<(), JsError> {
        self.charge(vm, 0)?;
        if !connected(&self.dom.borrow().document, form)
            || self.data.borrow().constructing_entry_list.contains(&form)
        {
            return Ok(());
        }
        if !direct && !self.data.borrow_mut().submitting.insert(form) {
            return Ok(());
        }
        let result = (|| {
            let skip = {
                let dom = self.dom.borrow();
                let doc = &dom.document;
                direct
                    || attr(doc, form, "novalidate")
                    || submitter.is_some_and(|n| attr(doc, n, "formnovalidate"))
            };
            if !skip && !self.validate(vm, form, true)? {
                return Ok(());
            }
            if !direct {
                let button = wrap(vm, &self.dom, submitter)?;
                if !dispatch_checked(
                    vm,
                    &self.dom,
                    Some(form),
                    "submit",
                    true,
                    true,
                    &[("submitter", button)],
                )? {
                    return Ok(());
                }
            }
            if !connected(&self.dom.borrow().document, form) {
                return Ok(());
            }
            let Some(entries) = self.form_data(vm, form, submitter)? else {
                return Ok(());
            };
            if !connected(&self.dom.borrow().document, form) {
                return Ok(());
            }
            vm.charge_work(1)?;
            let submission = {
                let dom = self.dom.borrow();
                construct_submission(vm, &dom, form, submitter, &entries)?
            };
            if self.data.borrow().submissions.len() >= QUEUED_SUBMISSIONS {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Form navigation queue exceeds 8 submissions",
                ));
            }
            self.data.borrow_mut().submissions.push_back(submission);
            Ok(())
        })();
        if !direct {
            self.data.borrow_mut().submitting.remove(&form);
        }
        result
    }
}

fn is_submit_button(doc: &Document, id: NodeId) -> bool {
    doc.namespace(id) == Namespace::Html
        && matches!(doc.tag(id), "input" | "button")
        && matches!(kind(doc, id).as_str(), "submit" | "image")
}
fn is_labelable(doc: &Document, id: NodeId) -> bool {
    is_control(doc, id)
        && matches!(doc.tag(id), "input" | "textarea" | "select" | "button")
        && !(html(doc, id, "input") && input_type(doc, id) == "hidden")
}
fn valid_control(doc: &Document, data: &Data, id: NodeId, radio_valid: Option<bool>) -> bool {
    if !is_control(doc, id)
        || disabled(doc, id)
        || (attr(doc, id, "readonly") && text_editable(doc, id))
        || matches!(
            kind(doc, id).as_str(),
            "hidden"
                | "button"
                | "reset"
                | "submit"
                | "image"
                | "output"
                | "fieldset"
                | "option"
                | "optgroup"
        )
    {
        return true;
    }
    let ty = kind(doc, id);
    if html(doc, id, "select") {
        if !attr(doc, id, "required") {
            return true;
        }
        let picked = selected(doc, data, id);
        if picked.is_empty() {
            return false;
        }
        if !attr(doc, id, "multiple")
            && doc
                .attr(id, "size")
                .and_then(|size| size.parse::<usize>().ok())
                .unwrap_or(0)
                <= 1
        {
            let placeholder = options(doc, id).first().copied().filter(|&option| {
                doc.nodes[option.0].parent == Some(id) && option_value(doc, option).is_empty()
            });
            return !placeholder.is_some_and(|option| picked.len() == 1 && picked[0] == option);
        }
        return true;
    }
    let value = current_value(doc, data, id);
    if ty == "radio" {
        if let Some(valid) = radio_valid {
            return valid;
        }
        let group = radio_group(doc, id);
        return !group.iter().any(|&id| attr(doc, id, "required"))
            || group.iter().any(|id| {
                data.controls
                    .get(id)
                    .and_then(|c| c.checked)
                    .unwrap_or_else(|| attr(doc, *id, "checked"))
            });
    }
    if attr(doc, id, "required") {
        if ty == "checkbox" && !checked(doc, data, id) {
            return false;
        }
        if ty == "radio"
            && !radio_group(doc, id)
                .into_iter()
                .any(|id| checked(doc, data, id))
        {
            return false;
        }
        if ty.starts_with("select-") && selected(doc, data, id).is_empty() {
            return false;
        }
        if !matches!(ty.as_str(), "checkbox" | "radio") && value.is_empty() {
            return false;
        }
    }
    if value.is_empty() {
        return true;
    }
    let text = value.to_string_lossy();
    if ty == "email" {
        let values = if attr(doc, id, "multiple") {
            text.split(',').collect::<Vec<_>>()
        } else {
            vec![text.as_str()]
        };
        if values.iter().any(|value| {
            let Some((local, domain)) = value.trim().split_once('@') else {
                return true;
            };
            local.is_empty()
                || domain.is_empty()
                || domain.contains('@')
                || value.chars().any(|c| c.is_ascii_whitespace())
        }) {
            return false;
        }
    }
    if ty == "url" && Url::parse(&text).is_err() {
        return false;
    }
    if ty == "number"
        && let Ok(number) = text.parse::<f64>()
        && (doc
            .attr(id, "min")
            .and_then(|s| s.parse::<f64>().ok())
            .is_some_and(|min| number < min)
            || doc
                .attr(id, "max")
                .and_then(|s| s.parse::<f64>().ok())
                .is_some_and(|max| number > max))
    {
        return false;
    }
    true
}
fn collect_entries(
    vm: &mut Vm,
    doc: &Document,
    data: &Data,
    form: NodeId,
    submitter: Option<NodeId>,
    host: &FormDataHost,
) -> Result<EntryList, JsError> {
    let mut entries = EntryList::default();
    for id in owned_controls(doc, data, form) {
        if disabled(doc, id) {
            continue;
        }
        let mut parent = doc.nodes[id.0].parent;
        let mut in_datalist = false;
        while let Some(id) = parent {
            if html(doc, id, "datalist") {
                in_datalist = true;
                break;
            }
            parent = doc.nodes[id.0].parent;
        }
        if in_datalist {
            continue;
        }
        let ty = kind(doc, id);
        if matches!(doc.tag(id), "fieldset" | "output" | "object") {
            continue;
        }
        if matches!(ty.as_str(), "submit" | "reset" | "button" | "image") && submitter != Some(id) {
            continue;
        }
        let name = doc.attr(id, "name").unwrap_or("");
        vm.charge_work(1 + name.len().div_ceil(8))?;
        if ty == "image" {
            let prefix = if name.is_empty() {
                String::new()
            } else {
                format!("{name}.")
            };
            entries.text(&format!("{prefix}x"), "0".into())?;
            entries.text(&format!("{prefix}y"), "0".into())?;
            continue;
        }
        if name.is_empty() {
            continue;
        }
        if matches!(ty.as_str(), "checkbox" | "radio") && !checked(doc, data, id) {
            continue;
        }
        if html(doc, id, "select") {
            for option in selected(doc, data, id) {
                if !disabled(doc, option) {
                    let value = option_value(doc, option);
                    vm.charge_work(value.len().div_ceil(8) + name.len().div_ceil(8))?;
                    entries.text(name, value.to_string_lossy())?;
                }
            }
        } else if ty == "hidden" && name.eq_ignore_ascii_case("_charset_") {
            entries.text(name, "UTF-8".into())?;
        } else if ty == "file" {
            if let Some(files) = data
                .file_inputs
                .selected(id)
                .filter(|files| files.len() != 0)
            {
                for index in 0..files.len() {
                    let file = files
                        .at(vm, index as u32)?
                        .expect("selected FileList entry");
                    entries.file(name, file)?;
                }
            } else {
                entries.file(name, host.empty_file(vm)?)?;
            }
        } else {
            let value = current_value(doc, data, id);
            vm.charge_work(value.len().div_ceil(8))?;
            entries.text(name, value.to_string_lossy())?;
        }
        if let Some(dirname) = doc.attr(id, "dirname")
            && !dirname.is_empty()
            && matches!(
                ty.as_str(),
                "text" | "search" | "url" | "tel" | "email" | "textarea"
            )
        {
            let mut current = Some(id);
            let mut direction = "ltr";
            while let Some(node) = current {
                if let Some(dir) = doc.attr(node, "dir")
                    && matches!(dir, "ltr" | "rtl")
                {
                    direction = dir;
                    break;
                }
                current = doc.nodes[node.0].parent;
            }
            vm.charge_work(dirname.len().div_ceil(8))?;
            entries.text(dirname, direction.into())?;
        }
    }
    Ok(entries)
}

fn construct_submission(
    vm: &mut Vm,
    dom: &State,
    form: NodeId,
    submitter: Option<NodeId>,
    entries: &Value,
) -> Result<FormSubmission, JsError> {
    let doc = &dom.document;
    let attribute = |name: &str, override_name: &str| {
        submitter
            .and_then(|id| doc.attr(id, override_name))
            .or_else(|| doc.attr(form, name))
    };
    let method = match enum_value(
        attribute("method", "formmethod"),
        &["get", "post", "dialog"],
        "get",
    ) {
        "post" => "POST",
        "dialog" => return Err(unsupported("Dialog form submission is not implemented")),
        _ => "GET",
    };
    let action = attribute("action", "formaction").unwrap_or("");
    vm.charge_work(action.len())?;
    if action.len() > 65536 {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Form action exceeds 64 KiB",
        ));
    }
    let mut target = if action.is_empty() {
        dom.url.clone()
    } else {
        document_base(dom)
            .join(action)
            .map_err(|_| JsError::type_error("Invalid form action URL"))?
    };
    if !matches!(target.scheme(), "http" | "https") {
        return Err(unsupported(
            "Form navigation currently supports HTTP(S) actions",
        ));
    }
    let browsing_target = attribute("target", "formtarget").unwrap_or("");
    if !matches!(browsing_target, "" | "_self" | "_top" | "_parent") {
        return Err(unsupported(
            "Named and new-window form targets are not implemented",
        ));
    }
    let encoding = if method == "POST" {
        match enum_value(
            attribute("enctype", "formenctype"),
            &[
                "multipart/form-data",
                "text/plain",
                "application/x-www-form-urlencoded",
            ],
            "application/x-www-form-urlencoded",
        ) {
            "multipart/form-data" => Encoding::Multipart,
            "text/plain" => Encoding::Plain,
            _ => Encoding::UrlEncoded,
        }
    } else {
        Encoding::UrlEncoded
    };
    let (encoded, mime) = form_data::encode(vm, entries, encoding)?;
    if method == "GET" {
        target.set_query(Some(
            std::str::from_utf8(&encoded).expect("URL-encoded data is ASCII"),
        ));
    }
    Ok(FormSubmission {
        form,
        submitter,
        url: target.into(),
        method: method.into(),
        content_type: (method == "POST").then_some(mime),
        body: (method == "POST").then_some(encoded),
    })
}
