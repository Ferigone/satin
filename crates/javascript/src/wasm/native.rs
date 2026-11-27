//! Validated core Wasm execution and the JavaScript boundary.
use super::{
    INSTANCE_EXPORTS, INSTANCE_IMPORTS, INSTANCE_MODULE, arg, class, tag, validate_import_object,
};
use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    builtins::method,
    heap::{ObjectKind, Property, PropertyKind},
    vm::to_i32,
};
use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    fmt,
    rc::Rc,
};
use wasmi::{
    CompilationMode, Config, Engine, Extern, ExternType, Func, FuncType, Nullable, ResumableCall,
    Store, Table, Val, ValType,
};

const FUEL_QUANTUM: u64 = 16_384;
// Wasmi's default metering charges seven units per byte of lazy translation.
const TRANSLATION_WORK_PER_BYTE: usize = 7;
const LINEAR_LIMIT: usize = 128 * 1024 * 1024;
const LINEAR_POOL_LIMIT: usize = 512 * 1024 * 1024;
const TABLE_LIMIT: usize = 32_768;
const CONTEXT: u32 = 0x5741_0001;
const MEMORY_BUFFER: u32 = 0x5741_0002;

#[derive(Default)]
pub(crate) struct Pool(Cell<usize>);

impl Pool {
    pub(crate) fn bytes(&self) -> usize {
        self.0.get()
    }
}

struct Resources {
    pool: Rc<Pool>,
    limit: usize,
    memory: usize,
    previous: usize,
}

impl Drop for Resources {
    fn drop(&mut self) {
        self.pool.0.set(self.pool.0.get() - self.memory);
    }
}

impl wasmi::ResourceLimiter for Resources {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::LimiterError> {
        let aggregate = self.pool.0.get();
        // A reallocation may briefly retain both the old and the new block.
        if desired > self.limit
            || maximum.is_some_and(|limit| desired > limit)
            || aggregate.saturating_add(desired) > LINEAR_POOL_LIMIT
        {
            return Ok(false);
        }
        self.previous = current;
        self.memory = desired;
        self.pool.0.set(aggregate - current + desired);
        Ok(true)
    }

    fn memory_grow_failed(
        &mut self,
        _: &wasmi::errors::MemoryError,
    ) -> Result<(), wasmi::errors::LimiterError> {
        self.pool
            .0
            .set(self.pool.0.get() - self.memory + self.previous);
        self.memory = self.previous;
        Ok(())
    }

    fn table_growing(
        &mut self,
        _: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmi::errors::LimiterError> {
        Ok(desired <= TABLE_LIMIT && maximum.is_none_or(|limit| desired <= limit))
    }

    fn instances(&self) -> usize {
        1
    }
    fn tables(&self) -> usize {
        1
    }
    fn memories(&self) -> usize {
        1
    }
}

type Execution = Rc<RefCell<Store<Resources>>>;

pub(crate) struct Module {
    module: wasmi::Module,
    estimated_bytes: usize,
}

impl Module {
    pub(super) fn compile(
        vm: &mut Vm,
        bytes: &[u8],
        mode: CompilationMode,
    ) -> Result<Self, JsError> {
        let estimated_bytes = bytes.len().saturating_mul(8);
        vm.ensure_allocation(estimated_bytes)?;
        vm.consume_work(bytes.len().div_ceil(16))?;
        if matches!(mode, CompilationMode::Eager) {
            vm.consume_work(bytes.len().saturating_mul(TRANSLATION_WORK_PER_BYTE))?;
        }
        let mut config = Config::default();
        // A start function can invoke JS before its instance has finished linking.
        config
            .consume_fuel(true)
            .allow_start_fn(false)
            .wasm_multi_memory(false)
            // Translation of the entry function happens before Wasmi can create
            // a resumable call. Compile executable modules outside execution slices.
            .compilation_mode(mode);
        let module = wasmi::Module::new(&Engine::new(&config), bytes)
            .map_err(|error| JsError::new(ErrorKind::CompileError, error.to_string()))?;
        vm.consume_work(0)?;
        Ok(Self {
            module,
            estimated_bytes,
        })
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.estimated_bytes
    }
}

struct LinearMemory {
    execution: Execution,
    memory: wasmi::Memory,
    generation: Cell<u64>,
}

pub(crate) struct Buffer {
    memory: Rc<LinearMemory>,
    length: usize,
    generation: u64,
}

impl Buffer {
    pub(crate) fn length(&self) -> Option<usize> {
        let store = self.memory.execution.borrow();
        self.matches(&store).then_some(self.length)
    }

