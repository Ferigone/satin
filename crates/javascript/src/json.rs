//! JSON's independent UTF-16 grammar and serializer. JSON is never evaluated as
//! JavaScript or delegated to a Unicode-scalar/serde representation.
use crate::{
    ErrorKind, JsError, JsString, ObjectId, Value, Vm,
    builtins::method,
    heap::{ObjectKind, Property},
    vm::{number_string, same_value},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Record {
    value: Value,
    start: usize,
    end: usize,
}
type Records = BTreeMap<(ObjectId, JsString), Record>;
struct Parser<'a> {
    source: &'a [u16],
    pos: usize,
    records: Option<Records>,
    punctuation_credit: usize,
}
impl Vm {
    /// Intrinsic JSON parser, independent of the mutable global JSON object.
    pub fn parse_json(&mut self, source: &JsString) -> Result<Value, JsError> {
        self.with_task(|vm| parse(vm, source, Value::Undefined))
    }
    /// None represents JSON.stringify's undefined result for an unsupported root.
    pub fn stringify_json(&mut self, value: Value) -> Result<Option<JsString>, JsError> {
        self.with_task(|vm| stringify(vm, value, Value::Undefined, Value::Undefined))
    }
    fn enter_json(&mut self) -> Result<(), JsError> {
        if self.json_depth >= 32 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "JSON nesting/reentrancy budget exhausted",
            ));
        }
        self.json_depth += 1;
        Ok(())
    }
}
fn syntax(pos: usize, message: &str) -> JsError {
    JsError::new(
        ErrorKind::SyntaxError,
        format!("Invalid JSON at UTF-16 offset {pos}: {message}"),
    )
}
impl Parser<'_> {
    fn peek(&self) -> Option<u16> {
        self.source.get(self.pos).copied()
    }
    fn bump(&mut self, vm: &mut Vm) -> Result<Option<u16>, JsError> {
        // These are grammar units in a native scan. Parsing never calls user
        // code until the complete value has been built; revivers pay script
        // instruction costs when they subsequently run.
        if self.punctuation_credit == 0 {
            vm.tick()?;
            self.punctuation_credit = vm.limits().buffer_units_per_instruction;
        }
        self.punctuation_credit -= 1;
        let value = self.peek();
        if value.is_some() {
            self.pos += 1;
        }
        Ok(value)
    }
    fn whitespace(&mut self, vm: &mut Vm) -> Result<(), JsError> {
        while matches!(self.peek(), Some(9 | 10 | 13 | 32)) {
            let end = self.source.len().min(self.pos.saturating_add(4096));
            let batch = &self.source[self.pos..end];
            let n = batch
                .iter()
                .position(|c| !matches!(c, 9 | 10 | 13 | 32))
                .unwrap_or(batch.len());
            vm.charge_buffer_work(n)?;
            self.pos += n;
        }
        Ok(())
    }
    fn expect(&mut self, vm: &mut Vm, unit: u16) -> Result<(), JsError> {
        if self.bump(vm)? == Some(unit) {
            Ok(())
        } else {
            Err(syntax(self.pos, "unexpected character"))
        }
    }
    fn literal(&mut self, vm: &mut Vm, text: &[u8], value: Value) -> Result<Value, JsError> {
        for ch in text {
            self.expect(vm, *ch as u16)?;
        }
        Ok(value)
    }
    fn string_unit(&mut self, vm: &mut Vm) -> Result<u16, JsError> {
        let unit = self
            .bump(vm)?
            .ok_or_else(|| syntax(self.pos, "unterminated string"))?;
        if unit < 32 {
            return Err(syntax(self.pos, "unescaped control character"));
        }
        Ok(unit)
    }
    fn string_escape(&mut self, vm: &mut Vm) -> Result<u16, JsError> {
        let escaped = self
            .bump(vm)?
            .ok_or_else(|| syntax(self.pos, "unterminated escape"))?;
        let unit = match escaped {
            34 => 34,
            47 => 47,
            92 => 92,
            98 => 8,
            102 => 12,
            110 => 10,
            114 => 13,
            116 => 9,
            117 => {
                let mut value = 0u16;
                for _ in 0..4 {
                    let digit = self
                        .bump(vm)?
                        .and_then(|u| char::from_u32(u as u32))
                        .and_then(|c| c.to_digit(16))
                        .ok_or_else(|| syntax(self.pos, "invalid Unicode escape"))?;
                    value = value * 16 + digit as u16;
                }
                value
            }
            _ => return Err(syntax(self.pos, "invalid string escape")),
        };
        Ok(unit)
    }
    /// Scan literal UTF-16 in bounded batches. Escapes and control characters
    /// still go through the JSON grammar, including isolated surrogate units.
    fn string_run(&mut self, vm: &mut Vm, decoded_len: Option<usize>) -> Result<(), JsError> {
        let start = self.pos;
        while self.pos < self.source.len() {
            let end = self.source.len().min(self.pos.saturating_add(4096));
            let batch = &self.source[self.pos..end];
            let n = batch
                .iter()
                .position(|&unit| matches!(unit, 0..=31 | 34 | 92))
                .unwrap_or(batch.len());
            vm.charge_buffer_work(n)?;
            self.pos += n;
            if decoded_len.is_some() || vm.limits().buffer_units_per_instruction == 1 {
                vm.ensure_string_allocation(
                    decoded_len.unwrap_or(0).saturating_add(self.pos - start),
                )?;
            } else {
                // An unescaped token is copied directly into final UTF-16
                // storage. There is no growing Vec or second result allocation.
                let units = self.pos - start;
                vm.check_string_length(units)?;
                vm.ensure_allocation(units.saturating_mul(2).saturating_add(128))?;
            }
            if n != batch.len() {
                break;
            }
        }
        Ok(())
    }
    fn string(&mut self, vm: &mut Vm) -> Result<JsString, JsError> {
        self.expect(vm, 34)?;
        let mut decoded: Option<Vec<u16>> = None;
        loop {
            let start = self.pos;
            let decoded_len = decoded.as_ref().map_or(0, Vec::len);
            self.string_run(vm, decoded.as_ref().map(Vec::len))?;
            let end = self.pos;
            match self.string_unit(vm)? {
                34 => {
                    let run = &self.source[start..end];
                    let value = if let Some(mut decoded) = decoded {
                        decoded.extend_from_slice(run);
                        vm.finish_buffer_string(decoded)?
                    } else {
                        vm.charge_buffer_work(run.len())?;
                        vm.checked_string(JsString::from_utf16_slice(run))?
                    };
                    return Ok(value.as_string().unwrap().clone());
                }
                92 => {
                    let unit = self.string_escape(vm)?;
                    let run = &self.source[start..end];
                    vm.ensure_string_allocation(decoded_len.saturating_add(run.len() + 1))?;
                    let out = decoded.get_or_insert_with(Vec::new);
                    out.extend_from_slice(run);
                    out.push(unit);
                }
                _ => unreachable!("string_run stops at a quote, escape, or invalid control"),
            }
        }
    }
    fn number(&mut self, vm: &mut Vm) -> Result<Value, JsError> {
        let start = self.pos;
        if self.peek() == Some(45) {
            self.bump(vm)?;
        }
        match self.peek() {
            Some(48) => {
                self.bump(vm)?;
                if self.peek().is_some_and(|u| (48..=57).contains(&u)) {
                    return Err(syntax(self.pos, "leading zero in number"));
                }
            }
            Some(49..=57) => {
                while self.peek().is_some_and(|u| (48..=57).contains(&u)) {
                    self.bump(vm)?;
                }
            }
            _ => return Err(syntax(self.pos, "missing number digits")),
        }
        if self.peek() == Some(46) {
            self.bump(vm)?;
            let before = self.pos;
            while self.peek().is_some_and(|u| (48..=57).contains(&u)) {
                self.bump(vm)?;
            }
            if before == self.pos {
                return Err(syntax(self.pos, "missing fractional digits"));
            }
        }
        if matches!(self.peek(), Some(69 | 101)) {
            self.bump(vm)?;
            if matches!(self.peek(), Some(43 | 45)) {
                self.bump(vm)?;
            }
            let before = self.pos;
            while self.peek().is_some_and(|u| (48..=57).contains(&u)) {
                self.bump(vm)?;
            }
            if before == self.pos {
                return Err(syntax(self.pos, "missing exponent digits"));
            }
        }
        vm.reserve_allocation((self.pos - start).saturating_mul(2).saturating_add(64))?;
        let text: String = self.source[start..self.pos]
            .iter()
            .map(|u| *u as u8 as char)
            .collect();
        let number = text
            .parse::<f64>()
            .map_err(|_| syntax(start, "invalid number"))?;
        Ok(Value::Number(number))
    }
    fn remember(
        &mut self,
        vm: &mut Vm,
        object: ObjectId,
        key: JsString,
        record: Record,
    ) -> Result<(), JsError> {
        if let Some(records) = &mut self.records {
            vm.reserve_allocation(192)?;
            records.insert((object, key), record);
        }
        Ok(())
    }
    fn value(&mut self, vm: &mut Vm) -> Result<Record, JsError> {
        vm.enter_json()?;
        let result = self.value_inner(vm);
        vm.json_depth -= 1;
        result
    }
    fn value_inner(&mut self, vm: &mut Vm) -> Result<Record, JsError> {
        self.whitespace(vm)?;
        let start = self.pos;
        let value = match self.peek() {
            Some(34) => Value::String(self.string(vm)?),
            Some(116) => self.literal(vm, b"true", Value::Bool(true))?,
            Some(102) => self.literal(vm, b"false", Value::Bool(false))?,
            Some(110) => self.literal(vm, b"null", Value::Null)?,
            Some(45 | 48..=57) => self.number(vm)?,
            Some(123) => {
                self.bump(vm)?;
                vm.ensure_allocation(256)?;
                let object = vm.new_object();
                self.whitespace(vm)?;
                if self.peek() == Some(125) {
                    self.bump(vm)?;
                } else {
                    loop {
                        if self.peek() != Some(34) {
                            return Err(syntax(self.pos, "object key must be a quoted string"));
                        }
                        let key = self.string(vm)?;
                        self.whitespace(vm)?;
                        self.expect(vm, 58)?;
                        let child = self.value(vm)?;
                        vm.define_property(
                            object.clone(),
                            key.clone(),
                            Property::data(child.value.clone()),
                        )?;
                        self.remember(vm, object.as_object().unwrap(), key, child)?;
                        self.whitespace(vm)?;
                        if self.peek() == Some(125) {
                            self.bump(vm)?;
                            break;
                        }
                        self.expect(vm, 44)?;
                        self.whitespace(vm)?;
                    }
                }
                object
            }
            Some(91) => {
                self.bump(vm)?;
                let array = vm.try_new_array(&[])?;
                self.whitespace(vm)?;
                let mut index = 0usize;
                if self.peek() == Some(93) {
                    self.bump(vm)?;
                } else {
                    loop {
                        if index >= vm.limits.max_array_length as usize {
                            return Err(JsError::new(
                                ErrorKind::ExecutionLimit,
                                "JSON array length budget exhausted",
                            ));
                        }
                        let child = self.value(vm)?;
                        let key = JsString::from(index.to_string());
                        vm.define_property(
                            array.clone(),
                            key.clone(),
                            Property::data(child.value.clone()),
                        )?;
                        self.remember(vm, array.as_object().unwrap(), key, child)?;
                        index += 1;
                        self.whitespace(vm)?;
                        if self.peek() == Some(93) {
                            self.bump(vm)?;
                            break;
                        }
                        self.expect(vm, 44)?;
                        self.whitespace(vm)?;
                    }
                }
                array
            }
            _ => return Err(syntax(self.pos, "expected a JSON value")),
        };
        Ok(Record {
            value,
            start,
            end: self.pos,
        })
    }
}
fn parse(vm: &mut Vm, source: &JsString, reviver: Value) -> Result<Value, JsError> {
    vm.account_value(Value::String(source.clone()))?;
    let revive = vm.is_callable(&reviver);
    let mut parser = Parser {
        source: source.as_utf16(),
        pos: 0,
        records: if revive { Some(BTreeMap::new()) } else { None },
        punctuation_credit: 0,
    };
    let root = parser.value(vm)?;
    parser.whitespace(vm)?;
    if parser.pos != source.len() {
        return Err(syntax(parser.pos, "unexpected trailing text"));
    }
    if !revive {
        return Ok(root.value);
    }
    let holder = vm.new_object();
    vm.define_property(
        holder.clone(),
        JsString::default(),
        Property::data(root.value.clone()),
    )?;
    internalize(
        vm,
        &reviver,
        holder,
        &JsString::default(),
        Some(&root),
        parser.records.as_ref().unwrap(),
        source,
    )
}
fn raw_json(vm: &mut Vm, value: &Value) -> Result<Value, JsError> {
    let source = vm.to_string(value)?;
    let units = source.as_utf16();
    // Only a primitive JSON value is allowed, with no surrounding whitespace.
    if !matches!(units.first(), Some(0x61..=0x7a | 0x30..=0x39 | 0x22 | 0x2d)) {
        return Err(syntax(0, "invalid start of raw JSON"));
    }
    if !matches!(units.last(), Some(0x61..=0x7a | 0x30..=0x39 | 0x22)) {
        return Err(syntax(units.len() - 1, "invalid end of raw JSON"));
    }
    parse(vm, &source, Value::Undefined)?;

    vm.ensure_allocation(256)?;
    let raw = vm.new_object_with_prototype(Value::Null)?;
    vm.define_data_property(
        raw.clone(),
        "rawJSON",
        Value::String(source.clone()),
        false,
        true,
        false,
    )?;
    // The object is fresh and unexposed: install the internal brand and finish
    // freezing it. Keep the validated original spelling, not the parsed value.
    let object = vm.heap.object_mut(raw.as_object().unwrap())?;
    object.kind = ObjectKind::RawJson(source);
    object.extensible = false;
    Ok(raw)
}
fn internalize(
    vm: &mut Vm,
    reviver: &Value,
    holder: Value,
    key: &JsString,
    record: Option<&Record>,
    records: &Records,
    source: &JsString,
) -> Result<Value, JsError> {
    vm.enter_json()?;
    let result = (|| {
        vm.tick()?;
        let value = vm.get(holder.clone(), key)?;
        let matches = record.is_some_and(|record| same_value(&record.value, &value));
        if let Some(id) = value.as_object() {
            let array = vm.is_array_exotic(&Value::Object(id))?;
            let keys = if array {
                None
            } else {
                Some(vm.own_keys(value.clone(), true)?)
            };
            if array {
                let length = vm.get_property(value.clone(), "length")?;
                let length = vm.to_number(&length)?.max(0.0).floor();
                if length > vm.limits.max_array_length as f64 {
                    return Err(JsError::new(
                        ErrorKind::ExecutionLimit,
                        "JSON reviver array length budget exhausted",
                    ));
                }
                for index in 0..length as usize {
                    vm.tick()?;
                    let key = JsString::from(index.to_string());
                    revive_child(
                        vm,
                        reviver,
                        value.clone(),
                        key,
                        if matches { Some(id) } else { None },
                        records,
                        source,
                    )?;
                }
            } else {
                for key in keys.unwrap() {
                    vm.tick()?;
                    revive_child(
                        vm,
                        reviver,
                        value.clone(),
                        key,
                        if matches { Some(id) } else { None },
                        records,
                        source,
                    )?;
                }
            }
        }
        let context = vm.new_object();
        if matches && value.as_object().is_none() {
            let record = record.unwrap();
            let len = record.end - record.start;
            vm.ensure_string_allocation(len)?;
            let text = vm.copy_string(&source.as_utf16()[record.start..record.end])?;
            vm.define_property(context.clone(), "source".into(), Property::data(text))?;
        }
        vm.invoke(
            reviver.clone(),
            holder,
            &[Value::String(key.clone()), value, context],
        )
    })();
    vm.json_depth -= 1;
    result
}
fn revive_child(
    vm: &mut Vm,
    reviver: &Value,
    holder: Value,
    key: JsString,
    original: Option<ObjectId>,
    records: &Records,
    source: &JsString,
) -> Result<(), JsError> {
    let record = original.and_then(|id| records.get(&(id, key.clone())));
    let replacement = internalize(vm, reviver, holder.clone(), &key, record, records, source)?;
    if replacement.is_undefined() {
        vm.delete(holder, &key, false)?;
    } else if let Some(id) = holder.as_object()
        && matches!(vm.heap.object(id)?.kind, ObjectKind::TypedArray(_))
        && let Some(index) = crate::binary::numeric_index(&key)
    {
        // CreateDataProperty supplies all three flags as true. A missing
        // integer-indexed element rejects silently, but conversion on an
        // existing element can throw and must abort the reviver walk.
        if vm.typed_element_exists(id, index)? {
            vm.typed_index_set(id, index, &replacement)?;
        }
    } else if let Err(error) = vm.define_property(holder, key, Property::data(replacement))
        && error.uncatchable()
    {
        return Err(error);
    }
    Ok(())
}

