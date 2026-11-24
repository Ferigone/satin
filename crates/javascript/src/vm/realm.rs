//! Intrinsics and host hooks for environments sharing one interpreter heap.

use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    dynamic_import::{DynamicImportHook, SourceReferrer},
    heap::{Cell, EnvId, Environment, Heap, Object, ObjectKind, PropertyKind},
};
use std::{collections::BTreeMap, rc::Rc};

#[derive(Clone, Copy)]
pub(crate) struct CoreIntrinsics {
    pub(super) object_prototype: ObjectId,
    pub(super) function_prototype: ObjectId,
    pub(super) array_prototype: ObjectId,
    pub(super) string_prototype: ObjectId,
    pub(super) number_prototype: ObjectId,
    pub(super) boolean_prototype: ObjectId,
    pub(super) error_prototype: ObjectId,
}

impl CoreIntrinsics {
    pub(crate) fn objects(&self) -> [ObjectId; 7] {
        [
            self.object_prototype,
            self.function_prototype,
            self.array_prototype,
            self.string_prototype,
            self.number_prototype,
            self.boolean_prototype,
            self.error_prototype,
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealmId(EnvId);

pub(crate) struct RealmState {
    /// Binding object of `global_env`; it need not be the global this value.
    pub(crate) global_object: ObjectId,
    pub(crate) global_env: EnvId,
    pub(crate) object_prototype: ObjectId,
    pub(crate) function_prototype: ObjectId,
    pub(crate) async_function_prototype: Option<ObjectId>,
    pub(crate) generator_function_prototype: Option<ObjectId>,
    pub(crate) generator_prototype: Option<ObjectId>,
    pub(crate) async_generator_function_prototype: Option<ObjectId>,
    pub(crate) async_generator_prototype: Option<ObjectId>,
    pub(crate) async_iterator_prototype: Option<ObjectId>,
    pub(crate) async_from_sync_prototype: Option<ObjectId>,
    pub(crate) array_prototype: ObjectId,
    constructor_prototypes: Vec<Option<ObjectId>>,
    pub(crate) intl_prototypes: Vec<(&'static str, ObjectId)>,
    pub(crate) binary: crate::binary::Intrinsics,
    pub(crate) clone_intrinsics: crate::structured_clone::Intrinsics,
    pub(crate) string_prototype: ObjectId,
    pub(crate) regexp_prototype: Option<ObjectId>,
    pub(crate) regexp_exec: Option<ObjectId>,
    pub(crate) regexp_constructor: Option<ObjectId>,
    pub(crate) regexp_iterator_prototype: Option<ObjectId>,
    pub(crate) number_prototype: ObjectId,
    pub(crate) bigint_prototype: Option<ObjectId>,
    pub(crate) boolean_prototype: ObjectId,
    pub(crate) error_prototype: ObjectId,
    pub(crate) error_prototypes: BTreeMap<ErrorKind, ObjectId>,
    pub(crate) intrinsic_eval: Option<ObjectId>,
    pub(crate) throw_type_error: Option<ObjectId>,
    pub(crate) promise_prototype: Option<ObjectId>,
    pub(crate) promise_constructor: Option<ObjectId>,
    pub(crate) symbol_prototype: Option<ObjectId>,
    pub(crate) iterator_prototype: Option<ObjectId>,
    pub(crate) array_iterator_prototype: Option<ObjectId>,
    pub(crate) string_iterator_prototype: Option<ObjectId>,
    pub(crate) module_realm: u64,
    pub(crate) active_referrer: Option<SourceReferrer>,
    pub(crate) dynamic_import_hook: Option<DynamicImportHook>,
    pub(crate) wasm_compilation_allowed: bool,
}

impl RealmState {
    pub(super) fn new(heap: &Heap, global_env: EnvId, wasm_compilation_allowed: bool) -> Self {
        let (
            global_object,
            &CoreIntrinsics {
                object_prototype,
                function_prototype,
                array_prototype,
                string_prototype,
                number_prototype,
                boolean_prototype,
                error_prototype,
            },
        ) = heap.realm_core(global_env).expect("fresh realm core");

        Self {
            global_object,
            global_env,
            object_prototype,
            function_prototype,
            async_function_prototype: None,
            generator_function_prototype: None,
            generator_prototype: None,
            async_generator_function_prototype: None,
            async_generator_prototype: None,
            async_iterator_prototype: None,
            async_from_sync_prototype: None,
            array_prototype,
            constructor_prototypes: Vec::new(),
            intl_prototypes: Vec::new(),
            binary: crate::binary::Intrinsics::default(),
            clone_intrinsics: crate::structured_clone::Intrinsics::default(),
            string_prototype,
            regexp_prototype: None,
            regexp_exec: None,
            regexp_constructor: None,
            regexp_iterator_prototype: None,
            number_prototype,
            bigint_prototype: None,
            boolean_prototype,
            error_prototype,
            error_prototypes: BTreeMap::new(),
            intrinsic_eval: None,
            throw_type_error: None,
            promise_prototype: None,
            promise_constructor: None,
            symbol_prototype: None,
            iterator_prototype: None,
            array_iterator_prototype: None,
            string_iterator_prototype: None,
            module_realm: crate::modules::module_realm_id(),
            active_referrer: None,
            dynamic_import_hook: None,
            wasm_compilation_allowed,
        }
    }
}

fn allocate_object(heap: &mut Heap, kind: ObjectKind, prototype: Option<ObjectId>) -> ObjectId {
    heap.allocate(Cell::Object(Object::new(kind, prototype)))
}

/// The primary realm is part of infallible VM startup, not a guest operation.
pub(super) fn allocate_primary_core(heap: &mut Heap) -> EnvId {
    assert_eq!(heap.live, 0, "a primary core requires a fresh heap");
    allocate_core(heap)
}

// Only called after admission, except during primary interpreter startup.
fn allocate_core(heap: &mut Heap) -> EnvId {
    let object_prototype = allocate_object(heap, ObjectKind::Ordinary, None);
    let prototype = Some(object_prototype);
    let global = allocate_object(heap, ObjectKind::Ordinary, prototype);
    let env = heap.allocate_env(Environment::new_global(global, global));
    let function_prototype = allocate_object(
        heap,
        ObjectKind::Native {
            callback: Rc::new(|_, _, _| Ok(Value::Undefined)),
            construct: None,
            realm_environment: env,
        },
        prototype,
    );
    let array_prototype = allocate_object(
        heap,
        ObjectKind::Array {
            length: 0,
            length_writable: true,
        },
        prototype,
    );
    let string_prototype = allocate_object(heap, ObjectKind::Boxed(Value::from("")), prototype);
    let number_prototype = allocate_object(heap, ObjectKind::Boxed(Value::Number(0.0)), prototype);
    let boolean_prototype = allocate_object(heap, ObjectKind::Boxed(Value::Bool(false)), prototype);
    let error_prototype = allocate_object(heap, ObjectKind::Ordinary, prototype);
    heap.attach_core_intrinsics(
        env,
        CoreIntrinsics {
            object_prototype,
            function_prototype,
            array_prototype,
            string_prototype,
            number_prototype,
            boolean_prototype,
            error_prototype,
        },
    );
    env
}

struct EnteredRealm<'a> {
    vm: &'a mut Vm,
    previous: EnvId,
}

impl Drop for EnteredRealm<'_> {
    fn drop(&mut self) {
        let previous = self
            .vm
            .inactive_realms
            .remove(&self.previous)
            .expect("caller realm");
        let leaving = std::mem::replace(&mut self.vm.realm, previous);
        self.vm.inactive_realms.insert(leaving.global_env, leaving);
        self.vm.realm_depth -= 1;
    }
}

impl Vm {
    /// Allocate a fresh realm in the same heap. Keep its global object rooted
    /// while the host holds the realm; a RealmId alone does not keep it alive.
    pub fn new_realm(&mut self) -> Result<(RealmId, Value), JsError> {
        self.with_task(|vm| {
            let cells = vm.realm_bootstrap_cells;
            // The trusted installer creates a fixed set of builtin objects
            // on fresh prototypes and never calls guest code. Reserve room
            // before using the same installer as the primary realm.
            let bytes = vm.realm_bootstrap_bytes + std::mem::size_of::<RealmState>();
            vm.ensure_allocation(bytes)?;
            if vm.heap.live.saturating_add(cells) > vm.limits.max_heap_objects {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "Realm exceeds the shared heap cell budget",
                ));
            }
            vm.consume_work(cells)?;
            let before = vm.estimated_heap_bytes();
            let env = allocate_core(&mut vm.heap);
            let state = RealmState::new(&vm.heap, env, vm.realm.wasm_compilation_allowed);
            let global = Value::Object(state.global_object);
            vm.inactive_realms.insert(env, state);
            let realm = RealmId(env);
            vm.with_realm(realm, |vm| {
                vm.install_realm_builtins();
                Ok(())
            })?;
            debug_assert!(vm.estimated_heap_bytes().saturating_sub(before) <= bytes);
            Ok((realm, global))
        })
    }

