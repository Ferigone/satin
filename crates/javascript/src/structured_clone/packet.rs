//! Owned structured clone data for messages between independent JavaScript VMs.
use super::{
    CloneResult, MAX_STRUCTURED_CLONE_TRANSFERS, StructuredCloneError, invalid_transfer,
    new_record, push_frame, read_record_property, temporary_allocation_error,
};
use crate::{
    ErrorKind, JsBigInt, JsError, JsString, ObjectId, Value, Vm,
    binary::ViewLayout,
    heap::{ObjectKind, Property},
};
use num_bigint::BigInt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    mem::size_of,
    vec::IntoIter,
};

const ALLOCATION_OVERHEAD: usize = 64;

/// An opaque, single-use packet containing no references to the sending VM.
/// The embedding can move it between threads and charge its retained size to a mailbox.
pub struct StructuredClonePacket {
    root: PacketValue,
    records: Vec<PacketRecord>,
    retained_bytes: usize,
}

impl StructuredClonePacket {
    /// Conservative byte estimate for the packet and its owned allocations.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

impl fmt::Debug for StructuredClonePacket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StructuredClonePacket")
            .field("records", &self.records.len())
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

enum PacketValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    BigInt(BigInt),
    String(Vec<u16>),
    Record(usize),
}

struct PacketProperty {
    key: Vec<u16>,
    value: PacketValue,
}

enum PacketRecord {
    Properties {
        array_length: Option<u32>,
        properties: Vec<PacketProperty>,
    },
    ArrayBuffer {
        bytes: Vec<u8>,
        maximum: Option<usize>,
    },
    View {
        buffer: usize,
        layout: ViewLayout,
    },
}

struct PacketBudget {
    limit: usize,
    used: usize,
}

impl PacketBudget {
    fn check(&self, bytes: usize) -> Result<(), JsError> {
        if bytes > self.limit - self.used {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Structured clone packet byte budget exhausted",
            ));
        }
        Ok(())
    }

    fn reserve(&mut self, vm: &mut Vm, bytes: usize) -> Result<(), JsError> {
        self.check(bytes)?;
        vm.reserve_allocation(bytes)?;
        self.used += bytes;
        Ok(())
    }

    fn account_transferred_backing(&mut self, capacity: usize) -> Result<(), JsError> {
        let bytes = backing_bytes(capacity);
        self.check(bytes)?;
        self.used += bytes;
        Ok(())
    }

    fn reserve_vec<T>(
        &mut self,
        vm: &mut Vm,
        buffer: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), JsError> {
        let prior = buffer.capacity();
        let requested = buffer
            .len()
            .saturating_add(additional)
            .saturating_sub(prior);
        if requested == 0 {
            return Ok(());
        }
        let overhead = if prior == 0 { ALLOCATION_OVERHEAD } else { 0 };
        self.reserve(
            vm,
            requested
                .saturating_mul(size_of::<T>())
                .saturating_add(overhead),
        )?;
        buffer
            .try_reserve_exact(additional)
            .map_err(|_| temporary_allocation_error())?;
        let extra = buffer.capacity() - prior - requested;
        self.reserve(vm, extra.saturating_mul(size_of::<T>()))
    }
}

struct Frame {
    source: ObjectId,
    target: usize,
    keys: IntoIter<JsString>,
}

struct Encoder<'a> {
    vm: &'a mut Vm,
    budget: PacketBudget,
    records: Vec<PacketRecord>,
    seen: BTreeMap<ObjectId, usize>,
    frames: Vec<Frame>,
    transfers: BTreeSet<ObjectId>,
}

impl Vm {
    /// Serialize primitives, ordinary objects, arrays and copied ArrayBuffers
    /// with their views. Author getters run in the sender. Unsupported objects
    /// and symbols return DataClone errors.
    pub fn serialize_clone_packet(
        &mut self,
        value: Value,
        max_bytes: usize,
    ) -> Result<StructuredClonePacket, StructuredCloneError> {
        self.serialize_clone_packet_with_transfer(value, &[], max_bytes, |_| Ok(()))
            .map(|(packet, ())| packet)
    }

