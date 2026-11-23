//! Internal ArrayBuffer and view records used by structured cloning.
use super::{Buffer, TypedArray, View, element::Element};
use crate::{JsError, ObjectId, StructuredCloneError, Value, Vm, heap::ObjectKind};

#[derive(Clone, Copy)]
pub(crate) struct ViewRecord {
    view: View,
    element: Option<Element>,
}

impl ViewRecord {
    pub(crate) fn buffer(self) -> ObjectId {
        self.view.buffer
    }
}

impl Vm {
    pub(crate) fn clone_buffer_bytes(
        &mut self,
        source: ObjectId,
    ) -> Result<Value, StructuredCloneError> {
        let buffer = self.buffer_data(source)?;
        let len = buffer.bytes.as_ref().ok_or(detached())?.len();
        let maximum = buffer.maximum;
        self.check_binary_allocation(len, 1)?;
        let bytes = self.copy_binary_range(source, 0..len)?;
        Ok(self.create_buffer(bytes, maximum, self.array_buffer_prototype())?)
    }

    pub(crate) fn clone_view_record(
        &self,
        source: ObjectId,
    ) -> Result<ViewRecord, StructuredCloneError> {
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
        Ok(ViewRecord { view, element })
    }

    pub(crate) fn materialize_clone_view(
        &mut self,
        record: ViewRecord,
        buffer: ObjectId,
    ) -> Result<Value, JsError> {
        let view = View {
            buffer,
            ..record.view
        };
        match record.element {
            Some(element) => self.new_typed_view(
                TypedArray { view, element },
                self.binary.typed[element as usize].expect("initialized typed array prototype"),
            ),
            None => {
                self.check_binary_allocation(0, 1)?;
                self.allocate_constructed(
                    ObjectKind::DataView(view),
                    self.binary
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
        if self.buffer_data(source)?.bytes.is_none() {
            return Err(detached());
        }
        Ok(())
    }

    /// Called after validation of every participant, with no intervening
    /// JavaScript execution. The existing allocation charge follows the store.
    pub(crate) fn commit_clone_transfer(&mut self, source: ObjectId, destination: ObjectId) {
        let buffer = self
            .buffer_data_mut(source)
            .expect("prevalidated transfer source");
        let moved = Buffer {
            bytes: buffer.bytes.take(),
            maximum: buffer.maximum,
        };
        *self
            .buffer_data_mut(destination)
            .expect("preallocated transfer destination") = moved;
    }
}

fn detached() -> StructuredCloneError {
    StructuredCloneError::DataClone("ArrayBuffer is detached")
}