    /// Execute a host operation in a realm without starting a new task or
    /// resetting the interpreter's limits. Values still belong to one heap.
    pub fn with_realm<T>(
        &mut self,
        realm: RealmId,
        body: impl FnOnce(&mut Self) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        if realm.0 == self.realm.global_env {
            return body(self);
        }
        let target = self
            .inactive_realms
            .remove(&realm.0)
            .ok_or_else(|| JsError::type_error("Invalid or collected JavaScript realm"))?;
        let previous = std::mem::replace(&mut self.realm, target);
        let previous_id = previous.global_env;
        self.inactive_realms.insert(previous_id, previous);
        self.realm_depth += 1;
        let scope = EnteredRealm {
            vm: self,
            previous: previous_id,
        };
        // Complete the throw in the defining realm before restoring the caller.
        // User-thrown values already have their own identity.
        body(scope.vm).map_err(|mut error| {
            if error.thrown.is_none() && !error.uncatchable() {
                match scope.vm.error_value(&error) {
                    Ok(value) => error.thrown = Some(value),
                    Err(allocation_error) => return allocation_error,
                }
            }
            error
        })
    }

    pub(crate) fn with_function_realm<T>(
        &mut self,
        function: ObjectId,
        body: impl FnOnce(&mut Self) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        if self.inactive_realms.is_empty() {
            return body(self);
        }
        let env = self.function_defining_environment(function)?;
        self.with_realm(RealmId(env), body)
    }

