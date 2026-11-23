//! HTML structured-clone bindings shared by Window and MessagePort.
use super::{
    abort::dom_exception_error,
    arg,
    blobs::BlobFactory,
    canvas::{ImageBitmapFactory, ImageDataFactory},
    dom_api,
    file_list::FileListHost,
    message_ports::MessagePorts,
    webidl,
};
use rust_browser_javascript::{
    JsError, MAX_STRUCTURED_CLONE_TRANSFERS, PlatformClone, StructuredCloneError,
    StructuredCloneOutput, Value, Vm,
};

#[derive(Clone)]
pub(super) struct CloneHost {
    blobs: BlobFactory,
    image_data: ImageDataFactory,
    image_bitmap: ImageBitmapFactory,
    file_lists: FileListHost,
    dom_exception: Value,
    exception_prototype: Value,
}

impl CloneHost {
    pub(super) fn new(
        vm: &mut Vm,
        blobs: BlobFactory,
        image_data: ImageDataFactory,
        image_bitmap: ImageBitmapFactory,
        file_lists: FileListHost,
    ) -> Result<Self, JsError> {
        let dom_exception = vm.get_global("DOMException")?;
        vm.root(dom_exception.clone());
        let exception_prototype = vm.get_property(dom_exception.clone(), "prototype")?;
        vm.root(exception_prototype.clone());
        Ok(Self {
            blobs,
            image_data,
            image_bitmap,
            file_lists,
            dom_exception,
            exception_prototype,
        })
    }

    pub(super) fn install(
        self,
        vm: &mut Vm,
        window: Value,
        ports: MessagePorts,
    ) -> Result<(), JsError> {
        webidl::operation(
            vm,
            window,
            "structuredClone",
            1,
            move |vm, receiver, args| {
                if !receiver.is_nullish() && receiver != vm.global() {
                    return Err(JsError::type_error("Illegal structuredClone receiver"));
                }
                webidl::required(args, 1, "structuredClone")?;
                let transfer = transfer_list(vm, arg(args, 1))?;
                Ok(self
                    .clone_graph(vm, args[0].clone(), &transfer, &ports)?
                    .value)
            },
        )?;
        Ok(())
    }

    fn clone_platform(
        &self,
        vm: &mut Vm,
        source: &Value,
    ) -> Result<Option<PlatformClone>, StructuredCloneError> {
        if let Some(object) = self.blobs.clone_blob(vm, source)? {
            return Ok(Some(PlatformClone {
                object,
                slots: vec![],
            }));
        }
        if let Some(object) = dom_api::clone_exception(vm, source, &self.exception_prototype)? {
            return Ok(Some(PlatformClone {
                object,
                slots: vec![],
            }));
        }
        if let Some(value) = self.file_lists.clone_file_list(vm, source)? {
            return Ok(Some(value));
        }
        if let Some(value) = self.image_bitmap.clone_bitmap(vm, source)? {
            return Ok(Some(value));
        }
        Ok(self.image_data.clone_image_data(vm, source)?)
    }

    pub(super) fn clone_graph(
        &self,
        vm: &mut Vm,
        value: Value,
        transfer: &[Value],
        ports: &MessagePorts,
    ) -> Result<StructuredCloneOutput, JsError> {
        vm.structured_clone_with_hooks(
            value,
            transfer,
            |vm, source| self.clone_platform(vm, source),
            |vm, source| {
                if let Some(plan) = ports.prepare_transfer(vm, source)? {
                    return Ok(Some(plan));
                }
                self.image_bitmap.prepare_transfer(vm, source)
            },
        )
        .map_err(|error| match error {
            StructuredCloneError::DataClone(message) => self.data_clone_error(vm, message),
            StructuredCloneError::JavaScript(error) => error,
        })
    }

    pub(super) fn data_clone_error(&self, vm: &mut Vm, message: &str) -> JsError {
        dom_exception_error(vm, self.dom_exception.clone(), "DataCloneError", message)
            .unwrap_or_else(|error| error)
    }
}

pub(super) fn transfer_argument(vm: &mut Vm, argument: Value) -> Result<Vec<Value>, JsError> {
    // Web IDL distinguishes a sequence from a dictionary by its @@iterator.
    // Retain that method so argument getters are evaluated only once.
    if argument.as_object().is_some() {
        let iterator = webidl::iterator_method(vm, &argument)?;
        if !iterator.is_nullish() {
            return transfer_sequence(vm, argument, iterator);
        }
    }
    transfer_list(vm, argument)
}

fn transfer_list(vm: &mut Vm, options: Value) -> Result<Vec<Value>, JsError> {
    if options.is_nullish() {
        return Ok(Vec::new());
    }
    if options.as_object().is_none() {
        return Err(JsError::type_error(
            "structuredClone options must be a dictionary",
        ));
    }
    let items = vm.get_property(options, "transfer")?;
    if items.is_undefined() {
        return Ok(Vec::new());
    }
    if items.as_object().is_none() {
        return Err(JsError::type_error(
            "structuredClone transfer must be an iterable object",
        ));
    }
    let iterator = webidl::iterator_method(vm, &items)?;
    transfer_sequence(vm, items, iterator)
}

fn transfer_sequence(vm: &mut Vm, items: Value, iterator: Value) -> Result<Vec<Value>, JsError> {
    webidl::sequence(
        vm,
        items,
        iterator,
        MAX_STRUCTURED_CLONE_TRANSFERS,
        |_, item| {
            if item.as_object().is_none() {
                return Err(JsError::type_error(
                    "structuredClone transfer item must be an object",
                ));
            }
            Ok(item)
        },
    )
}