    fn matches(&self, store: &Store<Resources>) -> bool {
        self.memory.generation.get() == self.generation
            && self.memory.memory.data_size(store) == self.length
    }

    pub(crate) fn read(&self) -> Option<Ref<'_, [u8]>> {
        let store = self.memory.execution.borrow();
        self.matches(&store)
            .then(|| Ref::map(store, |store| self.memory.memory.data(store)))
    }

    pub(crate) fn write(&self) -> Option<RefMut<'_, [u8]>> {
        let store = self.memory.execution.borrow_mut();
        self.matches(&store)
            .then(|| RefMut::map(store, |store| self.memory.memory.data_mut(store)))
    }
}

pub(super) fn memory_buffer(vm: &mut Vm, object: &Value) -> Option<Result<Value, JsError>> {
    let memory = vm.host_data::<LinearMemory>(object)?;
    if let Some(buffer) = vm.host_slot(object, MEMORY_BUFFER)
        && vm
            .buffer_source_len(&buffer)
            .is_ok_and(|length| length.is_some())
    {
        return Some(Ok(buffer));
    }
    let length = memory.memory.data_size(&*memory.execution.borrow());
    let generation = memory.generation.get();
    Some((|| {
        let buffer = vm.new_linear_buffer(Buffer {
            memory,
            length,
            generation,
        })?;
        vm.set_host_slot(buffer.clone(), CONTEXT, object.clone())?;
        vm.set_host_slot(object.clone(), MEMORY_BUFFER, buffer.clone())?;
        Ok(buffer)
    })())
}

pub(super) fn memory_grow(
    vm: &mut Vm,
    object: &Value,
    delta: usize,
) -> Option<Result<Value, JsError>> {
    let memory = vm.host_data::<LinearMemory>(object)?;
    let mut store = memory.execution.borrow_mut();
    store.data_mut().limit = store_limit(vm, store.data().memory);
    let result = memory.memory.grow(&mut *store, delta as u64);
    Some(
        result
            .map(|previous| {
                memory
                    .generation
                    .set(memory.generation.get().wrapping_add(1));
                Value::from(previous as usize)
            })
            .map_err(|error| JsError::new(ErrorKind::RangeError, error.to_string())),
    )
}

pub(super) fn is_memory(vm: &Vm, object: &Value) -> bool {
    vm.host_data::<LinearMemory>(object).is_some()
}

#[derive(Debug)]
struct HostCall {
    index: usize,
    params: Vec<Val>,
    results: Vec<ValType>,
}

impl fmt::Display for HostCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WebAssembly import {}", self.index)
    }
}

impl wasmi::errors::HostError for HostCall {}

fn link_error(message: impl Into<String>) -> JsError {
    JsError::new(ErrorKind::LinkError, message)
}

fn runtime_error(error: wasmi::Error) -> JsError {
    JsError::new(ErrorKind::RuntimeError, error.to_string())
}

fn numeric_type(ty: ValType) -> bool {
    matches!(
        ty,
        ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64
    )
}