enum Placement {
    Root,
    Array { first: bool },
    Object { first: bool },
}
struct Serializer {
    out: Vec<u16>,
    ancestors: Vec<ObjectId>,
    replacer: Option<Value>,
    properties: Option<Vec<JsString>>,
    gap: JsString,
}
impl Serializer {
    fn emit(&mut self, vm: &mut Vm, units: &[u16]) -> Result<(), JsError> {
        vm.consume_work(units.len())?;
        vm.ensure_string_allocation(self.out.len().saturating_add(units.len()))?;
        self.out.extend_from_slice(units);
        Ok(())
    }
    fn ascii(&mut self, vm: &mut Vm, text: &str) -> Result<(), JsError> {
        let units: Vec<_> = text.bytes().map(|b| b as u16).collect();
        self.emit(vm, &units)
    }
    fn indent(&mut self, vm: &mut Vm, level: usize) -> Result<(), JsError> {
        if !self.gap.is_empty() {
            self.emit(vm, &[10])?;
            let gap = self.gap.clone();
            for _ in 0..level {
                self.emit(vm, gap.as_utf16())?;
            }
        }
        Ok(())
    }
    fn quote(&mut self, vm: &mut Vm, string: &JsString) -> Result<(), JsError> {
        self.emit(vm, &[34])?;
        let units = string.as_utf16();
        let mut index = 0;
        while index < units.len() {
            vm.tick()?;
            let unit = units[index];
            match unit {
                34 => self.emit(vm, &[92, 34])?,
                92 => self.emit(vm, &[92, 92])?,
                8 => self.emit(vm, &[92, 98])?,
                9 => self.emit(vm, &[92, 116])?,
                10 => self.emit(vm, &[92, 110])?,
                12 => self.emit(vm, &[92, 102])?,
                13 => self.emit(vm, &[92, 114])?,
                0..=31 | 0xd800..=0xdfff => {
                    if (0xd800..=0xdbff).contains(&unit)
                        && units
                            .get(index + 1)
                            .is_some_and(|u| (0xdc00..=0xdfff).contains(u))
                    {
                        self.emit(vm, &units[index..index + 2])?;
                        index += 1;
                    } else {
                        let hex = b"0123456789abcdef";
                        self.emit(
                            vm,
                            &[
                                92,
                                117,
                                hex[((unit >> 12) & 15) as usize] as u16,
                                hex[((unit >> 8) & 15) as usize] as u16,
                                hex[((unit >> 4) & 15) as usize] as u16,
                                hex[(unit & 15) as usize] as u16,
                            ],
                        )?;
                    }
                }
                _ => self.emit(vm, &[unit])?,
            }
            index += 1;
        }
        self.emit(vm, &[34])
    }
    fn prepare(&mut self, vm: &mut Vm, holder: Value, key: &JsString) -> Result<Value, JsError> {
        vm.consume_work(key.len())?;
        let mut value = vm.get(holder.clone(), key)?;
        if value.as_object().is_some() || matches!(value, Value::BigInt(_)) {
            let to_json = vm.get_property(value.clone(), "toJSON")?;
            if vm.is_callable(&to_json) {
                value = vm.invoke(to_json, value, &[Value::String(key.clone())])?;
            }
        }
        if let Some(replacer) = &self.replacer {
            value = vm.invoke(
                replacer.clone(),
                holder,
                &[Value::String(key.clone()), value],
            )?;
        }
        if let Some(id) = value.as_object() {
            let boxed = match &vm.heap.object(id)?.kind {
                ObjectKind::Boxed(v) => Some(v.clone()),
                _ => None,
            };
            if let Some(boxed) = boxed {
                value = match boxed {
                    Value::Number(_) => Value::Number(vm.to_number(&value)?),
                    Value::String(_) => Value::String(vm.to_string(&value)?),
                    Value::Bool(b) => Value::Bool(b),
                    Value::BigInt(integer) => Value::BigInt(integer),
                    _ => value,
                };
            }
        }
        Ok(value)
    }
    fn property(
        &mut self,
        vm: &mut Vm,
        holder: Value,
        key: &JsString,
        level: usize,
        placement: Placement,
    ) -> Result<bool, JsError> {
        vm.enter_json()?;
        let result = (|| {
            vm.tick()?;
            let value = self.prepare(vm, holder, key)?;
            let omitted =
                value.is_undefined() || matches!(value, Value::Symbol(_)) || vm.is_callable(&value);
            if omitted && !matches!(placement, Placement::Array { .. }) {
                return Ok(false);
            }
            match placement {
                Placement::Root => {}
                Placement::Array { first } | Placement::Object { first } => {
                    if !first {
                        self.emit(vm, &[44])?;
                    }
                    self.indent(vm, level)?;
                    if matches!(placement, Placement::Object { .. }) {
                        self.quote(vm, key)?;
                        self.emit(vm, &[58])?;
                        if !self.gap.is_empty() {
                            self.emit(vm, &[32])?;
                        }
                    }
                }
            }
            if omitted {
                self.ascii(vm, "null")?;
            } else {
                self.value(vm, value, level)?;
            }
            Ok(true)
        })();
        vm.json_depth -= 1;
        result
    }
    fn value(&mut self, vm: &mut Vm, value: Value, level: usize) -> Result<(), JsError> {
        match value {
            Value::Null => self.ascii(vm, "null"),
            Value::Bool(b) => self.ascii(vm, if b { "true" } else { "false" }),
            Value::Number(n) => {
                let text = if n.is_finite() {
                    number_string(n)
                } else {
                    "null".into()
                };
                self.ascii(vm, &text)
            }
            Value::String(string) => self.quote(vm, &string),
            Value::BigInt(_) => Err(JsError::type_error("Cannot stringify a BigInt value")),
            Value::Object(id) => {
                if let ObjectKind::RawJson(source) = &vm.heap.object(id)?.kind {
                    let source = source.clone();
                    return self.emit(vm, source.as_utf16());
                }
                if self.ancestors.contains(&id) {
                    return Err(JsError::type_error("Cannot stringify a cyclic object"));
                }
                self.ancestors.push(id);
                let result = self.object(vm, id, level);
                self.ancestors.pop();
                result
            }
            Value::Undefined | Value::Symbol(_) => Ok(()),
        }
    }
    fn object(&mut self, vm: &mut Vm, id: ObjectId, level: usize) -> Result<(), JsError> {
        let value = Value::Object(id);
        if vm.is_array_exotic(&Value::Object(id))? {
            let length = vm.get_property(value.clone(), "length")?;
            let length = vm.to_number(&length)?.max(0.0).floor();
            if length > vm.limits.max_array_length as f64 {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "JSON stringify array length budget exhausted",
                ));
            }
            let length = length as usize;
            self.emit(vm, &[91])?;
            for index in 0..length {
                let key = JsString::from(index.to_string());
                self.property(
                    vm,
                    value.clone(),
                    &key,
                    level + 1,
                    Placement::Array { first: index == 0 },
                )?;
            }
            if length > 0 {
                self.indent(vm, level)?;
            }
            self.emit(vm, &[93])
        } else {
            self.emit(vm, &[123])?;
            let keys = if let Some(keys) = &self.properties {
                vm.reserve_allocation(keys.len().saturating_mul(std::mem::size_of::<JsString>()))?;
                keys.clone()
            } else {
                vm.own_keys(value.clone(), true)?
            };
            let mut first = true;
            for key in keys {
                if self.property(
                    vm,
                    value.clone(),
                    &key,
                    level + 1,
                    Placement::Object { first },
                )? {
                    first = false;
                }
            }
            if !first {
                self.indent(vm, level)?;
            }
            self.emit(vm, &[125])
        }
    }
}
fn boxed_kind(vm: &Vm, value: &Value) -> Option<Value> {
    value
        .as_object()
        .and_then(|id| vm.heap.object(id).ok())
        .and_then(|object| {
            if let ObjectKind::Boxed(v) = &object.kind {
                Some(v.clone())
            } else {
                None
            }
        })
}
fn stringify(
    vm: &mut Vm,
    value: Value,
    replacer: Value,
    space: Value,
) -> Result<Option<JsString>, JsError> {
    let callback = vm.is_callable(&replacer).then_some(replacer.clone());
    let properties = if callback.is_none()
        && replacer.as_object().is_some_and(|id| {
            vm.heap
                .object(id)
                .is_ok_and(|o| matches!(o.kind, ObjectKind::Array { .. }))
        }) {
        let length = vm.get_property(replacer.clone(), "length")?;
        let length = vm.to_number(&length)?.max(0.0).floor();
        if length > vm.limits.max_array_length as f64 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "JSON replacer length budget exhausted",
            ));
        }
        let mut keys = Vec::new();
        let mut seen = BTreeSet::new();
        for index in 0..length as usize {
            vm.tick()?;
            let item = vm.get_property(replacer.clone(), &index.to_string())?;
            if matches!(item, Value::String(_) | Value::Number(_))
                || matches!(
                    boxed_kind(vm, &item),
                    Some(Value::String(_) | Value::Number(_))
                )
            {
                let key = vm.to_string(&item)?;
                vm.consume_work(key.len())?;
                vm.account_value(Value::String(key.clone()))?;
                if !seen.contains(&key) {
                    vm.reserve_allocation(128)?;
                    seen.insert(key.clone());
                    keys.push(key);
                }
            }
        }
        Some(keys)
    } else {
        None
    };
    let space = match boxed_kind(vm, &space) {
        Some(Value::Number(_)) => Value::Number(vm.to_number(&space)?),
        Some(Value::String(_)) => Value::String(vm.to_string(&space)?),
        _ => space,
    };
    let gap = match space {
        Value::Number(n) => JsString::from_utf16(vec![32; n.floor().clamp(0.0, 10.0) as usize]),
        Value::String(s) => JsString::from_utf16(s.as_utf16()[..s.len().min(10)].to_vec()),
        _ => JsString::default(),
    };
    vm.consume_work(gap.len().saturating_mul(2))?;
    let holder = vm.new_object();
    vm.define_property(holder.clone(), JsString::default(), Property::data(value))?;
    let mut serializer = Serializer {
        out: Vec::new(),
        ancestors: Vec::new(),
        replacer: callback,
        properties,
        gap,
    };
    if !serializer.property(vm, holder, &JsString::default(), 0, Placement::Root)? {
        return Ok(None);
    }
    let value = vm.finish_string(serializer.out)?;
    Ok(value.as_string().cloned())
}
pub(crate) fn install(vm: &mut Vm) {
    let json = vm.new_object();
    vm.define_data_property(vm.global(), "JSON", json.clone(), true, false, true)
        .unwrap();
    vm.define_data_property_key(
        json.clone(),
        vm.well_known_key("toStringTag"),
        "JSON".into(),
        false,
        false,
        true,
    )
    .unwrap();
    method(vm, json.clone(), "parse", 2, |vm, _, args| {
        let source = vm.to_string(&args.first().cloned().unwrap_or_default())?;
        parse(vm, &source, args.get(1).cloned().unwrap_or_default())
    });
    method(vm, json.clone(), "rawJSON", 1, |vm, _, args| {
        raw_json(vm, args.first().unwrap_or(&Value::Undefined))
    });
    method(vm, json.clone(), "isRawJSON", 1, |vm, _, args| {
        Ok(Value::Bool(
            args.first()
                .and_then(Value::as_object)
                .and_then(|id| vm.heap.object(id).ok())
                .is_some_and(|object| matches!(object.kind, ObjectKind::RawJson(_))),
        ))
    });
    method(vm, json, "stringify", 3, |vm, _, args| {
        let result = stringify(
            vm,
            args.first().cloned().unwrap_or_default(),
            args.get(1).cloned().unwrap_or_default(),
            args.get(2).cloned().unwrap_or_default(),
        )?;
        Ok(result.map(Value::String).unwrap_or_default())
    });
}