    /// Serialize a message and move the listed ArrayBuffer stores into it.
    /// After author getters and validation, `admit` can reserve the packet's
    /// final retained size in the embedding before any source is detached.
    /// A refused admission does not commit any transfers. The reservation is
    /// returned with the packet so it can live through message delivery.
    pub fn serialize_clone_packet_with_transfer<T>(
        &mut self,
        value: Value,
        transfer: &[Value],
        max_bytes: usize,
        admit: impl FnOnce(usize) -> Result<T, JsError>,
    ) -> Result<(StructuredClonePacket, T), StructuredCloneError> {
        self.with_task(|vm| {
            Ok(Encoder {
                vm,
                budget: PacketBudget {
                    limit: max_bytes,
                    used: 0,
                },
                records: Vec::new(),
                seen: BTreeMap::new(),
                frames: Vec::new(),
                transfers: BTreeSet::new(),
            }
            .encode(value, transfer, admit))
        })?
    }

    /// Reconstruct a packet in this VM with its intrinsic prototypes and limits.
    /// Receiving builds data properties and does not call script setters.
    pub fn deserialize_clone_packet(
        &mut self,
        packet: StructuredClonePacket,
    ) -> Result<Value, JsError> {
        self.with_task(|vm| decode(vm, packet))
    }
}

