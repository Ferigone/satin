//! V8 execution for classic document scripts using the existing Satin host.

use crate::{ErrorKind, JsError, JsString, ObjectId, RootId, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr,
    rc::Rc,
    sync::{Once, mpsc},
    thread,
    time::Duration,
};

// Keep the transport private to V8. All DOM state and method dispatch stay in
// the owning Satin runtime. The map gives each host object one JS identity.
const BRIDGE: &str = r#"(function (dispatch) {
    const wrappers = new Map();
    const ids = new WeakMap();
    const global = globalThis;
    const ProxyConstructor = Proxy;
    const get = Reflect.get;
    const set = Reflect.set;
    const own = Object.getOwnPropertyDescriptor;
    const byId = Function.call.bind(Map.prototype.get);
    const rememberWrapper = Function.call.bind(Map.prototype.set);
    const idOf = Function.call.bind(WeakMap.prototype.get);
    const rememberId = Function.call.bind(WeakMap.prototype.set);
    return [
        function wrap(id, callable) {
            const existing = byId(wrappers, id);
            if (existing !== undefined) return existing;
            const target = callable ? function () {} : {};
            const proxy = new ProxyConstructor(target, {
                get(target, name, receiver) {
                    const property = own(target, name);
                    if (typeof name === 'symbol' ||
                        (property && !property.configurable && !property.writable)) {
                        return get(target, name, receiver);
                    }
                    const value = dispatch(0, id, name);
                    return value === undefined ? get(target, name, receiver) : value;
                },
                set(target, name, value) {
                    if (typeof name === 'symbol') return set(target, name, value);
                    dispatch(1, id, name, value);
                    return true;
                },
                has(target, name) {
                    return typeof name === 'string' && dispatch(4, id, name) || name in target;
                },
                apply(target, receiver, args) {
                    return dispatch(2, id, receiver, args);
                },
                construct(target, args) {
                    return dispatch(3, id, undefined, args);
                },
            });
            rememberWrapper(wrappers, id, proxy);
            rememberId(ids, proxy, id);
            return proxy;
        },
        value => value === global ? -1 : idOf(ids, value),
    ];
})"#;

const WRAP_SLOT: i32 = 0;
const ID_SLOT: i32 = 1;
const HOST_GLOBALS: &[&str] = &[
    "document",
    "console",
    "navigator",
    "location",
    "history",
    "performance",
    "getComputedStyle",
    "getSelection",
    "atob",
    "btoa",
    "URL",
    "URLSearchParams",
    "addEventListener",
    "removeEventListener",
    "setTimeout",
    "clearTimeout",
    "setInterval",
    "clearInterval",
    "requestAnimationFrame",
    "cancelAnimationFrame",
];

#[derive(Default)]
pub(crate) enum DocumentEngine {
    #[default]
    Interpreter,
    Ready(DocumentRuntime),
    Running,
}

#[derive(Default)]
struct Bridge {
    // A call may use the host only during an entered evaluation. Temporarily
    // clearing the pointer while borrowed prevents nested mutable references.
    active: Cell<*mut Vm>,
    objects: RefCell<HostObjects>,
    callbacks: RefCell<Vec<HostCallback>>,
}

struct HostCallback {
    function: v8::Global<v8::Function>,
    native: RootId,
}

#[derive(Default)]
struct HostObjects {
    indices: HashMap<ObjectId, u32>,
    roots: Vec<RootId>,
}

struct HostPointer<'a> {
    slot: &'a Cell<*mut Vm>,
    restore: *mut Vm,
}

impl Drop for HostPointer<'_> {
    fn drop(&mut self) {
        self.slot.set(self.restore);
    }
}

impl Bridge {
    fn with_host<T>(
        &self,
        action: impl FnOnce(&mut Vm) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let pointer = self.active.replace(ptr::null_mut());
        if pointer.is_null() {
            return Err(JsError::type_error(
                "Satin host callback is unavailable or reentrant",
            ));
        }
        let _guard = HostPointer {
            slot: &self.active,
            restore: pointer,
        };
        // The pointer is installed from the exclusive VM borrow for the
        // duration of this call to V8, on the same thread. The guard prevents
        // another callback from using it until this reference has ended.
        action(unsafe { &mut *pointer })
    }

