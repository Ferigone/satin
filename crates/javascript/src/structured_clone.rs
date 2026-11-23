//! Same-realm structured cloning with identity preservation and a transfer
//! commit phase after graph traversal.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, Value, Vm,
    collections::CollectionCloneEntry,
    heap::{ObjectKind, PropertyKind},
};
use std::{collections::BTreeMap, mem::size_of, vec::IntoIter};

pub const MAX_STRUCTURED_CLONE_TRANSFERS: usize = 4096;

#[derive(Debug)]
pub enum StructuredCloneError {
    /// The binding converts this to its intrinsic DataCloneError DOMException.
    DataClone(&'static str),
    /// Getter exceptions and VM resource failures propagate without conversion.
    JavaScript(JsError),
}

impl From<JsError> for StructuredCloneError {
    fn from(error: JsError) -> Self {
        Self::JavaScript(error)
    }
}

#[derive(Default)]
pub(crate) struct Intrinsics {
    pub(crate) date: Option<ObjectId>,
    pub(crate) map: Option<ObjectId>,
    pub(crate) set: Option<ObjectId>,
}

type CloneResult<T> = Result<T, StructuredCloneError>;
type PlatformHook<'a> = dyn Fn(&mut Vm, &Value) -> CloneResult<Option<PlatformClone>> + 'a;
type TransferHook<'a> = dyn Fn(&mut Vm, &Value) -> CloneResult<Option<PlatformTransfer>> + 'a;
type TransferValidation = dyn Fn(&Vm) -> CloneResult<()>;

/// A new platform wrapper and the trusted steps to move the original's state.
/// Preparation may allocate but must leave the source intact. Validation runs
/// after author getters. Commit must be infallible, allocate nothing and run no
/// author code. All transfers are validated before the first commit.
pub struct PlatformTransfer {
    pub object: Value,
    pub validate: Box<TransferValidation>,
    pub commit: Box<dyn FnOnce(&mut Vm)>,
}

/// Transfer destinations are in transfer-list order, including destinations
/// not referenced in the cloned graph.
#[derive(Debug)]
pub struct StructuredCloneOutput {
    pub value: Value,
    pub transferred: Vec<Value>,
}

enum Transfer {
    Buffer { from: ObjectId, to: ObjectId },
    Platform(PlatformTransfer),
}

impl Transfer {
    fn validate(&self, vm: &Vm) -> CloneResult<()> {
        match self {
            Self::Buffer { from, .. } => vm.validate_clone_transfer(*from),
            Self::Platform(plan) => (plan.validate)(vm),
        }
    }

    fn commit(self, vm: &mut Vm) {
        match self {
            Self::Buffer { from, to } => vm.commit_clone_transfer(from, to),
            Self::Platform(plan) => (plan.commit)(vm),
        }
    }
}

/// A new platform instance and the internal reference slots to clone through
/// the same graph. The host fills scalar data without running author code.
pub struct PlatformClone {
    pub object: Value,
    pub slots: Vec<(u32, Value)>,
}

enum Frame {
    Properties {
        source: ObjectId,
        target: Value,
        keys: IntoIter<JsString>,
    },
    Collection {
        target: ObjectId,
        entries: IntoIter<CollectionCloneEntry>,
        pending_value: Option<(Value, Value)>,
    },
    ErrorCause {
        target: Value,
        cause: Value,
    },
    Platform {
        target: Value,
        slots: IntoIter<(u32, Value)>,
    },
}

struct Cloner<'vm, 'hook> {
    vm: &'vm mut Vm,
    platform: Option<&'hook PlatformHook<'hook>>,
    platform_transfer: Option<&'hook TransferHook<'hook>>,
    copies: BTreeMap<ObjectId, ObjectId>,
    pending: Vec<Frame>,
}

impl Vm {
    /// Marks an object created for a native platform interface. A script object
    /// that inherits from its prototype is still an ordinary object.
    pub fn mark_platform_object(&mut self, value: &Value) -> Result<(), JsError> {
        let object = value
            .as_object()
            .ok_or_else(|| JsError::type_error("Platform object marker requires an object"))?;
        self.heap.object_mut(object)?.platform_object = true;
        Ok(())
    }

    /// Clones supported ECMAScript records into this realm, with ArrayBuffer
    /// transfer. Successful transfer detaches every original in the supplied
    /// list. Live author getters may run before transfer is committed.
    pub fn structured_clone(
        &mut self,
        value: Value,
        transfer: &[Value],
    ) -> Result<Value, StructuredCloneError> {
        Ok(self
            .clone_with_platform_hooks(value, transfer, None, None)?
            .value)
    }