impl Encoder<'_> {
    fn encode<T>(
        mut self,
        value: Value,
        transfer: &[Value],
        admit: impl FnOnce(usize) -> Result<T, JsError>,
    ) -> CloneResult<(StructuredClonePacket, T)> {
        self.budget
            .reserve(self.vm, size_of::<StructuredClonePacket>())?;
        self.prepare_transfers(transfer)?;
        let root = self.visit(value)?;
        while let Some(mut frame) = self.frames.pop() {
            self.vm.tick()?;
            let Some(key) = frame.keys.next() else {
                continue;
            };
            let (source, target) = (frame.source, frame.target);
            if !frame.keys.as_slice().is_empty() {
                push_frame(self.vm, &mut self.frames, frame)?;
            }
            if let Some(value) = read_record_property(self.vm, source, &key)? {
                let key = self.copy_units(&key)?;
                let value = self.visit(value)?;
                let PacketRecord::Properties { properties, .. } = &mut self.records[target] else {
                    unreachable!("only record properties have a frame");
                };
                properties.push(PacketProperty { key, value });
            }
        }
        self.validate_transfers(transfer)?;
        let reservation = admit(self.budget.used)?;
        self.commit_transfers(transfer);
        Ok((
            StructuredClonePacket {
                root,
                records: self.records,
                retained_bytes: self.budget.used,
            },
            reservation,
        ))
    }

    fn prepare_transfers(&mut self, transfer: &[Value]) -> CloneResult<()> {
        if transfer.len() > MAX_STRUCTURED_CLONE_TRANSFERS {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Structured clone transfer list budget exhausted",
            )
            .into());
        }
        for value in transfer {
            self.vm
                .consume_work(1 + 2 * lookup_work(self.transfers.len()))?;
            let source = value.as_object().ok_or_else(invalid_transfer)?;
            if !matches!(
                self.vm.heap.object(source)?.kind,
                ObjectKind::ArrayBuffer(_)
            ) {
                return Err(invalid_transfer());
            }
            if self.transfers.contains(&source) {
                return Err(StructuredCloneError::DataClone("Duplicate transferable"));
            }
            self.vm.reserve_allocation(96)?;
            self.transfers.insert(source);
        }
        Ok(())
    }

    fn validate_transfers(&mut self, transfer: &[Value]) -> CloneResult<()> {
        // Budget both passes before admission. There can be no failure after
        // the first detach, and each capacity must reflect the final store.
        self.vm.consume_work(
            transfer
                .len()
                .saturating_mul(2 * (1 + lookup_work(self.seen.len()))),
        )?;
        for value in transfer {
            let source = value.as_object().expect("validated transfer list");
            let capacity = self.vm.clone_transfer_capacity(source)?;
            if self.seen.contains_key(&source) {
                self.budget.account_transferred_backing(capacity)?;
            }
        }
        Ok(())
    }

    fn commit_transfers(&mut self, transfer: &[Value]) {
        for value in transfer {
            let source = value.as_object().expect("validated transfer list");
            let (bytes, maximum) = self.vm.take_clone_packet_store(source);
            // ArrayBuffers that are absent from the message still detach. When
            // no receiver reference exists, their backing need not be queued.
            if let Some(&target) = self.seen.get(&source) {
                self.records[target] = PacketRecord::ArrayBuffer { bytes, maximum };
            }
        }
    }

    fn visit(&mut self, value: Value) -> CloneResult<PacketValue> {
        self.vm.tick()?;
        Ok(match value {
            Value::Undefined => PacketValue::Undefined,
            Value::Null => PacketValue::Null,
            Value::Bool(value) => PacketValue::Bool(value),
            Value::Number(value) => PacketValue::Number(value),
            Value::String(value) => PacketValue::String(self.copy_units(&value)?),
            Value::BigInt(value) => {
                let bytes = integer_bytes(value.bit_length());
                self.vm.consume_work(bytes.div_ceil(8))?;
                self.budget.reserve(self.vm, bytes)?;
                PacketValue::BigInt(value.to_owned_integer())
            }
            Value::Symbol(_) => {
                return Err(StructuredCloneError::DataClone("Symbols are not cloneable"));
            }
            Value::Object(source) => PacketValue::Record(self.visit_record(source)?),
        })
    }

    fn copy_units(&mut self, value: &JsString) -> Result<Vec<u16>, JsError> {
        self.vm.consume_work(value.len().div_ceil(32))?;
        let mut units = Vec::new();
        self.budget.reserve_vec(self.vm, &mut units, value.len())?;
        units.extend_from_slice(value.as_utf16());
        Ok(units)
    }

    fn visit_record(&mut self, source: ObjectId) -> CloneResult<usize> {
        self.vm.consume_work(lookup_work(self.seen.len()))?;
        if let Some(&target) = self.seen.get(&source) {
            return Ok(target);
        }
        let object = self.vm.heap.object(source)?;
        if object.is_platform_object() {
            return Err(StructuredCloneError::DataClone(
                "Platform object is not cloneable",
            ));
        }
        let record = match &object.kind {
            ObjectKind::Ordinary => self.record_properties(source, self.records.len(), None)?,
            ObjectKind::Array { length, .. } => {
                self.record_properties(source, self.records.len(), Some(*length))?
            }
            ObjectKind::ArrayBuffer(_) => self.encode_buffer(source)?,
            ObjectKind::TypedArray(_) | ObjectKind::DataView(_) => {
                let (buffer, layout) = self.vm.clone_view_layout(source)?;
                PacketRecord::View {
                    buffer: self.visit_record(buffer)?,
                    layout,
                }
            }
            _ => {
                return Err(StructuredCloneError::DataClone(
                    "Value is not cloneable across VMs",
                ));
            }
        };
        if self.records.len() == self.records.capacity() {
            let extra = self.records.len().max(4);
            self.budget.reserve_vec(self.vm, &mut self.records, extra)?;
        }
        self.vm.reserve_allocation(96)?;
        let target = self.records.len();
        self.seen.insert(source, target);
        self.records.push(record);
        Ok(target)
    }

    fn encode_buffer(&mut self, source: ObjectId) -> CloneResult<PacketRecord> {
        self.vm.consume_work(lookup_work(self.transfers.len()))?;
        if self.transfers.contains(&source) {
            return Ok(PacketRecord::ArrayBuffer {
                bytes: Vec::new(),
                maximum: None,
            });
        }
        let (length, maximum) = self.vm.clone_buffer_layout(source)?;
        self.budget.check(backing_bytes(length))?;
        let bytes = self
            .vm
            .copy_buffer_source(&Value::Object(source))?
            .expect("validated ArrayBuffer backing store");
        self.budget
            .reserve(self.vm, backing_bytes(bytes.capacity()))?;
        Ok(PacketRecord::ArrayBuffer { bytes, maximum })
    }

    fn record_properties(
        &mut self,
        source: ObjectId,
        target: usize,
        array_length: Option<u32>,
    ) -> Result<PacketRecord, JsError> {
        let keys = self.vm.enumerable_own_keys(Value::Object(source))?;
        let mut properties = Vec::new();
        self.budget
            .reserve_vec(self.vm, &mut properties, keys.len())?;
        if !keys.is_empty() {
            push_frame(
                self.vm,
                &mut self.frames,
                Frame {
                    source,
                    target,
                    keys: keys.into_iter(),
                },
            )?;
        }
        Ok(PacketRecord::Properties {
            array_length,
            properties,
        })
    }
}