    fn object(&self, vm: &Vm, index: u32) -> Result<Value, JsError> {
        self.objects
            .borrow()
            .roots
            .get(index as usize)
            .and_then(|root| vm.root_value(*root))
            .ok_or_else(|| JsError::type_error("Unknown Satin host object"))
    }

    fn index(&self, vm: &mut Vm, object: ObjectId) -> Result<u32, JsError> {
        let mut objects = self.objects.borrow_mut();
        if let Some(index) = objects.indices.get(&object) {
            return Ok(*index);
        }
        let index = u32::try_from(objects.roots.len())
            .map_err(|_| JsError::new(ErrorKind::HeapLimit, "Satin host bridge object limit"))?;
        objects.indices.insert(object, index);
        objects.roots.push(vm.root(Value::Object(object)));
        Ok(index)
    }

    fn import<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        vm: &mut Vm,
        value: Value,
    ) -> Result<v8::Local<'s, v8::Value>, JsError> {
        Ok(match value {
            Value::Undefined => v8::undefined(scope).into(),
            Value::Null => v8::null(scope).into(),
            Value::Bool(value) => v8::Boolean::new(scope, value).into(),
            Value::Number(value) => v8::Number::new(scope, value).into(),
            Value::String(value) => {
                v8::String::new_from_two_byte(scope, value.as_utf16(), v8::NewStringType::Normal)
                    .ok_or_else(allocation_error)?
                    .into()
            }
            Value::Object(_) if value == vm.global() => {
                scope.get_current_context().global(scope).into()
            }
            Value::Object(object) => {
                if let Some(callback) = self.callbacks.borrow().iter().find(|callback| {
                    vm.root_value(callback.native) == Some(Value::Object(object))
                }) {
                    return Ok(v8::Local::new(scope, &callback.function).into());
                }
                let wrap = function_slot(scope, WRAP_SLOT)?;
                let index = v8::Integer::new_from_unsigned(scope, self.index(vm, object)?);
                let callable = v8::Boolean::new(scope, vm.is_callable(&value));
                let receiver = v8::undefined(scope);
                wrap.call(scope, receiver.into(), &[index.into(), callable.into()])
                    .ok_or_else(bridge_error)?
            }
            Value::BigInt(_) | Value::Symbol(_) => {
                return Err(JsError::type_error(
                    "This host primitive cannot yet cross the V8 boundary",
                ));
            }
        })
    }

    fn export<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        vm: &mut Vm,
        value: v8::Local<'s, v8::Value>,
        accept_callback: bool,
    ) -> Result<Value, JsError> {
        if value.is_object() {
            let resolve = function_slot(scope, ID_SLOT)?;
            let receiver = v8::undefined(scope);
            let index = resolve
                .call(scope, receiver.into(), &[value])
                .ok_or_else(bridge_error)?;
            if index.is_uint32() {
                return self.object(vm, index.uint32_value(scope).ok_or_else(bridge_error)?);
            }
            if index.is_int32() && index.int32_value(scope) == Some(-1) {
                return Ok(vm.global());
            }
            if accept_callback && let Ok(function) = v8::Local::<v8::Function>::try_from(value) {
                let mut callbacks = self.callbacks.borrow_mut();
                if let Some(callback) = callbacks.iter().find(|callback| {
                    v8::Local::new(scope, &callback.function).strict_equals(value)
                }) {
                    return vm.root_value(callback.native).ok_or_else(bridge_error);
                }
                let index = callbacks.len();
                let native = vm.new_native_function("V8 document callback", move |vm, this, args| {
                    vm.call_v8_document_callback(index, this, args)
                });
                callbacks.push(HostCallback {
                    function: v8::Global::new(scope, function),
                    native: vm.root(native.clone()),
                });
                return Ok(native);
            }
            return Err(JsError::type_error(
                "This V8 object cannot yet be passed to Satin host code",
            ));
        }
        if value.is_string() {
            return Ok(Value::String(read_string(scope, value)?));
        }
        if value.is_number() {
            return Ok(Value::Number(
                value.number_value(scope).ok_or_else(bridge_error)?,
            ));
        }
        if value.is_boolean() {
            return Ok(Value::Bool(value.boolean_value(scope)));
        }
        if value.is_undefined() {
            return Ok(Value::Undefined);
        }
        if value.is_null() {
            return Ok(Value::Null);
        }
        Err(JsError::type_error(
            "This V8 primitive cannot yet be passed to Satin host code",
        ))
    }
}