fn from_js(vm: &mut Vm, value: &Value, ty: ValType) -> Result<Val, JsError> {
    Ok(match ty {
        ValType::I32 => Val::I32(to_i32(vm.to_number(value)?)),
        ValType::I64 => Val::I64(vm.to_bigint_low_u64(value)? as i64),
        ValType::F32 => Val::F32((vm.to_number(value)? as f32).into()),
        ValType::F64 => Val::F64(vm.to_number(value)?.into()),
        _ => {
            return Err(JsError::type_error(
                "Unsupported WebAssembly signature at the JavaScript boundary",
            ));
        }
    })
}

fn to_js(vm: &mut Vm, value: &Val) -> Result<Value, JsError> {
    match *value {
        Val::I32(value) => Ok(value.into()),
        Val::I64(value) => vm.bigint_from_i64(value),
        Val::F32(value) => Ok(Value::Number(f32::from(value).into())),
        Val::F64(value) => Ok(Value::Number(value.into())),
        _ => Err(JsError::type_error(
            "Unsupported WebAssembly signature at the JavaScript boundary",
        )),
    }
}

fn imported_call(vm: &mut Vm, context: &Value, call: &HostCall) -> Result<Vec<Val>, JsError> {
    let imports = vm
        .host_slot(context, INSTANCE_IMPORTS)
        .expect("initialized Wasm import context");
    let callback = vm.get_property(imports, &call.index.to_string())?;
    let arguments = call
        .params
        .iter()
        .map(|value| to_js(vm, value))
        .collect::<Result<Vec<_>, _>>()?;
    let value = vm.invoke(callback, Value::Undefined, &arguments)?;
    match call.results.as_slice() {
        [] => Ok(Vec::new()),
        &[result] => Ok(vec![from_js(vm, &value, result)?]),
        _ => Err(JsError::type_error(
            "Multiple WebAssembly import results require an iterable",
        )),
    }
}

fn run_step<T>(
    vm: &mut Vm,
    execution: &Execution,
    additional: u64,
    step: impl FnOnce(&mut Store<Resources>) -> T,
) -> Result<T, JsError> {
    vm.consume_work(1)?;
    let mut store = execution.borrow_mut();
    store.data_mut().limit = store_limit(vm, store.data().memory);
    let fuel = FUEL_QUANTUM.max(additional);
    store.set_fuel(fuel).expect("Wasm fuel enabled");
    let result = step(&mut store);
    let spent = fuel - store.get_fuel().expect("Wasm fuel enabled");
    drop(store);
    vm.consume_work(spent as usize)?;
    Ok(result)
}

struct Function {
    execution: Execution,
    function: Func,
    ty: FuncType,
    #[cfg(feature = "media-startup-diagnostics")]
    trace_origin: Option<TrapOrigin>,
}

#[cfg(feature = "media-startup-diagnostics")]
enum TrapOrigin {
    Export(String),
    TableSlot(usize),
}

impl Function {
    fn invoke(&self, vm: &mut Vm, context: &Value, args: &[Value]) -> Result<Value, JsError> {
        let inputs = self
            .ty
            .params()
            .iter()
            .enumerate()
            .map(|(i, &ty)| from_js(vm, args.get(i).unwrap_or(&Value::Undefined), ty))
            .collect::<Result<Vec<_>, _>>()?;
        let mut output: Vec<_> = self
            .ty
            .results()
            .iter()
            .copied()
            .map(Val::default_for_ty)
            .collect();
        let mut state = run_step(vm, &self.execution, 0, |store| {
            self.function.call_resumable(store, &inputs, &mut output)
        })?;
        loop {
            state = match state {
                Ok(ResumableCall::Finished) => break,
                Ok(ResumableCall::HostTrap(trap)) => {
                    let call = trap
                        .host_error()
                        .downcast_ref::<HostCall>()
                        .ok_or_else(|| {
                            JsError::new(ErrorKind::RuntimeError, trap.host_error().to_string())
                        })?;
                    let values = imported_call(vm, context, call)?;
                    run_step(vm, &self.execution, 0, |store| {
                        trap.resume(store, &values, &mut output)
                    })?
                }
                Ok(ResumableCall::OutOfFuel(suspended)) => {
                    let requested = suspended.required_fuel();
                    run_step(vm, &self.execution, requested, |store| {
                        suspended.resume(store, &mut output)
                    })?
                }
                Err(error) => {
                    if let Some(call) = error.downcast_ref::<HostCall>() {
                        output = imported_call(vm, context, call)?;
                        break;
                    }
                    #[cfg(feature = "media-startup-diagnostics")]
                    self.trace_trap(&error);
                    return Err(runtime_error(error));
                }
            };
        }
        match output.as_slice() {
            [] => Ok(Value::Undefined),
            [value] => to_js(vm, value),
            values => {
                let values = values
                    .iter()
                    .map(|value| to_js(vm, value))
                    .collect::<Result<Vec<_>, _>>()?;
                vm.try_new_array(&values)
            }
        }
    }