    // GetFunctionRealm follows internal target slots without invoking proxy traps.
    fn function_defining_environment(&self, mut function: ObjectId) -> Result<EnvId, JsError> {
        loop {
            match &self.heap.object(function)?.kind {
                ObjectKind::Proxy(proxy) => {
                    function = proxy
                        .pair
                        .ok_or_else(|| JsError::type_error("Revoked Proxy"))?
                        .0;
                }
                ObjectKind::Bound { target, .. } => {
                    function = target.as_object().expect("bound callable target");
                }
                ObjectKind::Native {
                    realm_environment, ..
                }
                | ObjectKind::PromiseFunction(_, realm_environment) => {
                    return Ok(*realm_environment);
                }
                ObjectKind::Function { environment, .. } => {
                    return Ok(self.environment_realm(*environment));
                }
                ObjectKind::Module(module) => {
                    return Ok(module
                        .environment
                        .map_or(self.realm.global_env, |env| self.environment_realm(env)));
                }
                _ => return Ok(self.realm.global_env),
            }
        }
    }

    pub(super) fn constructor_realm_prototype(
        &self,
        function: ObjectId,
        fallback: ObjectId,
    ) -> Result<ObjectId, JsError> {
        let env = self.function_defining_environment(function)?;
        let Some(foreign) = self.inactive_realms.get(&env) else {
            return Ok(fallback);
        };
        let prototype = self
            .realm
            .constructor_prototypes
            .iter()
            .position(|id| *id == Some(fallback))
            .and_then(|slot| foreign.constructor_prototypes[slot]);
        Ok(prototype.unwrap_or(fallback))
    }