fn allocation_error() -> JsError {
    JsError::new(ErrorKind::HeapLimit, "V8 could not allocate a script value")
}

fn bridge_error() -> JsError {
    JsError::new(
        ErrorKind::InternalError,
        "V8 could not dispatch a Satin host operation",
    )
}

fn function_slot<'s>(
    scope: &v8::PinScope<'s, '_>,
    slot: i32,
) -> Result<v8::Local<'s, v8::Function>, JsError> {
    scope
        .get_current_context()
        .get_embedder_data(scope, slot)
        .and_then(|value| v8::Local::try_from(value).ok())
        .ok_or_else(bridge_error)
}

fn read_string(scope: &v8::PinScope, value: v8::Local<v8::Value>) -> Result<JsString, JsError> {
    let value = v8::Local::<v8::String>::try_from(value)
        .map_err(|_| JsError::type_error("A property key must be a string"))?;
    let mut units = vec![0; value.length()];
    value.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
    Ok(JsString::from_utf16(units))
}

fn dispatch<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut result: v8::ReturnValue,
) {
    let outcome = (|| {
        let bridge = scope
            .get_current_context()
            .get_slot::<Bridge>()
            .ok_or_else(bridge_error)?;
        let operation = args.get(0).uint32_value(scope).ok_or_else(bridge_error)?;
        let index = args.get(1).uint32_value(scope).ok_or_else(bridge_error)?;
        bridge.with_host(|vm| {
            let value = bridge.object(vm, index)?;
            let returned = match operation {
                0 => vm.get_property_key(
                    value,
                    crate::PropertyKey::String(read_string(scope, args.get(2))?),
                )?,
                1 => {
                    let key = crate::PropertyKey::String(read_string(scope, args.get(2))?);
                    let assigned = bridge.export(scope, vm, args.get(3), true)?;
                    vm.set_property_key(value, key, assigned)?;
                    Value::Undefined
                }
                2 | 3 => {
                    let receiver = bridge.export(scope, vm, args.get(2), false)?;
                    let arguments = v8::Local::<v8::Array>::try_from(args.get(3))
                        .map_err(|_| bridge_error())?;
                    let values = (0..arguments.length())
                        .map(|index| {
                            let argument =
                                arguments.get_index(scope, index).ok_or_else(bridge_error)?;
                            bridge.export(scope, vm, argument, true)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if operation == 2 {
                        vm.call(value, receiver, &values)?
                    } else {
                        vm.construct(value, &values)?
                    }
                }
                4 => Value::Bool(
                    vm.has_property(&value, &read_string(scope, args.get(2))?.to_string_lossy())?,
                ),
                _ => return Err(bridge_error()),
            };
            bridge.import(scope, vm, returned)
        })
    })();
    match outcome {
        Ok(value) => result.set(value),
        Err(error) => {
            if let Some(message) = v8::String::new(scope, &error.message) {
                let exception = match error.kind {
                    ErrorKind::TypeError => v8::Exception::type_error(scope, message),
                    ErrorKind::RangeError => v8::Exception::range_error(scope, message),
                    _ => v8::Exception::error(scope, message),
                };
                scope.throw_exception(exception);
            }
        }
    }
}

// rusty_v8 enters a newly created OwnedIsolate and expects it to be current
// at destruction. Satin documents can close in any order on the UI thread.
struct InactiveIsolate(Option<v8::OwnedIsolate>);

struct EnteredIsolate<'a>(&'a mut v8::OwnedIsolate);

impl Drop for EnteredIsolate<'_> {
    fn drop(&mut self) {
        // This guard owns the matching enter, with all inner scopes dropped.
        unsafe { self.0.exit() };
    }
}

