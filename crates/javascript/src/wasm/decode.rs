//! Decoder and validator for the straight-line i32 WebAssembly subset.
use crate::{ErrorKind, JsError, Vm};
use std::{collections::HashSet, mem::size_of, ops::Range};

pub(super) const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_PARAMS: usize = 64;
const MAX_LOCALS: usize = 4096;
const MAX_STACK: usize = 4096;
const MAX_NAME_BYTES: usize = 4096;

#[derive(Clone, Copy)]
pub(super) struct Signature {
    pub(super) params: usize,
    pub(super) result: bool,
}

pub(super) struct Import {
    pub(super) module: Range<usize>,
    pub(super) field: Range<usize>,
    pub(super) signature: Signature,
}

pub(super) struct Export {
    pub(super) name: Range<usize>,
    pub(super) function: usize,
}

pub(super) struct Function {
    pub(super) signature: Signature,
    pub(super) locals: usize,
    pub(super) stack: usize,
    pub(super) code: Vec<Op>,
}

pub(super) enum Op {
    LocalGet(usize),
    LocalSet(usize),
    LocalTee(usize),
    Const(i32),
    CallImport(usize),
    Drop,
    Add,
    Sub,
    Mul,
}

pub(crate) struct Module {
    source: Vec<u8>,
    pub(super) imports: Vec<Import>,
    pub(super) functions: Vec<Function>,
    pub(super) exports: Vec<Export>,
}

impl Module {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.source.capacity()
            + self.imports.capacity() * size_of::<Import>()
            + self.functions.capacity() * size_of::<Function>()
            + self.exports.capacity() * size_of::<Export>()
            + self
                .functions
                .iter()
                .map(|function| function.code.capacity() * size_of::<Op>())
                .sum::<usize>()
            + size_of::<Self>()
    }

    pub(super) fn name(&self, range: &Range<usize>) -> &str {
        std::str::from_utf8(&self.source[range.clone()]).expect("validated WebAssembly name")
    }

    pub(super) fn signature(&self, function: usize) -> Signature {
        if function < self.imports.len() {
            self.imports[function].signature
        } else {
            self.functions[function - self.imports.len()].signature
        }
    }
}

pub(super) fn compile_error(message: &'static str) -> JsError {
    JsError::new(ErrorKind::CompileError, message)
}

pub(super) fn allocation_error() -> JsError {
    JsError::new(ErrorKind::HeapLimit, "WebAssembly allocation failed")
}

struct Reader<'a> {
    source: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn read(&mut self, len: usize) -> Result<&'a [u8], JsError> {
        let end = self
            .position
            .checked_add(len)
            .filter(|end| *end <= self.end)
            .ok_or_else(|| compile_error("Unexpected end of WebAssembly section"))?;
        let bytes = &self.source[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, JsError> {
        Ok(self.read(1)?[0])
    }

    fn integer(&mut self, signed: bool) -> Result<u32, JsError> {
        let mut value = 0;
        for shift in (0..32).step_by(7) {
            let byte = self.byte()?;
            if shift == 28 {
                let high = if signed && byte & 8 != 0 { 0x70 } else { 0 };
                if byte & 0xf0 != high {
                    return Err(compile_error("Invalid WebAssembly integer encoding"));
                }
            }
            value |= u32::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                if signed && shift < 28 && byte & 0x40 != 0 {
                    value |= u32::MAX << (shift + 7);
                }
                return Ok(value);
            }
        }
        unreachable!("fifth byte must terminate WebAssembly integer")
    }

    fn unsigned(&mut self) -> Result<usize, JsError> {
        self.integer(false).map(|number| number as usize)
    }

    fn count(&mut self, limit: usize) -> Result<usize, JsError> {
        let count = self.unsigned()?;
        if count > limit {
            return Err(compile_error(
                "WebAssembly vector exceeds the supported limit",
            ));
        }
        Ok(count)
    }

    fn subsection(&mut self) -> Result<Self, JsError> {
        let len = self.unsigned()?;
        let position = self.position;
        self.read(len)?;
        Ok(Self {
            source: self.source,
            position,
            end: self.position,
        })
    }

    fn name(&mut self) -> Result<Range<usize>, JsError> {
        let length = self.count(MAX_NAME_BYTES)?;
        let start = self.position;
        std::str::from_utf8(self.read(length)?)
            .map_err(|_| compile_error("WebAssembly name is not UTF-8"))?;
        Ok(start..self.position)
    }

    fn finished(&self) -> bool {
        self.position == self.end
    }

    fn require_finished(&self) -> Result<(), JsError> {
        if !self.finished() {
            return Err(compile_error("Trailing bytes in WebAssembly section"));
        }
        Ok(())
    }

    fn i32_types(&mut self, count: usize) -> Result<(), JsError> {
        if !self.read(count)?.iter().all(|byte| *byte == 0x7f) {
            return Err(compile_error(
                "Only i32 WebAssembly value types are supported",
            ));
        }
        Ok(())
    }
}