    #[cfg(feature = "media-startup-diagnostics")]
    fn trace_trap(&self, error: &wasmi::Error) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TRACES: AtomicUsize = AtomicUsize::new(0);
        if let (Some(origin), Some(trap)) = (&self.trace_origin, error.as_trap_code())
            && TRACES.fetch_add(1, Ordering::Relaxed) < 32
        {
            match origin {
                TrapOrigin::Export(name) => {
                    eprintln!("SATIN_QA_WASM_TRAP export={name} trap={trap:?}");
                }
                TrapOrigin::TableSlot(index) => {
                    eprintln!("SATIN_QA_WASM_TRAP table_slot={index} trap={trap:?}");
                }
            }
        }
    }
}

#[cfg(feature = "media-startup-diagnostics")]
fn trap_trace_origin(name: &str, table_slot: Option<usize>) -> Option<TrapOrigin> {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("SATIN_QA_WASM_TRAPS").is_some()) {
        return None;
    }
    if let Some(index) = table_slot {
        return Some(TrapOrigin::TableSlot(index));
    }
    let safe = !name.is_empty() && name.len() <= 32
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_$".contains(&byte));
    Some(TrapOrigin::Export(if safe { name } else { "-" }.into()))
}

fn function(
    vm: &mut Vm,
    execution: &Execution,
    context: &Value,
    function: Func,
    name: &str,
    #[cfg(feature = "media-startup-diagnostics")] table_slot: Option<usize>,
) -> Result<Value, JsError> {
    let ty = function.ty(&*execution.borrow());
    let length = ty.params().len();
    let native = Rc::new(Function {
        execution: execution.clone(),
        function,
        ty,
        #[cfg(feature = "media-startup-diagnostics")]
        trace_origin: trap_trace_origin(name, table_slot),
    });
    let callable = native.clone();
    let captured = context.clone();
    let value = vm.native(
        name,
        length,
        false,
        Rc::new(move |vm, _, args| callable.invoke(vm, &captured, args)),
    );
    vm.set_host_slot(value.clone(), CONTEXT, context.clone())?;
    vm.set_host_data(value.clone(), native)?;
    Ok(value)
}

struct FunctionTable {
    execution: Execution,
    table: Table,
}

fn table_receiver(vm: &Vm, this: &Value) -> Result<Rc<FunctionTable>, JsError> {
    vm.host_data(this)
        .ok_or_else(|| JsError::type_error("WebAssembly.Table receiver required"))
}

fn table_ref(vm: &Vm, table: &FunctionTable, value: &Value) -> Result<wasmi::Ref, JsError> {
    let element = table.table.ty(&*table.execution.borrow()).element();
    if matches!(value, Value::Null) {
        return Ok(wasmi::Ref::null(element));
    }
    if let Some(function) = vm.host_data::<Function>(value)
        && Rc::ptr_eq(&table.execution, &function.execution)
        && element == wasmi::RefType::Func
    {
        return Ok(wasmi::Ref::Func(Nullable::Val(function.function)));
    }
    Err(JsError::type_error(
        "Expected a WebAssembly function from this table's store or null",
    ))
}