impl InactiveIsolate {
    fn new() -> Self {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
        });
        let isolate = v8::Isolate::new(Default::default());
        // The constructor made this isolate current on the present thread.
        unsafe { isolate.exit() };
        Self(Some(isolate))
    }

    fn enter(&mut self) -> EnteredIsolate<'_> {
        let isolate = self.0.as_mut().expect("owned document isolate");
        // OwnedIsolate is thread-affine. The mutable guard pairs this enter
        // with an exit before any later activation or disposal.
        unsafe { isolate.enter() };
        EnteredIsolate(isolate)
    }
}

impl Drop for InactiveIsolate {
    fn drop(&mut self) {
        if let Some(isolate) = self.0.take() {
            // OwnedIsolate's destructor will exit this activation itself.
            unsafe { isolate.enter() };
            drop(isolate);
        }
    }
}

pub(crate) struct DocumentRuntime {
    context: v8::Global<v8::Context>,
    bridge: Rc<Bridge>,
    isolate: InactiveIsolate,
}

enum Task<'a> {
    Evaluate { source: &'a str, identifier: &'a str },
    Invoke { index: usize, receiver: Value, arguments: &'a [Value] },
}

impl Drop for DocumentRuntime {
    fn drop(&mut self) {
        let _entered = self.isolate.enter();
        // The V8 functions can retain their context, which owns the typed slot.
        self.bridge.callbacks.borrow_mut().clear();
    }
}

impl DocumentRuntime {
    pub(crate) fn new(vm: &mut Vm) -> Result<Self, JsError> {
        let mut isolate = InactiveIsolate::new();
        let bridge = Rc::new(Bridge::default());
        let context = {
            let entered = isolate.enter();
            entered.0.set_allow_atomics_wait(false);
            entered
                .0
                .set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
            v8::scope!(let handles, &mut *entered.0);
            let context = v8::Context::new(handles, Default::default());
            context.set_slot(bridge.clone());
            let scope = &mut v8::ContextScope::new(handles, context);
            let source = v8::String::new(scope, BRIDGE).ok_or_else(allocation_error)?;
            let factory = v8::Script::compile(scope, source, None)
                .and_then(|script| script.run(scope))
                .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
                .ok_or_else(bridge_error)?;
            let dispatch = v8::Function::new(scope, dispatch).ok_or_else(bridge_error)?;
            let receiver = v8::undefined(scope);
            let bindings = factory
                .call(scope, receiver.into(), &[dispatch.into()])
                .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
                .ok_or_else(bridge_error)?;
            for slot in [WRAP_SLOT, ID_SLOT] {
                let binding = bindings
                    .get_index(scope, slot as u32)
                    .ok_or_else(bridge_error)?;
                context.set_embedder_data(slot, binding);
            }
            let global = context.global(scope);
            for key in ["window", "self"] {
                let key = v8::String::new(scope, key).ok_or_else(allocation_error)?;
                if global.set(scope, key.into(), global.into()) != Some(true) {
                    return Err(bridge_error());
                }
            }
            for name in HOST_GLOBALS {
                let value = vm.get_global(name)?;
                if value != Value::Undefined {
                    let key = v8::String::new(scope, name).ok_or_else(allocation_error)?;
                    let value = bridge.import(scope, vm, value)?;
                    if global.set(scope, key.into(), value) != Some(true) {
                        return Err(bridge_error());
                    }
                }
            }
            v8::Global::new(scope, context)
        };
        Ok(Self {
            context,
            bridge,
            isolate,
        })
    }