struct Allocation<'a> {
    vm: &'a mut Vm,
    held_bytes: usize,
}

impl Allocation<'_> {
    fn check(&mut self, bytes: usize) -> Result<(), JsError> {
        self.held_bytes = self.held_bytes.saturating_add(bytes);
        self.vm.ensure_allocation(self.held_bytes)
    }

    fn vector<T>(&mut self, len: usize) -> Result<Vec<T>, JsError> {
        self.check(len.saturating_mul(size_of::<T>()))?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(len)
            .map_err(|_| allocation_error())?;
        Ok(result)
    }
}

fn signature(signatures: &[Signature], index: usize) -> Result<Signature, JsError> {
    signatures
        .get(index)
        .copied()
        .ok_or_else(|| compile_error("Unknown WebAssembly function type"))
}

fn read_types(
    reader: &mut Reader<'_>,
    alloc: &mut Allocation<'_>,
) -> Result<Vec<Signature>, JsError> {
    let count = reader.count(MAX_ENTRIES)?;
    let mut signatures = alloc.vector(count)?;
    for _ in 0..count {
        if reader.byte()? != 0x60 {
            return Err(compile_error("Expected a WebAssembly function type"));
        }
        let params = reader.count(MAX_PARAMS)?;
        reader.i32_types(params)?;
        let results = reader.count(1)?;
        reader.i32_types(results)?;
        signatures.push(Signature {
            params,
            result: results == 1,
        });
    }
    Ok(signatures)
}

fn read_imports(
    reader: &mut Reader<'_>,
    alloc: &mut Allocation<'_>,
    signatures: &[Signature],
) -> Result<Vec<Import>, JsError> {
    let count = reader.count(MAX_ENTRIES)?;
    let mut imports = alloc.vector(count)?;
    for _ in 0..count {
        let module = reader.name()?;
        let field = reader.name()?;
        if reader.byte()? != 0 {
            return Err(compile_error(
                "Only WebAssembly function imports are supported",
            ));
        }
        imports.push(Import {
            module,
            field,
            signature: signature(signatures, reader.unsigned()?)?,
        });
    }
    Ok(imports)
}

fn read_exports(
    reader: &mut Reader<'_>,
    alloc: &mut Allocation<'_>,
    function_count: usize,
) -> Result<Vec<Export>, JsError> {
    let count = reader.count(MAX_ENTRIES)?;
    let mut exports = alloc.vector(count)?;
    let mut names = HashSet::new();
    alloc.check(count * 64)?;
    names.try_reserve(count).map_err(|_| allocation_error())?;
    for _ in 0..count {
        let name = reader.name()?;
        if !names.insert(&reader.source[name.clone()]) {
            return Err(compile_error("Duplicate WebAssembly export name"));
        }
        if reader.byte()? != 0 {
            return Err(compile_error(
                "Only WebAssembly function exports are supported",
            ));
        }
        let function = reader.unsigned()?;
        if function >= function_count {
            return Err(compile_error("Unknown exported WebAssembly function"));
        }
        exports.push(Export { name, function });
    }
    Ok(exports)
}