pub(super) fn install_table(vm: &mut Vm, namespace: &Value) {
    let prototype = vm.new_object();
    tag(vm, &prototype, "WebAssembly.Table");
    class(vm, namespace, "Table", &prototype, false, |_, _, _| {
        Err(JsError::type_error(
            "Only tables exported by a WebAssembly instance are supported",
        ))
    });
    let length = vm.new_native_function("get length", |vm, this, _| {
        let data = table_receiver(vm, &this)?;
        let length = data.table.size(&*data.execution.borrow());
        Ok((length as usize).into())
    });
    vm.define_property(
        prototype.clone(),
        "length".into(),
        Property {
            kind: PropertyKind::Accessor {
                get: Some(length),
                set: None,
            },
            enumerable: true,
            configurable: true,
        },
    )
    .unwrap();
    method(vm, prototype.clone(), "get", 1, |vm, this, args| {
        let data = table_receiver(vm, &this)?;
        let index = super::memory::page_count(vm, &arg(args, 0))?;
        let entry = data.table.get(&*data.execution.borrow(), index as u64);
        match entry {
            Some(wasmi::Ref::Func(Nullable::Val(func))) => {
                let context = vm
                    .host_slot(&this, CONTEXT)
                    .expect("initialized WebAssembly table");
                function(
                    vm,
                    &data.execution,
                    &context,
                    func,
                    "",
                    #[cfg(feature = "media-startup-diagnostics")]
                    Some(index),
                )
            }
            Some(value) if value.is_null() => Ok(Value::Null),
            Some(_) => Err(JsError::type_error("Unsupported WebAssembly reference")),
            None => Err(JsError::new(
                ErrorKind::RangeError,
                "WebAssembly table index out of bounds",
            )),
        }
    });
    method(vm, prototype.clone(), "set", 2, |vm, this, args| {
        let data = table_receiver(vm, &this)?;
        let index = super::memory::page_count(vm, &arg(args, 0))?;
        let value = table_ref(vm, &data, args.get(1).unwrap_or(&Value::Null))?;
        data.table
            .set(&mut *data.execution.borrow_mut(), index as u64, value)
            .map_err(|error| JsError::new(ErrorKind::RangeError, error.to_string()))?;
        Ok(Value::Undefined)
    });
    method(vm, prototype.clone(), "grow", 1, |vm, this, args| {
        let data = table_receiver(vm, &this)?;
        let delta = super::memory::page_count(vm, &arg(args, 0))?;
        let value = table_ref(vm, &data, args.get(1).unwrap_or(&Value::Null))?;
        let previous = data
            .table
            .grow(&mut *data.execution.borrow_mut(), delta as u64, value)
            .map_err(|error| JsError::new(ErrorKind::RangeError, error.to_string()))?;
        Ok((previous as usize).into())
    });
    vm.realm.wasm_table_prototype = prototype.as_object();
    vm.root(prototype);
}

