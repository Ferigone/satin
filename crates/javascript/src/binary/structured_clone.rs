//! Internal ArrayBuffer and view records used by structured cloning.
use super::{Buffer, TypedArray, View, element::Element};
use crate::{JsError, ObjectId, StructuredCloneError, Value, Vm, heap::ObjectKind};

#[derive(Clone, Copy)]
pub(crate) struct ViewLayout {
    offset: usize,
    bytes: Option<usize>,
    element: Option<Element>,
}

impl Vm {
    pub(crate) fn clone_buffer_bytes(
        &mut self,
        source: ObjectId,
    ) -> Result<Value, StructuredCloneError> {
        if let ObjectKind::SharedArrayBuffer(backing) = self.heap.object(source)?.kind {
            let proto =
                self.realm
                    .binary
                    .shared_array_buffer
                    .ok_or(StructuredCloneError::DataClone(
                        "Shared memory is unavailable in this realm",
                    ))?;
            return Ok(self.allocate_constructed(ObjectKind::SharedArrayBuffer(backing), proto)?);
        }
        let (len, maximum) = self.clone_buffer_layout(source)?;
        self.check_binary_allocation(len, 1)?;
        let bytes = self.copy_binary_range(source, 0..len)?;
        Ok(self.materialize_clone_buffer(bytes, maximum)?)
    }

    pub(crate) fn clone_buffer_layout(
        &self,
        source: ObjectId,
    ) -> Result<(usize, Option<usize>), StructuredCloneError> {
        self.require_unshared(source).map_err(|_| {
            StructuredCloneError::DataClone("Shared memory cannot cross this host agent boundary")
        })?;
        let buffer = self.buffer_data(source)?;
        Ok((
            buffer.bytes.as_ref().ok_or(detached())?.len(),
            buffer.maximum,
        ))
    }

    pub(crate) fn materialize_clone_buffer(
        &mut self,
        bytes: Vec<u8>,
        maximum: Option<usize>,
    ) -> Result<Value, JsError> {
        self.create_buffer(bytes, maximum, self.array_buffer_prototype())
    }

    pub(crate) fn clone_view_layout(
        &self,
        source: ObjectId,
    ) -> Result<(ObjectId, ViewLayout), StructuredCloneError> {
        let (view, element) = match self.heap.object(source)?.kind {
            ObjectKind::TypedArray(array) => (array.view, Some(array.element)),
            ObjectKind::DataView(view) => (view, None),
            _ => unreachable!("cloner validated the array buffer view kind"),
        };
        let width = element.map_or(1, Element::width);
        if self.view_range(view, width)?.is_none() {
            return Err(StructuredCloneError::DataClone(
                "ArrayBufferView is detached or out of bounds",
            ));
        }
        Ok((
            view.buffer,
            ViewLayout {
                offset: view.offset,
                bytes: view.bytes,
                element,
            },
        ))
    }

    pub(crate) fn materialize_clone_view(
        &mut self,
        layout: ViewLayout,
        buffer: ObjectId,
    ) -> Result<Value, JsError> {
        let view = View {
            buffer,
            offset: layout.offset,
            bytes: layout.bytes,
        };
        match layout.element {
            Some(element) => self.new_typed_view(
                TypedArray { view, element },
                self.realm.binary.typed[element as usize]
                    .expect("initialized typed array prototype"),
            ),
            None => {
                self.check_binary_allocation(0, 1)?;
                self.allocate_constructed(
                    ObjectKind::DataView(view),
                    self.realm
                        .binary
                        .data_view
                        .expect("initialized DataView prototype"),
                )
            }
        }
    }

    pub(crate) fn new_transfer_destination(&mut self) -> Result<ObjectId, JsError> {
        self.check_binary_allocation(0, 1)?;
        Ok(self
            .allocate_constructed(
                ObjectKind::ArrayBuffer(Buffer {
                    bytes: None,
                    maximum: None,
                    wasm: false,
                }),
                self.array_buffer_prototype(),
            )?
            .as_object()
            .unwrap())
    }

    pub(crate) fn validate_clone_transfer(
        &self,
        source: ObjectId,
    ) -> Result<(), StructuredCloneError> {
        self.clone_transfer_capacity(source).map(|_| ())
    }

    pub(crate) fn clone_transfer_capacity(
        &self,
        source: ObjectId,
    ) -> Result<usize, StructuredCloneError> {
        self.require_unshared(source)
            .map_err(|_| StructuredCloneError::DataClone("Shared buffers cannot be transferred"))?;
        let buffer = self.buffer_data(source)?;
        if buffer.wasm {
            return Err(StructuredCloneError::DataClone(
                "WebAssembly buffers cannot be transferred",
            ));
        }
        Ok(buffer.bytes.as_ref().ok_or_else(detached)?.capacity())
    }

    /// Called after validation of every participant, with no intervening
    /// JavaScript execution. The existing allocation charge follows the store.
    pub(crate) fn commit_clone_transfer(&mut self, source: ObjectId, destination: ObjectId) {
        let (bytes, maximum) = self.take_clone_store(source);
        *self
            .buffer_data_mut(destination)
            .expect("preallocated transfer destination") = Buffer {
            bytes: Some(bytes),
            maximum,
            wasm: false,
        };
    }

    /// Moves a validated store out of this VM without allocating or calling JS.
    pub(crate) fn take_clone_packet_store(&mut self, source: ObjectId) -> (Vec<u8>, Option<usize>) {
        let (bytes, maximum) = self.take_clone_store(source);
        self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_sub(bytes.capacity());
        (bytes, maximum)
    }

    fn take_clone_store(&mut self, source: ObjectId) -> (Vec<u8>, Option<usize>) {
        let buffer = self
            .buffer_data_mut(source)
            .expect("prevalidated transfer source");
        (
            buffer.bytes.take().expect("prevalidated transfer bytes"),
            buffer.maximum,
        )
    }
}

fn detached() -> StructuredCloneError {
    StructuredCloneError::DataClone("ArrayBuffer is detached")
}