    pub(crate) fn evaluate(
        &mut self,
        vm: &mut Vm,
        source: &str,
        identifier: &str,
        timeout: Duration,
    ) -> Result<Value, JsError> {
        self.run(vm, Task::Evaluate { source, identifier }, timeout)
    }

    pub(crate) fn invoke(
        &mut self, vm: &mut Vm, index: usize, receiver: Value, arguments: &[Value], timeout: Duration,
    ) -> Result<Value, JsError> {
        self.run(vm, Task::Invoke { index, receiver, arguments }, timeout)
    }

    fn run(&mut self, vm: &mut Vm, task: Task<'_>, timeout: Duration) -> Result<Value, JsError> {
        let entered = self.isolate.enter();
        let handle = entered.0.thread_safe_handle();
        let (finished, wait) = mpsc::channel();
        let watchdog = thread::Builder::new()
            .name("satin-v8-watchdog".into())
            .spawn(move || {
                if wait.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                    handle.terminate_execution();
                }
            })
            .map_err(|error| JsError::new(ErrorKind::InternalError, error.to_string()))?;
        self.bridge.active.set(vm);
        let guard = HostPointer {
            slot: &self.bridge.active,
            restore: ptr::null_mut(),
        };
        let result = (|| {
            v8::scope!(let handles, &mut *entered.0);
            let context = v8::Local::new(handles, &self.context);
            let scope = &mut v8::ContextScope::new(handles, context);
            v8::tc_scope!(let scope, scope);
            let value = match task {
                Task::Evaluate { source, identifier } => {
                    let source = v8::String::new(scope, source).ok_or_else(allocation_error)?;
                    let identifier = v8::String::new(scope, identifier).ok_or_else(allocation_error)?;
                    let origin = v8::ScriptOrigin::new(
                        scope, identifier.into(), 0, 0, false, -1, None, false, false, false, None,
                    );
                    let Some(script) = v8::Script::compile(scope, source, Some(&origin)) else {
                        let detail = scope.message().map(|message| message.get(scope).to_rust_string_lossy(scope));
                        return Err(JsError::new(ErrorKind::SyntaxError,
                            detail.unwrap_or_else(|| "V8 could not compile the script".into())));
                    };
                    script.run(scope)
                }
                Task::Invoke { index, receiver, arguments } => {
                    let function = {
                        let callbacks = self.bridge.callbacks.borrow();
                        let callback = callbacks.get(index).ok_or_else(bridge_error)?;
                        v8::Local::new(scope, &callback.function)
                    };
                    let (receiver, arguments) = self.bridge.with_host(|vm| {
                        let receiver = self.bridge.import(scope, vm, receiver)?;
                        let arguments = arguments.iter().cloned().map(|argument| {
                            self.bridge.import(scope, vm, argument)
                        }).collect::<Result<Vec<_>, _>>()?;
                        Ok((receiver, arguments))
                    })?;
                    function.call(scope, receiver, &arguments)
                }
            };
            let Some(value) = value else {
                let detail = scope
                    .message()
                    .map(|message| message.get(scope).to_rust_string_lossy(scope));
                let error = JsError::new(
                    ErrorKind::Error,
                    detail.unwrap_or_else(|| "V8 script threw an exception".into()),
                );
                if !scope.has_terminated() {
                    scope.reset();
                    scope.perform_microtask_checkpoint();
                }
                return Err(error);
            };
            scope.perform_microtask_checkpoint();
            self.bridge.with_host(|vm| {
                Ok(self
                    .bridge
                    .export(scope, vm, value, false)
                    .unwrap_or(Value::Undefined))
            })
        })();
        drop(guard);
        let _ = finished.send(());
        let _ = watchdog.join();
        if entered.0.is_execution_terminating() {
            entered.0.cancel_terminate_execution();
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "V8 document task exceeded its wall deadline",
            ));
        }
        result
    }
}