pub(super) fn instantiate(
    vm: &mut Vm,
    module: &Module,
    module_object: Value,
    imports: &Value,
    prototype: ObjectId,
    memory_proto: ObjectId,
) -> Result<Value, JsError> {
    validate_import_object(imports)?;
    if module.module.imports().len() != 0 && imports.is_undefined() {
        return Err(JsError::type_error(
            "WebAssembly module requires an import object",
        ));
    }
    let resources = Resources {
        pool: vm.wasm_linear_pool.clone(),
        limit: store_limit(vm, 0),
        memory: 0,
        previous: 0,
    };
    let mut store = Store::new(module.module.engine(), resources);
    store.limiter(|resource| resource);
    let mut imports_wasm = Vec::new();
    let mut callbacks = Vec::new();
    for import in module.module.imports() {
        let namespace = vm.get_property(imports.clone(), import.module())?;
        if namespace.as_object().is_none() {
            return Err(JsError::type_error(
                "WebAssembly import namespace must be an object",
            ));
        }
        let value = vm.get_property(namespace, import.name())?;
        let ExternType::Func(ty) = import.ty() else {
            return Err(link_error("Unsupported WebAssembly import kind"));
        };
        if !vm.is_callable(&value) {
            return Err(link_error("WebAssembly function import is not callable"));
        }
        if ty.results().len() > 1
            || !ty
                .params()
                .iter()
                .chain(ty.results())
                .copied()
                .all(numeric_type)
        {
            return Err(link_error("Unsupported WebAssembly import signature"));
        }
        let index = callbacks.len();
        callbacks.push(value);
        let results = ty.results().to_vec();
        let function = Func::new(&mut store, ty.clone(), move |_, params, _| {
            Err(wasmi::Error::host(HostCall {
                index,
                params: params.to_vec(),
                results: results.clone(),
            }))
        });
        imports_wasm.push(Extern::Func(function));
    }
    let context = vm.new_object();
    let callbacks = vm.try_new_array(&callbacks)?;
    vm.set_host_slot(context.clone(), INSTANCE_IMPORTS, callbacks)?;
    vm.set_host_slot(context.clone(), INSTANCE_MODULE, module_object)?;
    store.data_mut().limit = store_limit(vm, 0);
    let instance = wasmi::Instance::new(&mut store, &module.module, &imports_wasm)
        .map_err(|error| link_error(error.to_string()))?;
    let exports: Vec<_> = instance
        .exports(&store)
        .map(|export| (export.name().to_owned(), export.into_extern()))
        .collect();
    let execution = Rc::new(RefCell::new(store));
    let values = vm.new_object_with_prototype(Value::Null)?;
    for (name, export) in exports {
        let value = match export {
            Extern::Func(func) => function(
                vm,
                &execution,
                &context,
                func,
                &name,
                #[cfg(feature = "media-startup-diagnostics")]
                None,
            )?,
            Extern::Memory(memory) => {
                let result = vm.new_object_with_prototype(Value::Object(memory_proto))?;
                vm.set_host_data(
                    result.clone(),
                    Rc::new(LinearMemory {
                        execution: execution.clone(),
                        memory,
                        generation: Cell::new(0),
                    }),
                )?;
                vm.set_host_slot(result.clone(), CONTEXT, context.clone())?;
                result
            }
            Extern::Table(table) => {
                let prototype = vm
                    .realm
                    .wasm_table_prototype
                    .expect("initialized WebAssembly.Table");
                let result = vm.new_object_with_prototype(Value::Object(prototype))?;
                vm.set_host_data(
                    result.clone(),
                    Rc::new(FunctionTable {
                        execution: execution.clone(),
                        table,
                    }),
                )?;
                vm.set_host_slot(result.clone(), CONTEXT, context.clone())?;
                result
            }
            _ => return Err(link_error("Unsupported WebAssembly export kind")),
        };
        vm.define_data_property(values.clone(), &name, value, false, true, false)?;
    }
    vm.prevent_extensions(values.as_object().unwrap())?;
    let result = vm.allocate_constructed(ObjectKind::WasmInstance, prototype)?;
    vm.set_host_slot(result.clone(), INSTANCE_EXPORTS, values)?;
    vm.set_host_slot(result.clone(), CONTEXT, context)?;
    #[cfg(feature = "media-startup-diagnostics")]
    eprintln!(
        "SATIN_QA_WASM instantiated bytes={} linear_bytes={} imports={} exports={}",
        module.estimated_bytes / 8,
        execution.borrow().data().memory,
        imports_wasm.len(),
        module.module.exports().count(),
    );
    Ok(result)
}

fn store_limit(vm: &Vm, current: usize) -> usize {
    vm.limits
        .max_heap_bytes
        .saturating_sub(vm.estimated_heap_bytes())
        .saturating_add(current)
        .min(LINEAR_LIMIT)
}