    pub fn structured_clone_with_platform(
        &mut self,
        value: Value,
        transfer: &[Value],
        platform: impl Fn(&mut Vm, &Value) -> Result<Option<PlatformClone>, StructuredCloneError>,
    ) -> Result<Value, StructuredCloneError> {
        Ok(self
            .clone_with_platform_hooks(value, transfer, Some(&platform), None)?
            .value)
    }

    pub fn structured_clone_with_hooks(
        &mut self,
        value: Value,
        transfer: &[Value],
        platform: impl Fn(&mut Vm, &Value) -> Result<Option<PlatformClone>, StructuredCloneError>,
        prepare: impl Fn(&mut Vm, &Value) -> Result<Option<PlatformTransfer>, StructuredCloneError>,
    ) -> Result<StructuredCloneOutput, StructuredCloneError> {
        self.clone_with_platform_hooks(value, transfer, Some(&platform), Some(&prepare))
    }

    fn clone_with_platform_hooks(
        &mut self,
        value: Value,
        transfer: &[Value],
        platform: Option<&PlatformHook<'_>>,
        platform_transfer: Option<&TransferHook<'_>>,
    ) -> Result<StructuredCloneOutput, StructuredCloneError> {
        self.with_task(|vm| {
            Ok(Cloner {
                vm,
                platform,
                platform_transfer,
                copies: BTreeMap::new(),
                pending: Vec::new(),
            }
            .clone_graph(value, transfer))
        })?
    }
}

impl Cloner<'_, '_> {
    fn clone_graph(
        &mut self,
        value: Value,
        transfer: &[Value],
    ) -> CloneResult<StructuredCloneOutput> {
        let (transfers, transferred) = self.prepare_transfers(transfer)?;
        let value = self.visit(value)?;
        while let Some(frame) = self.pending.pop() {
            self.vm.tick()?;
            self.advance(frame)?;
        }
        self.vm.consume_work(transfers.len().saturating_mul(2))?;
        for transfer in &transfers {
            transfer.validate(self.vm)?;
        }
        for transfer in transfers {
            transfer.commit(self.vm);
        }
        Ok(StructuredCloneOutput { value, transferred })
    }

