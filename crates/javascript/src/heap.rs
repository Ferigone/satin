use crate::{
    ErrorKind, JsError, JsString, ObjectId, SymbolId, Value, Vm, bytecode::Code,
    vm::realm::CoreIntrinsics,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) type NativeCallback = Rc<dyn Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError>>;
pub(crate) type NativeConstruct =
    Rc<dyn Fn(&mut Vm, Value, &[Value], Value) -> Result<Value, JsError>>;
#[derive(Clone)]
pub(crate) enum PropertyKind {
    Data {
        value: Value,
        writable: bool,
    },
    Accessor {
        get: Option<Value>,
        set: Option<Value>,
    },
}
#[derive(Clone)]
pub(crate) struct Property {
    pub kind: PropertyKind,
    pub enumerable: bool,
    pub configurable: bool,
}
impl Property {
    pub fn data(value: Value) -> Self {
        Self {
            kind: PropertyKind::Data {
                value,
                writable: true,
            },
            enumerable: true,
            configurable: true,
        }
    }
}
pub(crate) enum ObjectKind {
    DeferredTemporal,
    InitializingTemporal,
    Ordinary,
    Arguments {
        environment: Option<EnvId>,
        mapped: BTreeMap<u32, JsString>,
    },
    RawJson(JsString),
    Proxy(crate::proxy::Proxy),
    Intl,
    Array {
        length: u32,
        length_writable: bool,
    },
    Function {
        code: Rc<Code>,
        environment: EnvId,
        referrer: Option<crate::dynamic_import::SourceReferrer>,
    },
    Native {
        initial_name: JsString,
        callback: NativeCallback,
        construct: Option<NativeConstruct>,
        // A native callback has no lexical environment to trace automatically.
        realm_environment: EnvId,
    },
    Bound {
        target: Value,
        this: Value,
        args: Vec<Value>,
    },
    Boxed(Value),
    Date(f64),
    ArrayBuffer(crate::binary::Buffer),
    SharedArrayBuffer(ObjectId),
    TypedArray(crate::binary::TypedArray),
    DataView(crate::binary::View),
    RegExp(Rc<crate::regexp::Program>, crate::regexp::Origin),
    RegExpStringIterator(crate::regexp::StringIterator),
    Error(ErrorKind, JsString),
    Promise(crate::promise::PromiseData),
    PromiseFunction(crate::promise::PromiseFunction, EnvId),
    Iterator(crate::iterator::IteratorData),
    DisposableStack {
        asynchronous: bool,
        disposed: bool,
    },
    Collection(Box<crate::collections::CollectionData>),
    CollectionIterator(crate::collections::CollectionIterator),
    WeakCollection(Box<crate::collections::weak::WeakData>),
    WeakRef(Option<Value>),
    FinalizationRegistry(Box<crate::collections::weak_ref::Registry>),
    PromiseAggregate(crate::promise::AggregateData),
    Generator(crate::generator::GeneratorState),
    AsyncGenerator(crate::generator::AsyncGenerator),
    AsyncFromSyncIterator(crate::async_iterator::AsyncFromSyncIterator),
    Module(Box<crate::modules::ModuleData>),
    ModuleNamespace(crate::modules::ModuleNamespace),
    WasmModule(Rc<crate::wasm::Module>),
    WasmInstance,
    WasmMemory(crate::wasm::Memory),
}
pub(crate) struct Object {
    /// Opaque host metadata is never script-visible and contains no JS handles.
    pub host_data: Option<Rc<dyn std::any::Any>>,
    pub platform_object: bool,
    pub indexed_properties: Option<Rc<dyn crate::HostIndexedProperties>>,
    pub named_properties: Option<Rc<dyn crate::HostNamedProperties>>,
    pub home_object: Option<ObjectId>,
    pub class_data: Option<Rc<crate::classes::ClassData>>,
    pub private_elements: BTreeMap<crate::private::PrivateId, crate::private::PrivateElement>,
    pub private_elements_forbidden: bool,
    pub host_slots: BTreeMap<u32, Value>,
    pub kind: ObjectKind,
    pub prototype: Option<ObjectId>,
    pub properties: BTreeMap<JsString, Property>,
    pub order: Vec<JsString>,
    pub symbols: BTreeMap<SymbolId, Property>,
    pub symbol_order: Vec<SymbolId>,
    pub extensible: bool,
    pub immutable_prototype: bool,
}
impl Object {
    pub fn is_platform_object(&self) -> bool {
        self.platform_object
            || self.private_elements_forbidden
            || self.host_data.is_some()
            || !self.host_slots.is_empty()
            || self.indexed_properties.is_some()
            || self.named_properties.is_some()
    }

