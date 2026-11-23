//! File selection state and the browser's picker request boundary.
use super::{FormHost, attr, disabled, html, input_type};
use crate::dom::{Document, NodeId};
use crate::script::{
    Shared, arg,
    blobs::{self, Blob, BlobFactory},
    dispatch_prepared, element, event_target,
    file_list::{self, FileList, FileListHost},
    set_attribute, text, webidl,
};
use ring::digest::{SHA256, digest};
use rust_browser_javascript::{ErrorKind, JsError, JsString, RootId, Value, Vm};
use std::{
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

pub const MAX_PICKER_FILES: usize = file_list::MAX_SELECTED_FILES;
pub const MAX_PICKER_FILE_BYTES: usize = blobs::BLOB_BYTES;
pub const MAX_PICKER_BYTES: usize = blobs::LIVE_BYTES;
const ACCEPT_BYTES: usize = 4096;
const PICKER_ACTIVATION: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct FilePickerRequest {
    serial: u64,
    pub input: NodeId,
    pub multiple: bool,
    pub accept: String,
}

pub struct SelectedFile {
    /// Used for reselecting the same disk file. Only the name reaches the DOM.
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub last_modified: i64,
    pub bytes: Vec<u8>,
}

struct Selection {
    root: RootId,
    files: Rc<FileList>,
    source_keys: Option<Rc<[[u8; 32]]>>,
}

struct Pending {
    request: FilePickerRequest,
    delivered: bool,
}

#[derive(Default)]
pub(super) struct FileInputs {
    selections: HashMap<NodeId, Selection>,
    pending: Option<Pending>,
    serial: u64,
    activated: Option<Instant>,
    blobs: Option<BlobFactory>,
}

pub(super) fn is_file(doc: &Document, id: NodeId) -> bool {
    html(doc, id, "input") && input_type(doc, id) == "file"
}

fn input(dom: &Shared, value: &Value) -> Result<NodeId, JsError> {
    let id = element(dom, value)?;
    if html(&dom.borrow().document, id, "input") {
        Ok(id)
    } else {
        Err(JsError::type_error("Illegal HTMLInputElement receiver"))
    }
}

impl FileInputs {
    pub(super) fn value(&self, id: NodeId) -> JsString {
        self.selections
            .get(&id)
            .and_then(|selection| selection.files.first_name())
            .map_or_else(JsString::default, |name| {
                let mut units: Vec<u16> = "C:\\fakepath\\".encode_utf16().collect();
                units.extend_from_slice(name.as_utf16());
                JsString::from_utf16(units)
            })
    }

    pub(super) fn label(&self, id: NodeId) -> String {
        let Some(selection) = self.selections.get(&id) else {
            return String::new();
        };
        if selection.files.len() > 1 {
            return format!("Wybrano pliki ({})", selection.files.len());
        }
        selection
            .files
            .first_name()
            .map_or_else(String::new, |name| name.to_string_lossy())
    }

    pub(super) fn selected(&self, id: NodeId) -> Option<&FileList> {
        self.selections
            .get(&id)
            .map(|selection| selection.files.as_ref())
    }

    pub(super) fn clear(&mut self, vm: &mut Vm, id: NodeId) -> Result<(), JsError> {
        if let Some(selection) = self.selections.get_mut(&id) {
            selection.files.clear(vm)?;
            selection.source_keys = None;
        }
        Ok(())
    }

    pub(super) fn remove(&mut self, vm: &mut Vm, id: NodeId) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request.input == id)
        {
            self.pending = None;
        }
        if let Some(selection) = self.selections.remove(&id) {
            vm.unroot(selection.root);
        }
    }

    fn replace(
        &mut self,
        vm: &mut Vm,
        id: NodeId,
        value: Value,
        source_keys: Option<Rc<[[u8; 32]]>>,
    ) {
        let files = FileList::get(vm, &value).expect("branded FileList");
        let root = vm.root(value);
        if let Some(old) = self.selections.insert(
            id,
            Selection {
                root,
                files,
                source_keys,
            },
        ) {
            vm.unroot(old.root);
        }
    }

    pub(super) fn clone_selections(
        &mut self,
        vm: &mut Vm,
        factory: &FileListHost,
        sources: &[NodeId],
        base: usize,
    ) -> Result<(), JsError> {
        let mut lists = Vec::new();
        for (at, id) in sources.iter().enumerate() {
            if let Some(selection) = self.selections.get(id)
                && selection.files.len() > 0
            {
                let copy = factory.duplicate(vm, &selection.files)?;
                lists.push((NodeId(base + at), copy, selection.source_keys.clone()));
            }
        }
        for (node, value, paths) in lists {
            self.replace(vm, node, value, paths);
        }
        Ok(())
    }

    fn unchanged(
        &self,
        vm: &mut Vm,
        node: NodeId,
        files: &[SelectedFile],
        source_keys: &[[u8; 32]],
    ) -> Result<bool, JsError> {
        let Some(prior) = self.selections.get(&node) else {
            return Ok(false);
        };
        if prior.files.len() != files.len() || prior.source_keys.as_deref() != Some(source_keys) {
            return Ok(false);
        }
        for (index, file) in files.iter().enumerate() {
            let prior = prior
                .files
                .at(vm, index as u32)?
                .expect("nonempty FileList entry");
            let prior = vm
                .host_data::<Blob>(&prior)
                .expect("FileList contains Files");
            let info = prior.file.as_ref().expect("File metadata");
            if info.name != file.name
                || info.last_modified != file.last_modified
                || prior.mime.as_ref() != file.mime
                || prior.bytes().len() != file.bytes.len()
            {
                return Ok(false);
            }
            vm.charge_work(file.bytes.len().div_ceil(64))?;
            if prior.bytes() != file.bytes {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl FormHost {
    pub(in crate::script) fn install_file_inputs(
        &self,
        vm: &mut Vm,
        prototype: Value,
    ) -> Result<(), JsError> {
        webidl::interface(
            vm,
            prototype.clone(),
            "HTMLInputElement",
            0,
            |_, _, _, _| Err(JsError::type_error("Illegal HTMLInputElement constructor")),
        )?;
        let host = self.clone();
        let get = vm.new_native_function("get files", move |vm, this, _| {
            let id = input(&host.dom, &this)?;
            if !is_file(&host.dom.borrow().document, id) {
                return Ok(Value::Null);
            }
            let mut data = host.data.borrow_mut();
            if let Some(selection) = data.file_inputs.selections.get(&id) {
                return Ok(vm
                    .root_value(selection.root)
                    .expect("rooted input FileList"));
            }
            let list = host.file_lists.create(vm, &[])?;
            data.file_inputs.replace(vm, id, list.clone(), None);
            Ok(list)
        });
        let host = self.clone();
        let set = vm.new_native_function("set files", move |vm, this, args| {
            let id = input(&host.dom, &this)?;
            let value = arg(args, 0);
            if value.is_nullish() {
                return Ok(Value::Undefined);
            }
            if FileList::get(vm, &value).is_none() {
                return Err(JsError::type_error("input.files requires a FileList"));
            }
            if is_file(&host.dom.borrow().document, id) {
                let mut data = host.data.borrow_mut();
                let same = data
                    .file_inputs
                    .selections
                    .get(&id)
                    .and_then(|selection| vm.root_value(selection.root))
                    .is_some_and(|previous| previous == value);
                if !same {
                    data.file_inputs.replace(vm, id, value, None);
                    drop(data);
                    host.mark();
                }
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), "files", Some(get), Some(set))?;

        let state = self.dom.clone();
        let get = vm.new_native_function("get accept", move |_, this, _| {
            let id = input(&state, &this)?;
            Ok(state
                .borrow()
                .document
                .attr(id, "accept")
                .unwrap_or("")
                .into())
        });
        let state = self.dom.clone();
        let set = vm.new_native_function("set accept", move |vm, this, args| {
            let id = input(&state, &this)?;
            let accept = text(vm, arg(args, 0))?;
            set_attribute(vm, &state, id, "accept", Some(accept))?;
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), "accept", Some(get), Some(set))?;

        let host = self.clone();
        webidl::operation(vm, prototype, "showPicker", 0, move |vm, this, _| {
            let id = input(&host.dom, &this)?;
            host.request_file_picker(vm, id, true)?;
            Ok(Value::Undefined)
        })?;
        Ok(())
    }

    pub(in crate::script) fn set_file_factory(&self, blobs: BlobFactory) {
        self.data.borrow_mut().file_inputs.blobs = Some(blobs);
    }

    pub(in crate::script) fn activate_picker_gesture(&self) {
        self.data.borrow_mut().file_inputs.activated = Some(Instant::now());
    }

    pub(super) fn request_file_picker(
        &self,
        vm: &mut Vm,
        id: NodeId,
        explicit: bool,
    ) -> Result<(), JsError> {
        let dom = self.dom.borrow();
        let doc = &dom.document;
        let file = is_file(doc, id);
        if disabled(doc, id)
            || (!file && attr(doc, id, "readonly") && super::text_editable(doc, id))
        {
            drop(dom);
            return if explicit {
                Err(self.exception(vm, "InvalidStateError", "The input is not mutable"))
            } else {
                Ok(())
            };
        }
        let mut form = self.data.borrow_mut();
        let data = &mut form.file_inputs;
        if data
            .activated
            .is_none_or(|time| time.elapsed() > PICKER_ACTIVATION)
        {
            drop(form);
            drop(dom);
            return if explicit {
                Err(self.exception(vm, "NotAllowedError", "The picker requires user activation"))
            } else {
                Ok(())
            };
        }
        if !file {
            return Ok(());
        }
        data.activated = None;
        if data.pending.is_none() {
            let accept = doc.attr(id, "accept").unwrap_or("");
            let accept =
                accept[..accept.floor_char_boundary(ACCEPT_BYTES.min(accept.len()))].to_owned();
            data.serial = data.serial.wrapping_add(1);
            data.pending = Some(Pending {
                request: FilePickerRequest {
                    serial: data.serial,
                    input: id,
                    multiple: attr(doc, id, "multiple"),
                    accept,
                },
                delivered: false,
            });
        }
        Ok(())
    }

    pub(in crate::script) fn take_file_picker_request(&self) -> Option<FilePickerRequest> {
        let mut data = self.data.borrow_mut();
        let pending = data.file_inputs.pending.as_mut()?;
        if pending.delivered {
            return None;
        }
        pending.delivered = true;
        Some(pending.request.clone())
    }

    pub(in crate::script) fn complete_file_picker(
        &self,
        vm: &mut Vm,
        request: &FilePickerRequest,
        result: Option<Vec<SelectedFile>>,
    ) -> Result<bool, JsError> {
        let node = request.input;
        {
            let mut data = self.data.borrow_mut();
            if !data.file_inputs.pending.as_ref().is_some_and(|pending| {
                pending.delivered
                    && pending.request.serial == request.serial
                    && pending.request.input == node
            }) {
                return Ok(false);
            }
            data.file_inputs.pending = None;
        }
        let multiple = {
            let dom = self.dom.borrow();
            let doc = &dom.document;
            if !is_file(doc, node) || disabled(doc, node) {
                return Ok(false);
            }
            request.multiple && attr(doc, node, "multiple")
        };
        let Some(files) = result else {
            self.fire_file_event(vm, node, "cancel")?;
            return Ok(true);
        };
        if files.is_empty() {
            self.fire_file_event(vm, node, "cancel")?;
            return Ok(true);
        }
        validate_files(&files, multiple)?;
        let source_keys = files
            .iter()
            .map(|file| {
                let bytes = file.path.as_os_str().as_encoded_bytes();
                vm.charge_work(bytes.len().div_ceil(16))?;
                Ok(digest(&SHA256, bytes)
                    .as_ref()
                    .try_into()
                    .expect("SHA-256 size"))
            })
            .collect::<Result<Vec<[u8; 32]>, JsError>>()?;
        let unchanged = self
            .data
            .borrow()
            .file_inputs
            .unchanged(vm, node, &files, &source_keys)?;
        if unchanged {
            self.fire_file_event(vm, node, "cancel")?;
            return Ok(true);
        }
        let blobs = self
            .data
            .borrow()
            .file_inputs
            .blobs
            .clone()
            .expect("installed File factory");
        let mut values = Vec::with_capacity(files.len());
        for file in files {
            values.push(blobs.file_from_host(
                vm,
                file.bytes,
                &file.mime,
                file.name,
                file.last_modified,
            )?);
        }
        let list = self.file_lists.create(vm, &values)?;
        self.data
            .borrow_mut()
            .file_inputs
            .replace(vm, node, list, Some(source_keys.into()));
        self.mark();
        self.fire_file_event(vm, node, "input")?;
        self.fire_file_event(vm, node, "change")?;
        Ok(true)
    }

    fn fire_file_event(&self, vm: &mut Vm, node: NodeId, kind: &str) -> Result<(), JsError> {
        let prototype = self.dom.borrow().event_prototype.clone();
        let object = vm.new_object_with_prototype(prototype)?;
        let event = event_target::init_event(
            vm,
            object,
            kind.into(),
            [true, false, kind == "input"],
            true,
        )?;
        dispatch_prepared(vm, &self.dom, Some(node), event, &[])?;
        Ok(())
    }
}

fn validate_files(files: &[SelectedFile], multiple: bool) -> Result<(), JsError> {
    if files.len() > if multiple { MAX_PICKER_FILES } else { 1 } {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Too many selected files",
        ));
    }
    let mut bytes = 0usize;
    for file in files {
        if file.name.is_empty() || file.name.contains(['/', '\\', '\0']) {
            return Err(JsError::type_error("Expected a selected file name"));
        }
        bytes = bytes.saturating_add(file.bytes.len());
        if file.bytes.len() > MAX_PICKER_FILE_BYTES || bytes > MAX_PICKER_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Selected files exceed the byte budget",
            ));
        }
    }
    Ok(())
}