    fn prepare_transfers(
        &mut self,
        transfer: &[Value],
    ) -> CloneResult<(Vec<Transfer>, Vec<Value>)> {
        if transfer.len() > MAX_STRUCTURED_CLONE_TRANSFERS {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Structured clone transfer list budget exhausted",
            )
            .into());
        }
        self.vm
            .reserve_allocation(transfer.len() * (size_of::<Transfer>() + size_of::<Value>()))?;
        let mut plans = Vec::new();
        plans
            .try_reserve_exact(transfer.len())
            .map_err(|_| temporary_allocation_error())?;
        let mut destinations = Vec::new();
        destinations
            .try_reserve_exact(transfer.len())
            .map_err(|_| temporary_allocation_error())?;
        for value in transfer {
            self.vm.tick()?;
            let source = value.as_object().ok_or(invalid_transfer())?;
            if self.copies.contains_key(&source) {
                return Err(StructuredCloneError::DataClone("Duplicate transferable"));
            }
            let object = self.vm.heap.object(source)?;
            let (target, plan) = if matches!(object.kind, ObjectKind::ArrayBuffer(_)) {
                let target = self.vm.new_transfer_destination()?;
                (
                    target,
                    Transfer::Buffer {
                        from: source,
                        to: target,
                    },
                )
            } else if let Some(prepare) = self.platform_transfer
                && object.is_platform_object()
                && let Some(plan) = prepare(self.vm, value)?
            {
                let target = plan.object.as_object().ok_or_else(|| {
                    JsError::new(
                        ErrorKind::InternalError,
                        "Transfer destination must be an object",
                    )
                })?;
                self.vm.heap.object(target)?;
                (target, Transfer::Platform(plan))
            } else {
                return Err(invalid_transfer());
            };
            self.remember(source, target)?;
            plans.push(plan);
            destinations.push(Value::Object(target));
        }
        Ok((plans, destinations))
    }

    fn remember(&mut self, source: ObjectId, target: ObjectId) -> Result<(), JsError> {
        self.vm.reserve_allocation(96)?;
        self.copies.insert(source, target);
        Ok(())
    }

    fn push(&mut self, frame: Frame) -> Result<(), JsError> {
        if self.pending.len() == self.pending.capacity() {
            let extra = self.pending.len().max(4);
            self.vm
                .reserve_allocation(extra.saturating_mul(size_of::<Frame>()))?;
            self.pending
                .try_reserve_exact(extra)
                .map_err(|_| temporary_allocation_error())?;
        }
        self.pending.push(frame);
        Ok(())
    }

    fn visit(&mut self, value: Value) -> CloneResult<Value> {
        self.vm.tick()?;
        let source = match value {
            Value::Object(id) => id,
            Value::Symbol(_) => {
                return Err(StructuredCloneError::DataClone("Symbols are not cloneable"));
            }
            primitive => return Ok(self.vm.account_value(primitive)?),
        };
        self.vm
            .consume_work(self.copies.len().max(1).ilog2() as usize)?;
        if let Some(&target) = self.copies.get(&source) {
            return Ok(Value::Object(target));
        }
        let object = self.vm.heap.object(source)?;
        if object.is_platform_object() {
            return self.copy_platform(source);
        }
        let target = match &object.kind {
            ObjectKind::Ordinary => self.copy_properties(source, None)?,
            ObjectKind::Array { length, .. } => self.copy_properties(source, Some(*length))?,
            ObjectKind::Boxed(
                value @ (Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::BigInt(_)),
            ) => self.vm.coerce_object(value.clone())?,
            ObjectKind::Date(time) => self.vm.allocate_constructed(
                ObjectKind::Date(*time),
                self.vm
                    .clone_intrinsics
                    .date
                    .expect("initialized Date prototype"),
            )?,
            ObjectKind::RegExp(program) => crate::regexp::literal(self.vm, program.clone())?,
            ObjectKind::ArrayBuffer(_) => self.vm.clone_buffer_bytes(source)?,
            ObjectKind::TypedArray(_) | ObjectKind::DataView(_) => {
                let record = self.vm.clone_view_record(source)?;
                let buffer = record.buffer();
                let target_buffer = if let Some(&copy) = self.copies.get(&buffer) {
                    copy
                } else {
                    let copy = self.vm.clone_buffer_bytes(buffer)?.as_object().unwrap();
                    self.remember(buffer, copy)?;
                    copy
                };
                self.vm.materialize_clone_view(record, target_buffer)?
            }
            ObjectKind::Collection(_) => {
                let (target, entries) = self.vm.collection_clone_snapshot(source)?;
                self.push(Frame::Collection {
                    target: target.as_object().unwrap(),
                    entries: entries.into_iter(),
                    pending_value: None,
                })?;
                target
            }
            ObjectKind::Error(_) => self.copy_error(source)?,
            _ => {
                return Err(StructuredCloneError::DataClone(
                    "Value is not structured-cloneable",
                ));
            }
        };
        self.remember(source, target.as_object().unwrap())?;
        Ok(target)
    }

    fn copy_platform(&mut self, source: ObjectId) -> CloneResult<Value> {
        let exception = StructuredCloneError::DataClone("Platform object is not cloneable");
        let platform = self.platform.ok_or(exception)?;
        let Some(copy) = platform(self.vm, &Value::Object(source))? else {
            return Err(StructuredCloneError::DataClone(
                "Platform object is not cloneable",
            ));
        };
        let target = copy.object.as_object().ok_or_else(|| {
            JsError::new(
                ErrorKind::InternalError,
                "Structured clone platform hook returned a non-object",
            )
        })?;
        self.remember(source, target)?;
        if !copy.slots.is_empty() {
            self.vm
                .reserve_allocation(copy.slots.capacity() * size_of::<(u32, Value)>())?;
            self.push(Frame::Platform {
                target: copy.object.clone(),
                slots: copy.slots.into_iter(),
            })?;
        }
        Ok(copy.object)
    }

    fn copy_properties(&mut self, source: ObjectId, length: Option<u32>) -> CloneResult<Value> {
        let target = if let Some(length) = length {
            if length > self.vm.limits.max_array_length {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Array length budget exhausted",
                )
                .into());
            }
            self.vm.allocate_constructed(
                ObjectKind::Array {
                    length,
                    length_writable: true,
                },
                self.vm.array_prototype,
            )?
        } else {
            self.vm
                .allocate_constructed(ObjectKind::Ordinary, self.vm.object_prototype)?
        };
        let keys = self.vm.enumerable_own_keys(Value::Object(source))?;
        self.push(Frame::Properties {
            source,
            target: target.clone(),
            keys: keys.into_iter(),
        })?;
        Ok(target)
    }

    fn copy_error(&mut self, source: ObjectId) -> CloneResult<Value> {
        let name = self.vm.get_property(Value::Object(source), "name")?;
        let supported = [
            ErrorKind::EvalError,
            ErrorKind::RangeError,
            ErrorKind::ReferenceError,
            ErrorKind::SyntaxError,
            ErrorKind::TypeError,
            ErrorKind::UriError,
        ];
        let kind = supported
            .into_iter()
            .find(|kind| {
                name.as_string()
                    .is_some_and(|s| kind.name().encode_utf16().eq(s.as_utf16().iter().copied()))
            })
            .unwrap_or(ErrorKind::Error);
        let prototype = self
            .vm
            .error_prototypes
            .get(&kind)
            .copied()
            .unwrap_or(self.vm.error_prototype);
        let message = self
            .own_data(source, "message")?
            .map(|value| self.vm.to_string(&value))
            .transpose()?;
        let target = self
            .vm
            .allocate_constructed(ObjectKind::Error(kind), prototype)?;
        if let Some(message) = message {
            self.vm.define_data_property(
                target.clone(),
                "message",
                message.into(),
                true,
                false,
                true,
            )?;
        }
        if let Some(cause) = self.own_data(source, "cause")? {
            self.push(Frame::ErrorCause {
                target: target.clone(),
                cause,
            })?;
        }
        Ok(target)
    }

    fn own_data(&mut self, object: ObjectId, key: &str) -> Result<Option<Value>, JsError> {
        Ok(self
            .vm
            .own_descriptor(object, &key.into())?
            .and_then(|property| match property.kind {
                PropertyKind::Data { value, .. } => Some(value),
                PropertyKind::Accessor { .. } => None,
            }))
    }

    fn advance(&mut self, frame: Frame) -> CloneResult<()> {
        match frame {
            Frame::Properties {
                source,
                target,
                mut keys,
            } => {
                let Some(key) = keys.next() else {
                    return Ok(());
                };
                self.push(Frame::Properties {
                    source,
                    target: target.clone(),
                    keys,
                })?;
                // The key was enumerable at the snapshot. A prior getter can
                // delete it, in which case we must not consult the prototype.
                if self.vm.own_descriptor(source, &key)?.is_some() {
                    let value = self.vm.get(Value::Object(source), &key)?;
                    let cloned = self.visit(value)?;
                    self.vm
                        .define_property(target, key, crate::heap::Property::data(cloned))?;
                }
            }
            Frame::Collection {
                target,
                mut entries,
                pending_value,
            } => {
                if let Some((key, value)) = pending_value {
                    self.push(Frame::Collection {
                        target,
                        entries,
                        pending_value: None,
                    })?;
                    let cloned = self.visit(value)?;
                    self.vm.collection_insert(target, key, cloned)?;
                } else if let Some((key, value)) = entries.next() {
                    let parent = self.pending.len();
                    self.push(Frame::Collection {
                        target,
                        entries,
                        pending_value: None,
                    })?;
                    let key = self.visit(key)?;
                    if let Some(value) = value {
                        let Frame::Collection { pending_value, .. } = &mut self.pending[parent]
                        else {
                            unreachable!("collection frame below cloned key")
                        };
                        *pending_value = Some((key, value));
                    } else {
                        self.vm.collection_insert(target, key, Value::Undefined)?;
                    }
                }
            }
            Frame::ErrorCause { target, cause } => {
                let cloned = self.visit(cause)?;
                self.vm
                    .define_data_property(target, "cause", cloned, true, false, true)?;
            }
            Frame::Platform { target, mut slots } => {
                if let Some((slot, source)) = slots.next() {
                    if !slots.as_slice().is_empty() {
                        self.push(Frame::Platform {
                            target: target.clone(),
                            slots,
                        })?;
                    }
                    let cloned = self.visit(source)?;
                    self.vm.set_host_slot(target, slot, cloned)?;
                }
            }
        }
        Ok(())
    }
}

fn temporary_allocation_error() -> JsError {
    JsError::new(
        ErrorKind::HeapLimit,
        "Structured clone bookkeeping allocation failed",
    )
}

fn invalid_transfer() -> StructuredCloneError {
    StructuredCloneError::DataClone("Value is not transferable")
}