fn decode(vm: &mut Vm, mut packet: StructuredClonePacket) -> Result<Value, JsError> {
    // Allocate all targets before defining properties so properties can point
    // forward or form cycles. Each view's backing precedes its record.
    let mut objects: Vec<Value> = Vec::new();
    vm.reserve_allocation(packet.records.len().saturating_mul(size_of::<Value>()))?;
    objects
        .try_reserve_exact(packet.records.len())
        .map_err(|_| temporary_allocation_error())?;
    for record in &mut packet.records {
        vm.tick()?;
        let object = match record {
            PacketRecord::Properties { array_length, .. } => new_record(vm, *array_length)?,
            PacketRecord::ArrayBuffer { bytes, maximum } => {
                vm.materialize_clone_buffer(std::mem::take(bytes), *maximum)?
            }
            PacketRecord::View { buffer, layout } => {
                let buffer = objects[*buffer]
                    .as_object()
                    .expect("serialized view backing");
                vm.materialize_clone_view(*layout, buffer)?
            }
        };
        objects.push(object);
    }
    for (object, record) in objects.iter().zip(packet.records) {
        if let PacketRecord::Properties { properties, .. } = record {
            for PacketProperty { key, value } in properties {
                let key = decode_string(vm, key)?;
                let value = decode_value(vm, value, &objects)?;
                vm.define_property(object.clone(), key, Property::data(value))?;
            }
        }
    }
    decode_value(vm, packet.root, &objects)
}

fn decode_value(vm: &mut Vm, value: PacketValue, objects: &[Value]) -> Result<Value, JsError> {
    vm.tick()?;
    let value = match value {
        PacketValue::Undefined => Value::Undefined,
        PacketValue::Null => Value::Null,
        PacketValue::Bool(value) => Value::Bool(value),
        PacketValue::Number(value) => Value::Number(value),
        PacketValue::String(units) => Value::String(decode_string(vm, units)?),
        PacketValue::BigInt(value) => {
            let bytes = integer_bytes(value.bits());
            vm.consume_work(bytes.div_ceil(8))?;
            vm.ensure_allocation(bytes + 192)?;
            Value::BigInt(JsBigInt::from_integer(value)?)
        }
        PacketValue::Record(index) => objects[index].clone(),
    };
    vm.account_value(value)
}

fn decode_string(vm: &mut Vm, units: Vec<u16>) -> Result<JsString, JsError> {
    vm.ensure_string_allocation(units.len())?;
    vm.consume_work(units.len().div_ceil(32))?;
    Ok(JsString::from_utf16(units))
}

fn integer_bytes(bits: u64) -> usize {
    // Account for the owned limb vector and rounding its capacity to whole words.
    ALLOCATION_OVERHEAD + bits.div_ceil(64) as usize * 8
}

fn backing_bytes(capacity: usize) -> usize {
    if capacity == 0 {
        0
    } else {
        capacity.saturating_add(ALLOCATION_OVERHEAD)
    }
}

fn lookup_work(entries: usize) -> usize {
    entries.max(1).ilog2() as usize
}