    pub fn new(kind: ObjectKind, prototype: Option<ObjectId>) -> Self {
        Self {
            host_data: None,
            platform_object: false,
            indexed_properties: None,
            named_properties: None,
            home_object: None,
            class_data: None,
            private_elements: BTreeMap::new(),
            private_elements_forbidden: false,
            host_slots: BTreeMap::new(),
            kind,
            prototype,
            properties: BTreeMap::new(),
            order: Vec::new(),
            symbols: BTreeMap::new(),
            symbol_order: Vec::new(),
            extensible: true,
            immutable_prototype: false,
        }
    }
    pub fn insert(&mut self, key: JsString, property: Property) {
        if !self.properties.contains_key(&key) && array_index(&key).is_none() {
            self.order.push(key.clone());
        }
        self.properties.insert(key, property);
    }
    pub fn keys(&self, enumerable_only: bool) -> Vec<JsString> {
        let mut indices: Vec<(u32, JsString)> = self
            .properties
            .iter()
            .filter(|(_, p)| !enumerable_only || p.enumerable)
            .filter_map(|(k, _)| array_index(k).map(|n| (n, k.clone())))
            .collect();
        indices.sort_by_key(|p| p.0);
        let mut out: Vec<_> = indices.into_iter().map(|(_, k)| k).collect();
        // These non-enumerable own properties exist from construction.
        if !enumerable_only
            && matches!(
                self.kind,
                ObjectKind::Array { .. } | ObjectKind::Boxed(Value::String(_))
            )
        {
            out.push("length".into());
        }
        out.extend(
            self.order
                .iter()
                .filter(|k| array_index(k).is_none())
                .filter(|k| {
                    self.properties
                        .get(*k)
                        .is_some_and(|p| !enumerable_only || p.enumerable)
                })
                .cloned(),
        );
        out
    }
}
pub(crate) fn array_index(key: &JsString) -> Option<u32> {
    let u = key.as_utf16();
    if u.is_empty() || u.len() > 10 || (u.len() > 1 && u[0] == 48) {
        return None;
    }
    let mut n = 0u32;
    for d in u {
        if !(48..=57).contains(d) {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((*d - 48) as u32)?;
    }
    if n == u32::MAX { None } else { Some(n) }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub(crate) struct EnvId(pub ObjectId);
#[derive(Clone)]
pub(crate) struct Binding {
    pub value: Value,
    pub mutable: bool,
    pub initialized: bool,
    pub lexical: bool,
    // [[Strict]] of an immutable binding; false for a named function expression.
    pub strict_immutable: bool,
    pub deletable: bool,
}
impl Binding {
    pub(crate) fn read(&self, key: &JsString) -> Result<Value, JsError> {
        if self.initialized {
            Ok(self.value.clone())
        } else {
            Err(JsError::new(
                ErrorKind::ReferenceError,
                format!("Cannot access '{key}' before initialization"),
            ))
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum EnvKind {
    Global { binding_object: ObjectId },
    With { binding_object: ObjectId },
    Function,
    Block,
    SimpleCatch,
    SloppyEval,
    // A Console command resolves helper names here and declares script bindings in its parent.
    Inspector,
    Module,
}
struct RealmIntrinsics {
    core: CoreIntrinsics,
    installed: Vec<ObjectId>,
}

pub(crate) struct Environment {
    pub parent: Option<EnvId>,
    pub kind: EnvKind,
    // Only bootstrapped global environments own intrinsic roots.
    realm_intrinsics: Option<Box<RealmIntrinsics>>,
    pub bindings: BTreeMap<JsString, Binding>,
    pub pending_arguments: Option<Box<[Value]>>,
    pub this: Option<Value>,
    pub this_initialized: bool,
    pub function: Option<Value>,
    pub home_object: Option<ObjectId>,
    pub new_target: Option<Value>,
    pub class_initializer: bool,
    pub module: Option<ObjectId>,
    pub aliases: BTreeMap<JsString, (EnvId, JsString)>,
    pub private_names: BTreeMap<JsString, crate::private::PrivateId>,
}
impl Environment {
    pub(crate) fn has_lazy_arguments(&self, key: &JsString) -> bool {
        self.pending_arguments.is_some()
            && key.as_utf16().iter().copied().eq("arguments".encode_utf16())
    }

    pub fn new_global(binding_object: ObjectId, global_this: ObjectId) -> Self {
        Self::new(
            None,
            EnvKind::Global { binding_object },
            Some(Value::Object(global_this)),
        )
    }

    pub fn new(parent: Option<EnvId>, kind: EnvKind, this: Option<Value>) -> Self {
        Self {
            parent,
            kind,
            realm_intrinsics: None,
            bindings: BTreeMap::new(),
            pending_arguments: None,
            this,
            this_initialized: true,
            function: None,
            home_object: None,
            new_target: None,
            class_initializer: false,
            module: None,
            aliases: BTreeMap::new(),
            private_names: BTreeMap::new(),
        }
    }
}
pub(crate) enum Cell {
    Object(Object),
    Environment(Environment),
}
struct Slot {
    generation: u32,
    marked: bool,
    cell: Option<Cell>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TraceTarget {
    Object(ObjectId),
    Symbol(usize),
}

impl TraceTarget {
    pub(crate) fn of_value(value: &Value) -> Option<Self> {
        match value {
            Value::Object(id) => Some(Self::Object(*id)),
            Value::Symbol(symbol) if symbol.0.registry_key.is_none() => {
                Some(Self::Symbol(symbol.id()))
            }
            _ => None,
        }
    }
}

pub(crate) struct TraceEdges {
    objects: Vec<ObjectId>,
    symbols: Vec<usize>,
    track_symbols: bool,
}

impl TraceEdges {
    pub(crate) fn push(&mut self, object: ObjectId) {
        self.objects.push(object);
    }

    pub(crate) fn extend(&mut self, objects: impl IntoIterator<Item = ObjectId>) {
        self.objects.extend(objects);
    }

    pub(crate) fn symbol(&mut self, symbol: &SymbolId) {
        if self.track_symbols && symbol.0.registry_key.is_none() {
            self.symbols.push(symbol.id());
        }
    }

    pub(crate) fn value(&mut self, value: &Value) {
        match value {
            Value::Object(id) => self.push(*id),
            Value::Symbol(symbol) => self.symbol(symbol),
            _ => {}
        }
    }

    fn target(&mut self, target: TraceTarget) {
        match target {
            TraceTarget::Object(id) => self.push(id),
            TraceTarget::Symbol(symbol) => self.symbols.push(symbol),
        }
    }

    fn pop(&mut self) -> Option<TraceTarget> {
        self.symbols
            .pop()
            .map(TraceTarget::Symbol)
            .or_else(|| self.objects.pop().map(TraceTarget::Object))
    }
}

pub(crate) struct Heap {
    id: u64,
    slots: Vec<Slot>,
    free: Vec<u32>,
    pub live: usize,
    weak_collections: usize,
    /// Conservative allocation-since-collection estimate, including containers.
    pub estimated_bytes: usize,
}
impl Default for Heap {
    fn default() -> Self {
        static NEXT_HEAP: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_HEAP
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                (id < ObjectId::HTML_DDA).then(|| id + 1)
            })
            .expect("heap identifier space exhausted");
        Self {
            id,
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            weak_collections: 0,
            estimated_bytes: 0,
        }
    }
}
impl Heap {
    pub(crate) const EMPTY_CELL_BYTES: usize = 256;
    pub(crate) const REALM_CORE_BYTES: usize = std::mem::size_of::<RealmIntrinsics>() + 128;

    pub fn allocate(&mut self, cell: Cell) -> ObjectId {
        self.estimated_bytes = self.estimated_bytes.saturating_add(cell_bytes(&cell));
        if matches!(&cell, Cell::Object(object) if matches!(object.kind, ObjectKind::WeakCollection(_) | ObjectKind::WeakRef(_) | ObjectKind::FinalizationRegistry(_)))
        {
            self.weak_collections += 1;
        }
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.cell = Some(cell);
            slot.marked = false;
            ObjectId {
                heap_id: self.id,
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 1,
                marked: false,
                cell: Some(cell),
            });
            ObjectId {
                heap_id: self.id,
                index,
                generation: 1,
            }
        }
    }
    fn slot(&self, id: ObjectId) -> Option<&Slot> {
        if !id.belongs_to(self.id) {
            return None;
        }
        let slot = self.slots.get(id.index as usize)?;
        (slot.generation == id.generation).then_some(slot)
    }
    fn slot_mut(&mut self, id: ObjectId) -> Option<&mut Slot> {
        if !id.belongs_to(self.id) {
            return None;
        }
        let slot = self.slots.get_mut(id.index as usize)?;
        (slot.generation == id.generation).then_some(slot)
    }
    fn cell(&self, id: ObjectId) -> Option<&Cell> {
        self.slot(id)?.cell.as_ref()
    }
    fn cell_mut(&mut self, id: ObjectId) -> Option<&mut Cell> {
        self.slot_mut(id)?.cell.as_mut()
    }
    pub fn object(&self, id: ObjectId) -> Result<&Object, JsError> {
        match self.cell(id) {
            Some(Cell::Object(o)) => Ok(o),
            _ => Err(JsError::type_error(
                "Invalid or collected object handle; externally held values must be rooted",
            )),
        }
    }
    pub fn object_mut(&mut self, id: ObjectId) -> Result<&mut Object, JsError> {
        match self.cell_mut(id) {
            Some(Cell::Object(o)) => Ok(o),
            _ => Err(JsError::type_error(
                "Invalid or collected object handle; externally held values must be rooted",
            )),
        }
    }
    pub(crate) fn distinct_objects_mut(
        &mut self,
        first: ObjectId,
        second: ObjectId,
    ) -> Result<(&mut Object, &mut Object), JsError> {
        let invalid = || JsError::type_error("Invalid or overlapping object handles");
        if !first.belongs_to(self.id) || !second.belongs_to(self.id) {
            return Err(invalid());
        }
        let [a, b] = self
            .slots
            .get_disjoint_mut([first.index as usize, second.index as usize])
            .map_err(|_| invalid())?;
        if a.generation != first.generation || b.generation != second.generation {
            return Err(invalid());
        }
        match (a.cell.as_mut(), b.cell.as_mut()) {
            (Some(Cell::Object(a)), Some(Cell::Object(b))) => Ok((a, b)),
            _ => Err(invalid()),
        }
    }
    pub fn environment(&self, id: EnvId) -> &Environment {
        match self.cell(id.0) {
            Some(Cell::Environment(env)) => env,
            _ => panic!("internal invariant: reachable environment"),
        }
    }
    pub fn environment_mut(&mut self, id: EnvId) -> &mut Environment {
        match self.cell_mut(id.0) {
            Some(Cell::Environment(env)) => env,
            _ => panic!("internal invariant: reachable environment"),
        }
    }
    pub fn allocate_env(&mut self, env: Environment) -> EnvId {
        EnvId(self.allocate(Cell::Environment(env)))
    }
    pub(crate) fn attach_core_intrinsics(&mut self, env: EnvId, intrinsics: CoreIntrinsics) {
        let environment = self.environment_mut(env);
        let EnvKind::Global { binding_object } = environment.kind else {
            unreachable!("only global environments own realm intrinsics");
        };
        assert!(environment.realm_intrinsics.is_none());
        environment.realm_intrinsics = Some(Box::new(RealmIntrinsics {
            core: intrinsics,
            installed: Vec::new(),
        }));
        // A retained global object keeps its lexical environment alive even
        // after author code deletes all of its builtin properties.
        self.object_mut(binding_object)
            .expect("global binding object")
            .host_slots
            .insert(u32::MAX, Value::Object(env.0));
        self.estimated_bytes = self.estimated_bytes.saturating_add(Self::REALM_CORE_BYTES);
    }
    pub(crate) fn realm_core(&self, env: EnvId) -> Option<(ObjectId, &CoreIntrinsics)> {
        // A realm handle is not a root. Validate heap identity, generation and
        // cell kind before accepting it; do not use the internal env accessor.
        let Cell::Environment(environment) = self.cell(env.0)? else {
            return None;
        };
        let EnvKind::Global { binding_object } = environment.kind else {
            return None;
        };
        Some((binding_object, &environment.realm_intrinsics.as_ref()?.core))
    }
    pub(crate) fn attach_realm_intrinsics(&mut self, env: EnvId, intrinsics: Vec<ObjectId>) {
        self.estimated_bytes += intrinsics.capacity() * std::mem::size_of::<ObjectId>();
        self.environment_mut(env)
            .realm_intrinsics
            .as_mut()
            .expect("realm core installed")
            .installed = intrinsics;
    }
    pub(crate) fn trace_roots(&self) -> TraceEdges {
        TraceEdges {
            objects: Vec::new(),
            symbols: Vec::new(),
            track_symbols: self.weak_collections != 0,
        }
    }

    fn target_marked(&self, target: TraceTarget, symbols: &BTreeSet<usize>) -> bool {
        match target {
            TraceTarget::Object(id) => self.slot(id).is_some_and(|slot| slot.marked),
            TraceTarget::Symbol(symbol) => symbols.contains(&symbol),
        }
    }

    pub(crate) fn collect(
        &mut self,
        mut pending: TraceEdges,
        conditional_edges: impl Iterator<Item = (ObjectId, ObjectId)>,
    ) -> (usize, Vec<ObjectId>) {
        let mut symbols = BTreeSet::new();
        let mut weak_owners = Vec::new();
        let mut cleanup = Vec::new();
        // A conditional edge is either followed immediately or registered
        // once under its unmarked key. Later marks wake the waiting values.
        let mut waiting = BTreeMap::<TraceTarget, Vec<TraceTarget>>::new();
        for (key, value) in conditional_edges {
            waiting.entry(TraceTarget::Object(key)).or_default().push(TraceTarget::Object(value));
        }
        while let Some(target) = pending.pop() {
            match target {
                TraceTarget::Object(id) => {
                    let Some(slot) = self.slot_mut(id) else {
                        continue;
                    };
                    if slot.marked || slot.cell.is_none() {
                        continue;
                    }
                    slot.marked = true;
                    let cell = self.cell(id).unwrap();
                    Self::trace_cell(cell, &mut pending);
                    if let Cell::Object(object) = cell
                        && let ObjectKind::WeakCollection(data) = &object.kind
                    {
                        weak_owners.push(id);
                        for (key, value) in data.ephemerons() {
                            if self.target_marked(key, &symbols) {
                                pending.target(value);
                            } else {
                                waiting.entry(key).or_default().push(value);
                            }
                        }
                    }
                    if let Cell::Object(object) = cell && matches!(object.kind, ObjectKind::WeakRef(_) | ObjectKind::FinalizationRegistry(_)) {
                        weak_owners.push(id);
                    }
                }
                TraceTarget::Symbol(symbol) => {
                    if !symbols.insert(symbol) {
                        continue;
                    }
                }
            }
            if let Some(values) = waiting.remove(&target) {
                for value in values {
                    pending.target(value);
                }
            }
        }
        drop(waiting);
        // Keys are evaluated against the completed mark set, before sweeping
        // resets any mark bit or frees a slot generation.
        for owner in weak_owners {
            let object = self.object_mut(owner).unwrap();
            let mut kind = std::mem::replace(&mut object.kind, ObjectKind::Ordinary);
            let marked = |key| self.target_marked(key, &symbols);
            match &mut kind {
                ObjectKind::WeakCollection(data) => data.retain_keys(marked),
                ObjectKind::WeakRef(target) => {
                    if target.as_ref().and_then(TraceTarget::of_value).is_some_and(|key| !marked(key)) {
                        *target = None;
                    }
                }
                ObjectKind::FinalizationRegistry(registry) => {
                    if registry.sweep(marked) { cleanup.push(owner); }
                }
                _ => unreachable!("marked weak owner"),
            }
            self.object_mut(owner).unwrap().kind = kind;
        }
        let before = self.live;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.cell.is_some() {
                if slot.marked {
                    slot.marked = false;
                } else {
                    if matches!(&slot.cell, Some(Cell::Object(object)) if matches!(object.kind, ObjectKind::WeakCollection(_) | ObjectKind::WeakRef(_) | ObjectKind::FinalizationRegistry(_)))
                    {
                        self.weak_collections -= 1;
                    }
                    slot.cell = None;
                    slot.generation = slot.generation.wrapping_add(1).max(1);
                    self.free.push(index as u32);
                    self.live -= 1;
                }
            }
        }
        self.estimated_bytes = self
            .slots
            .iter()
            .filter_map(|s| s.cell.as_ref())
            .map(cell_bytes)
            .sum();
        (before - self.live, cleanup)
    }

    fn trace_cell(cell: &Cell, pending: &mut TraceEdges) {
        match cell {
            Cell::Environment(env) => {
                if let EnvKind::Global { binding_object } | EnvKind::With{binding_object} = env.kind {
                    pending.push(binding_object);
                }
                if let Some(intrinsics) = &env.realm_intrinsics {
                    pending.extend(intrinsics.core.objects());
                    pending.extend(intrinsics.installed.iter().copied());
                }
                if let Some(p) = env.parent {
                    pending.push(p.0);
                }
                if let Some(v) = &env.this {
                    pending.value(v);
                }
                if let Some(v) = &env.function {
                    pending.value(v);
                }
                if let Some(v) = &env.new_target {
                    pending.value(v);
                }
                pending.extend(env.home_object);
                pending.extend(env.module);
                pending.extend(env.aliases.values().map(|(env, _)| env.0));
                if let Some(args) = &env.pending_arguments {
                    for value in args.iter() { pending.value(value); }
                }
                for b in env.bindings.values() {
                    pending.value(&b.value);
                }
            }
            Cell::Object(o) => {
                pending.extend(o.home_object);
                if let Some(data) = &o.class_data {
                    data.trace(pending);
                }
                for element in o.private_elements.values() {
                    element.visit(|value| pending.value(value));
                }
                if let Some(p) = o.prototype {
                    pending.push(p);
                }
                match &o.kind {
                    ObjectKind::Function {
                        environment,
                        referrer,
                        ..
                    } => {
                        pending.push(environment.0);
                        if let Some(crate::dynamic_import::SourceReferrer::Module(module)) =
                            referrer
                        {
                            pending.push(module.0);
                        }
                    }
                    ObjectKind::Native {
                        realm_environment, ..
                    } => pending.push(realm_environment.0),
                    ObjectKind::Proxy(proxy) => {
                        if let Some((target,handler))=proxy.pair {pending.push(target);pending.push(handler);}
                    }
                    ObjectKind::Bound { target, this, args } => {
                        pending.value(target);
                        pending.value(this);
                        for v in args {
                            pending.value(v);
                        }
                    }
                    ObjectKind::Arguments { environment, .. } => pending.extend(environment.map(|env| env.0)),
                    ObjectKind::Boxed(v) => pending.value(v),
                    ObjectKind::SharedArrayBuffer(backing) => pending.push(*backing),
                    ObjectKind::TypedArray(array) => pending.push(array.view.buffer),
                    ObjectKind::WasmMemory(memory) => pending.push(memory.buffer),
                    ObjectKind::DataView(view) => pending.push(view.buffer),
                    ObjectKind::RegExpStringIterator(crate::regexp::StringIterator::Active {
                        matcher,
                        ..
                    }) => pending.value(matcher),
                    ObjectKind::Promise(data) => data.trace(pending),
                    ObjectKind::PromiseFunction(function, realm) => {
                        pending.push(realm.0);
                        function.trace(pending);
                    }
                    ObjectKind::Iterator(data) => data.trace(pending),
                    ObjectKind::Collection(data) => data.trace(pending),
                    ObjectKind::CollectionIterator(data) => pending.extend(data.target),
                    ObjectKind::PromiseAggregate(data) => data.trace(pending),
                    ObjectKind::FinalizationRegistry(registry) => registry.trace(pending),
                    ObjectKind::Generator(state) => state.trace(pending),
                    ObjectKind::AsyncGenerator(state) => state.trace(pending),
                    ObjectKind::AsyncFromSyncIterator(state) => state.trace(pending),
                    ObjectKind::Module(data) => data.trace(pending),
                    ObjectKind::ModuleNamespace(data) => data.trace(pending),
                    _ => {}
                }
                for symbol in o.symbols.keys() {
                    pending.symbol(symbol);
                }
                for p in o.properties.values().chain(o.symbols.values()) {
                    match &p.kind {
                        PropertyKind::Data { value, .. } => pending.value(value),
                        PropertyKind::Accessor { get, set } => {
                            if let Some(v) = get {
                                pending.value(v);
                            }
                            if let Some(v) = set {
                                pending.value(v);
                            }
                        }
                    }
                }
                for value in o.host_slots.values() {
                    pending.value(value);
                }
            }
        }
    }
    pub fn live_cells(&self) -> impl Iterator<Item = &Cell> {
        self.slots.iter().filter_map(|s| s.cell.as_ref())
    }
}
fn cell_bytes(cell: &Cell) -> usize {
    Heap::EMPTY_CELL_BYTES.saturating_add(match cell {
        Cell::Object(object) => object
            .properties
            .keys()
            .map(|key| 192 + key.len() * 2)
            .sum::<usize>()
            .saturating_add(object.order.capacity() * std::mem::size_of::<JsString>())
            .saturating_add(match &object.kind {
                ObjectKind::Arguments { mapped, .. } => mapped.len() * 96,
                ObjectKind::ArrayBuffer(buffer) => buffer.retained_bytes(),
                ObjectKind::RegExp(program, _) => program.retained_bytes(),
                ObjectKind::Module(data) => data.extra_bytes(),
                ObjectKind::WasmModule(module) => module.retained_bytes(),
                ObjectKind::ModuleNamespace(data) => data.exports.len() * 256,
                ObjectKind::Bound { args, .. } => args.capacity() * std::mem::size_of::<Value>(),
                ObjectKind::Collection(data) => data.extra_bytes(),
                ObjectKind::WeakCollection(data) => data.extra_bytes(),
                ObjectKind::Promise(data) => {
                    data.reactions.capacity() * std::mem::size_of::<crate::promise::ReactionPair>()
                        + data.extra_bytes()
                }
                ObjectKind::FinalizationRegistry(registry) => registry.bytes(),
                ObjectKind::PromiseAggregate(data) => {
                    data.values.capacity() * std::mem::size_of::<Value>()
                        + data.keys.capacity() * std::mem::size_of::<crate::PropertyKey>()
                }
                ObjectKind::Generator(state) => state.extra_bytes(),
                ObjectKind::Iterator(state) => state.extra_bytes(),
                ObjectKind::AsyncGenerator(state) => state.extra_bytes(),
                _ => 0,
            })
            .saturating_add(
                object
                    .class_data
                    .as_deref()
                    .map_or(0, crate::classes::ClassData::extra_bytes),
            )
            .saturating_add(
                object.private_elements.len() * crate::private::PrivateElement::ESTIMATED_BYTES,
            )
            .saturating_add(object.host_slots.len() * 128)
            .saturating_add(object.symbol_order.capacity() * std::mem::size_of::<SymbolId>())
            .saturating_add(object.symbols.len() * 256),
        Cell::Environment(env) => {
            env.pending_arguments.as_deref().map_or(0, std::mem::size_of_val) + env.bindings
                .keys()
                .map(|key| 192 + key.len() * 2)
                .sum::<usize>()
                + env
                    .aliases
                    .iter()
                    .map(|(name, (_, target))| 256 + (name.len() + target.len()) * 2)
                    .sum::<usize>()
                + env
                    .private_names
                    .keys()
                    .map(crate::private::name_bytes)
                    .sum::<usize>()
                + env.realm_intrinsics.as_ref().map_or(0, |intrinsics| {
                    std::mem::size_of::<RealmIntrinsics>()
                        + intrinsics.installed.capacity() * std::mem::size_of::<ObjectId>()
                })
        }
    })
}