fn read_body(
    mut body: Reader<'_>,
    alloc: &mut Allocation<'_>,
    signature: Signature,
    imports: &[Import],
) -> Result<Function, JsError> {
    let groups = body.count(MAX_LOCALS)?;
    let mut locals = signature.params;
    for _ in 0..groups {
        locals += body.count(MAX_LOCALS)?;
        if locals > MAX_LOCALS {
            return Err(compile_error("Too many WebAssembly locals"));
        }
        body.i32_types(1)?;
    }
    // Each decoded instruction occupies at least one input byte.
    let mut code = alloc.vector(body.end - body.position)?;
    let mut depth = 0usize;
    let mut stack = 0;
    loop {
        let opcode = body.byte()?;
        let (op, consumed, produced) = match opcode {
            0x0b => break,
            0x10 => {
                let index = body.unsigned()?;
                let import = imports.get(index).ok_or_else(|| {
                    compile_error("WebAssembly calls to defined functions are not supported")
                })?;
                (
                    Op::CallImport(index),
                    import.signature.params,
                    usize::from(import.signature.result),
                )
            }
            0x1a => (Op::Drop, 1, 0),
            0x20..=0x22 => {
                let index = body.unsigned()?;
                if index >= locals {
                    return Err(compile_error("Unknown WebAssembly local"));
                }
                match opcode {
                    0x20 => (Op::LocalGet(index), 0, 1),
                    0x21 => (Op::LocalSet(index), 1, 0),
                    _ => (Op::LocalTee(index), 1, 1),
                }
            }
            0x41 => (Op::Const(body.integer(true)? as i32), 0, 1),
            0x6a => (Op::Add, 2, 1),
            0x6b => (Op::Sub, 2, 1),
            0x6c => (Op::Mul, 2, 1),
            _ => return Err(compile_error("Unsupported WebAssembly instruction")),
        };
        depth = depth
            .checked_sub(consumed)
            .ok_or_else(|| compile_error("WebAssembly operand stack underflow"))?
            + produced;
        if depth > MAX_STACK {
            return Err(compile_error(
                "WebAssembly operand stack exceeds the supported limit",
            ));
        }
        stack = stack.max(depth);
        code.push(op);
    }
    body.require_finished()?;
    if depth != usize::from(signature.result) {
        return Err(compile_error(
            "WebAssembly function result does not match its type",
        ));
    }
    Ok(Function {
        signature,
        locals,
        stack,
        code,
    })
}

pub(super) fn decode(vm: &mut Vm, source: Vec<u8>) -> Result<Module, JsError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(compile_error(
            "WebAssembly source exceeds the supported byte limit",
        ));
    }
    vm.charge_work(source.len() / 4 + 1)?;
    let mut reader = Reader {
        source: &source,
        position: 0,
        end: source.len(),
    };
    if reader.read(8)? != b"\0asm\x01\0\0\0" {
        return Err(compile_error("Invalid WebAssembly header"));
    }
    let mut alloc = Allocation {
        vm,
        held_bytes: source.capacity(),
    };
    let mut signatures = Vec::new();
    let mut declared = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut functions = Vec::new();
    let mut previous = 0;
    while !reader.finished() {
        let id = reader.byte()?;
        let mut section = reader.subsection()?;
        if id == 0 {
            section.name()?;
            continue;
        }
        if id <= previous {
            return Err(compile_error(
                "WebAssembly sections are duplicated or out of order",
            ));
        }
        previous = id;
        match id {
            1 => signatures = read_types(&mut section, &mut alloc)?,
            2 => imports = read_imports(&mut section, &mut alloc, &signatures)?,
            3 => {
                let count = section.count(MAX_ENTRIES)?;
                declared = alloc.vector(count)?;
                for _ in 0..count {
                    declared.push(signature(&signatures, section.unsigned()?)?);
                }
            }
            7 => exports = read_exports(&mut section, &mut alloc, imports.len() + declared.len())?,
            10 => {
                let count = section.count(MAX_ENTRIES)?;
                if count != declared.len() {
                    return Err(compile_error("WebAssembly function and code counts differ"));
                }
                functions = alloc.vector(count)?;
                for signature in &declared {
                    functions.push(read_body(
                        section.subsection()?,
                        &mut alloc,
                        *signature,
                        &imports,
                    )?);
                }
            }
            _ => return Err(compile_error("Unsupported WebAssembly section")),
        }
        section.require_finished()?;
    }
    if declared.len() != functions.len() {
        return Err(compile_error("Missing WebAssembly code section"));
    }
    Ok(Module {
        source,
        imports,
        functions,
        exports,
    })
}