    // Snapshot the built-in prototype identities during trusted bootstrap. A page
    // may later overwrite or delete any of the corresponding global properties.
    fn constructor_prototypes(&self) -> Vec<Option<ObjectId>> {
        fn data_object(heap: &Heap, object: ObjectId, key: &str) -> Option<ObjectId> {
            match &heap.object(object).ok()?.properties.get(&key.into())?.kind {
                PropertyKind::Data { value, .. } => value.as_object(),
                _ => None,
            }
        }
        let realm = &self.realm;
        let mut slots = vec![
            realm.async_function_prototype,
            realm.generator_function_prototype,
            realm.generator_prototype,
            realm.async_generator_function_prototype,
            realm.async_generator_prototype,
            realm.async_iterator_prototype,
        ];
        let groups: &[(&str, &[&str])] = &[
            (
                "",
                &[
                    "Object",
                    "Function",
                    "Array",
                    "Number",
                    "String",
                    "Boolean",
                    "Date",
                    "RegExp",
                    "Error",
                    "TypeError",
                    "ReferenceError",
                    "SyntaxError",
                    "RangeError",
                    "EvalError",
                    "URIError",
                    "AggregateError",
                    "SuppressedError",
                    "Promise",
                    "Map",
                    "Set",
                    "WeakMap",
                    "WeakSet",
                    "WeakRef",
                    "FinalizationRegistry",
                    "Iterator",
                    "ArrayBuffer",
                    "SharedArrayBuffer",
                    "DataView",
                    "Int8Array",
                    "Uint8Array",
                    "Uint8ClampedArray",
                    "Int16Array",
                    "Uint16Array",
                    "Int32Array",
                    "Uint32Array",
                    "Float16Array",
                    "Float32Array",
                    "Float64Array",
                    "BigInt64Array",
                    "BigUint64Array",
                    "DisposableStack",
                    "AsyncDisposableStack",
                ],
            ),
            (
                "Intl",
                &[
                    "Locale",
                    "Collator",
                    "ListFormat",
                    "DisplayNames",
                    "NumberFormat",
                    "DateTimeFormat",
                    "RelativeTimeFormat",
                    "PluralRules",
                    "Segmenter",
                    "DurationFormat",
                ],
            ),
            ("WebAssembly", &["Module", "Instance", "Memory"]),
        ];
        for &(namespace, names) in groups {
            let container = if namespace.is_empty() {
                Some(realm.global_object)
            } else {
                data_object(&self.heap, realm.global_object, namespace)
            };
            slots.extend(names.iter().map(|name| {
                let class = data_object(&self.heap, container?, name)?;
                data_object(&self.heap, class, "prototype")
            }));
        }
        slots
    }

    fn environment_realm(&self, mut environment: EnvId) -> EnvId {
        while let Some(parent) = self.heap.environment(environment).parent {
            environment = parent;
        }
        environment
    }

    pub(super) fn install_realm_builtins(&mut self) {
        let first_root = super::RootId(self.next_root);
        self.initializing = true;
        crate::builtins::install(self);
        self.initializing = false;
        let mut intrinsics = self
            .roots
            .split_off(&first_root)
            .into_values()
            .filter_map(|value| value.as_object())
            .collect::<Vec<_>>();
        self.realm.constructor_prototypes = self.constructor_prototypes();
        let realm = &self.realm;
        intrinsics.extend(realm.constructor_prototypes.iter().flatten().copied());
        intrinsics.extend(
            [
                realm.async_function_prototype,
                realm.generator_function_prototype,
                realm.generator_prototype,
                realm.async_generator_function_prototype,
                realm.async_generator_prototype,
                realm.async_iterator_prototype,
                realm.async_from_sync_prototype,
                realm.regexp_prototype,
                realm.regexp_constructor,
                realm.regexp_exec,
                realm.regexp_iterator_prototype,
                realm.bigint_prototype,
                realm.intrinsic_eval,
                realm.throw_type_error,
                realm.promise_constructor,
                realm.promise_prototype,
                realm.symbol_prototype,
                realm.iterator_prototype,
                realm.array_iterator_prototype,
                realm.string_iterator_prototype,
            ]
            .into_iter()
            .flatten(),
        );
        intrinsics.extend(realm.error_prototypes.values().copied());
        intrinsics.extend(realm.intl_prototypes.iter().map(|(_, id)| *id));
        self.heap
            .attach_realm_intrinsics(realm.global_env, intrinsics);
    }
}

#[cfg(test)]
fn allocate_additional_core(vm: &mut crate::Vm) -> Result<EnvId, crate::JsError> {
    // Fixed seed: seven prototypes, one binding object, one environment.
    // This isolates tests of the core's tracing from builtin installation.
    const CELLS: usize = 9;
    const BYTES: usize = CELLS * Heap::EMPTY_CELL_BYTES + Heap::REALM_CORE_BYTES;
    if vm.heap.live.saturating_add(CELLS) > vm.limits.max_heap_objects {
        return Err(crate::JsError::new(
            crate::ErrorKind::HeapLimit,
            "Realm core exceeds the shared heap cell budget",
        ));
    }
    vm.ensure_allocation(BYTES)?;
    vm.consume_work(CELLS)?;
    Ok(allocate_core(&mut vm.heap))
}

#[cfg(test)]
mod entry;
#[cfg(test)]
mod tests;
