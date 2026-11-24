//! Traced source-text module records, graph linking, live aliases and namespaces.
mod evaluation;
use crate::{
    ErrorKind, JsError, JsString, ObjectId, RootId, Value, Vm,
    bytecode::{self, Chunk},
    heap::{
        Binding, Cell, EnvId, EnvKind, Environment, Object, ObjectKind, Property, PropertyKind,
        TraceEdges,
    },
    parser::{
        self,
        modules::{ExportEntry, ImportEntry},
    },
    vm::Reference,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ModuleId(pub(crate) ObjectId, pub(crate) u64);
static NEXT_MODULE_REALM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub(crate) fn module_realm_id() -> u64 {
    NEXT_MODULE_REALM.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ModuleRequest {
    pub specifier: JsString,
    pub attributes: BTreeMap<JsString, JsString>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModuleType {
    #[default]
    JavaScript,
    Json,
    Text,
    Bytes,
}

impl ModuleRequest {
    pub fn module_type(&self) -> Result<ModuleType, JsError> {
        if self.attributes.is_empty() {
            return Ok(ModuleType::JavaScript);
        }
        if self.attributes.len() == 1
            && let Some(value) = self.attributes.get(&JsString::from("type"))
        {
            if value == &JsString::from("json") {
                return Ok(ModuleType::Json);
            }
            if value == &JsString::from("text") {
                return Ok(ModuleType::Text);
            }
            if value == &JsString::from("bytes") {
                return Ok(ModuleType::Bytes);
            }
        }
        Err(JsError::type_error("Unsupported import attribute"))
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleStatus {
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    Evaluated,
    Errored,
}
#[derive(Clone, Default)]
pub(crate) struct ModuleCode {
    pub body: Rc<Chunk>,
    pub requests: Vec<ModuleRequest>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    import_indices: BTreeMap<JsString, usize>,
    export_indices: BTreeMap<JsString, usize>,
    star_requests: Vec<usize>,
}
pub(crate) struct ModuleData {
    pub identifier: JsString,
    pub code: Rc<ModuleCode>,
    pub status: ModuleStatus,
    pub environment: Option<EnvId>,
    synthetic_default: Option<Value>,
    pub dependencies: Vec<ModuleId>,
    pub namespace: Option<Value>,
    pub meta: Option<Value>,
    pub evaluation: Option<Value>,
    pub error: Option<Rc<JsError>>,
    pub cycle_root: Option<ModuleId>,
    async_order: Option<u64>,
    pending_dependencies: usize,
    async_parents: Vec<ModuleId>,
}
pub(crate) struct ModuleNamespace {
    pub module: ModuleId,
    pub exports: BTreeMap<JsString, ExportTarget>,
}
impl ModuleData {
    pub fn trace(&self, p: &mut TraceEdges) {
        if let Some(default) = &self.synthetic_default {
            p.value(default);
        }
        p.extend(self.environment.map(|e| e.0));
        p.extend(self.dependencies.iter().map(|m| m.0));
        p.extend(self.cycle_root.map(|m| m.0));
        p.extend(self.async_parents.iter().map(|m| m.0));
        for value in self
            .namespace
            .iter()
            .chain(&self.meta)
            .chain(&self.evaluation)
            .chain(self.error.as_ref().and_then(|error| error.thrown.as_ref()))
        {
            p.value(value);
        }
    }
    pub fn strings(&self, s: &mut Vec<JsString>) {
        s.push(self.identifier.clone());
        if let Some(Value::String(reason)) =
            self.error.as_ref().and_then(|error| error.thrown.as_ref())
        {
            s.push(reason.clone());
        }
        if let Some(Value::String(value)) = &self.synthetic_default {
            s.push(value.clone());
        }
        for request in &self.code.requests {
            s.push(request.specifier.clone());
            for (key, value) in &request.attributes {
                s.extend([key.clone(), value.clone()]);
            }
        }
        for import in &self.code.imports {
            s.push(import.local.clone());
            s.extend(import.imported.iter().cloned());
        }
        for export in &self.code.exports {
            match export {
                ExportEntry::Local { exported, local } => {
                    s.extend([exported.clone(), local.clone()])
                }
                ExportEntry::Indirect {
                    exported, imported, ..
                } => s.extend([exported.clone(), imported.clone()]),
                ExportEntry::Namespace { exported, .. } => s.push(exported.clone()),
                ExportEntry::Star { .. } => {}
            }
        }
    }
    pub fn extra_bytes(&self) -> usize {
        self.code
            .requests
            .iter()
            .map(|request| 256 + 128 * request.attributes.len())
            .sum::<usize>()
            + self.code.imports.len() * 384
            + self.code.exports.len() * 384
            + self.dependencies.capacity() * 128
            + self.async_parents.capacity() * std::mem::size_of::<ModuleId>()
            + self
                .error
                .as_ref()
                .map_or(0, |error| error.message.capacity() + 128)
    }
}
impl ModuleNamespace {
    pub fn trace(&self, p: &mut TraceEdges) {
        p.push(self.module.0);
        for target in self.exports.values() {
            p.push(match target {
                ExportTarget::Binding(env, _) => env.0,
                ExportTarget::Namespace(module) => module.0,
            });
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum Resolution {
    Missing,
    Ambiguous,
    Resolved(ExportTarget),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExportTarget {
    Binding(EnvId, JsString),
    Namespace(ModuleId),
}
impl Vm {
    fn validate_module_realm(&self, id: ModuleId) -> Result<(), JsError> {
        if id.1 == self.realm.module_realm {
            Ok(())
        } else {
            Err(JsError::type_error("Module handle belongs to another VM"))
        }
    }
    pub(crate) fn module_data(&self, id: ModuleId) -> Result<&ModuleData, JsError> {
        self.validate_module_realm(id)?;
        match &self.heap.object(id.0)?.kind {
            ObjectKind::Module(data) => Ok(data),
            _ => Err(JsError::type_error("Invalid module handle")),
        }
    }
    fn module_data_mut(&mut self, id: ModuleId) -> Result<&mut ModuleData, JsError> {
        self.validate_module_realm(id)?;
        match &mut self.heap.object_mut(id.0)?.kind {
            ObjectKind::Module(data) => Ok(data),
            _ => Err(JsError::type_error("Invalid module handle")),
        }
    }
    pub fn parse_module(&mut self, source: &str, identifier: &str) -> Result<ModuleId, JsError> {
        let code = compile_module(source, self.compilation_budget(source)?)?;
        self.new_module_record(identifier, code, None)
    }

    /// Parse a fetched module according to its host-validated type. Binary imports
    /// retain their exact octets, including a possible UTF-8 byte-order mark.
    pub fn parse_module_bytes(
        &mut self,
        source: &[u8],
        identifier: &str,
        kind: ModuleType,
    ) -> Result<ModuleId, JsError> {
        if source.len() > self.limits.max_source_bytes {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Module source exceeds configured byte limit",
            ));
        }
        let value = if kind == ModuleType::Bytes {
            self.new_immutable_uint8_array(source)?
        } else {
            let bytes = source.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(source);
            self.ensure_allocation(bytes.len().saturating_mul(3))?;
            let source = String::from_utf8_lossy(bytes);
            if kind == ModuleType::JavaScript {
                return self.parse_module(&source, identifier);
            }
            self.ensure_string_allocation(source.encode_utf16().count())?;
            let text = JsString::from(source.as_ref());
            if kind == ModuleType::Json {
                self.parse_json(&text)?
            } else {
                self.account_value(Value::String(text))?
            }
        };
        let local = JsString::from("*default*");
        let name = JsString::from("default");
        let mut body = Chunk {
            strict: true,
            ..Chunk::default()
        };
        body.scope.lexical.push((local.clone(), false));
        let code = ModuleCode {
            body: Rc::new(body),
            exports: vec![ExportEntry::Local {
                local,
                exported: name.clone(),
            }],
            export_indices: BTreeMap::from([(name, 0)]),
            ..ModuleCode::default()
        };
        self.new_module_record(identifier, Rc::new(code), Some(value))
    }

    fn new_module_record(
        &mut self,
        identifier: &str,
        code: Rc<ModuleCode>,
        synthetic_default: Option<Value>,
    ) -> Result<ModuleId, JsError> {
        self.ensure_string_allocation(identifier.encode_utf16().count())?;
        self.retain_compiled_chunk(&code.body)?;
        let identifier = JsString::from(identifier);
        self.account_value(Value::String(identifier.clone()))?;
        self.ensure_allocation(
            512 + code
                .requests
                .iter()
                .map(|request| 256 + 128 * request.attributes.len())
                .sum::<usize>()
                + code.imports.len() * 384
                + code.exports.len() * 384,
        )?;
        if self.heap.live >= self.limits.max_heap_objects {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Heap cell budget exhausted",
            ));
        }
        let object = self.heap.allocate(Cell::Object(Object::new(
            ObjectKind::Module(Box::new(ModuleData {
                identifier,
                code,
                status: ModuleStatus::Unlinked,
                environment: None,
                synthetic_default,
                dependencies: Vec::new(),
                namespace: None,
                meta: None,
                evaluation: None,
                error: None,
                cycle_root: None,
                async_order: None,
                pending_dependencies: 0,
                async_parents: Vec::new(),
            })),
            None,
        )));
        Ok(ModuleId(object, self.realm.module_realm))
    }
    pub fn check_module_syntax(&mut self, source: &str) -> Result<(), JsError> {
        let code = compile_module(source, self.compilation_budget(source)?)?;
        self.ensure_allocation(bytecode::retained_size_with_chunks(
            Vec::new(),
            vec![code.body.clone()],
        ))
    }
    pub fn module_identifier(&self, id: ModuleId) -> Result<JsString, JsError> {
        Ok(self.module_data(id)?.identifier.clone())
    }
    pub fn module_status(&self, id: ModuleId) -> Result<ModuleStatus, JsError> {
        Ok(self.module_data(self.module_cycle_root(id)?)?.status)
    }
    pub fn module_requests(&self, id: ModuleId) -> Result<Vec<ModuleRequest>, JsError> {
        Ok(self.module_data(id)?.code.requests.clone())
    }
    pub fn root_module(&mut self, id: ModuleId) -> Result<RootId, JsError> {
        self.module_data(id)?;
        Ok(self.root(Value::Object(id.0)))
    }
    pub fn module_import_meta(&mut self, id: ModuleId) -> Result<Value, JsError> {
        if let Some(meta) = &self.module_data(id)?.meta {
            return Ok(meta.clone());
        }
        self.ensure_allocation(512)?;
        let meta = self.new_object_with_prototype(Value::Null)?;
        let url = self.module_data(id)?.identifier.clone();
        self.define_data_property(meta.clone(), "url", Value::String(url), true, true, true)?;
        self.module_data_mut(id)?.meta = Some(meta.clone());
        Ok(meta)
    }
    pub(crate) fn import_meta_for_env(&mut self, mut env: EnvId) -> Result<Value, JsError> {
        loop {
            let e = self.heap.environment(env);
            if let Some(id) = e.module {
                return self.module_import_meta(ModuleId(id, self.realm.module_realm));
            }
            env = e.parent.ok_or_else(|| {
                JsError::new(ErrorKind::SyntaxError, "import.meta outside a module")
            })?;
        }
    }
    pub(crate) fn read_import(&mut self, env: EnvId, key: &JsString) -> Result<Value, JsError> {
        self.tick()?;
        let (target, name) = self
            .heap
            .environment(env)
            .aliases
            .get(key)
            .cloned()
            .ok_or_else(|| JsError::new(ErrorKind::InternalError, "Missing import binding"))?;
        self.read_reference(Reference::Binding(target, name))
    }
    fn dependency(&mut self, id: ModuleId, index: &usize) -> Result<ModuleId, JsError> {
        self.tick()?;
        self.module_data(id)?
            .dependencies
            .get(*index)
            .copied()
            .ok_or_else(|| JsError::new(ErrorKind::SyntaxError, "Unresolved module dependency"))
    }
    pub fn link_module<F>(&mut self, root: ModuleId, mut resolver: F) -> Result<(), JsError>
    where
        F: FnMut(&mut Vm, ModuleId, &JsString) -> Result<ModuleId, JsError>,
    {
        self.link_module_with_requests(root, |vm, importer, request| {
            if request.module_type()? != ModuleType::JavaScript {
                return Err(JsError::type_error(
                    "Module loader requires a typed resolver for this import",
                ));
            }
            resolver(vm, importer, &request.specifier)
        })
    }

    pub fn link_module_with_requests<F>(
        &mut self,
        root: ModuleId,
        mut resolver: F,
    ) -> Result<(), JsError>
    where
        F: FnMut(&mut Vm, ModuleId, &ModuleRequest) -> Result<ModuleId, JsError>,
    {
        self.with_task(|vm| {
            if vm.module_data(root)?.status != ModuleStatus::Unlinked {
                return if matches!(
                    vm.module_data(root)?.status,
                    ModuleStatus::Linked
                        | ModuleStatus::Evaluating
                        | ModuleStatus::Evaluated
                        | ModuleStatus::Errored
                ) {
                    Ok(())
                } else {
                    Err(JsError::new(
                        ErrorKind::TypeError,
                        "Module cannot be linked in its current state",
                    ))
                };
            }
            let mut newly = Vec::new();
            let result = vm.link_graph(root, &mut resolver, &mut newly);
            if result.is_err() {
                for id in newly {
                    if let Ok(data) = vm.module_data_mut(id) {
                        data.status = ModuleStatus::Unlinked;
                        data.environment = None;
                        data.namespace = None;
                        data.dependencies.clear();
                    }
                }
            }
            result
        })
    }
    fn link_graph<F>(
        &mut self,
        root: ModuleId,
        resolver: &mut F,
        newly: &mut Vec<ModuleId>,
    ) -> Result<(), JsError>
    where
        F: FnMut(&mut Vm, ModuleId, &ModuleRequest) -> Result<ModuleId, JsError>,
    {
        let mut seen = BTreeSet::new();
        let mut frames = Vec::new();
        self.begin_module_link(root, &mut seen, &mut frames, newly)?;
        while let Some((id, next_request)) = frames.last_mut() {
            self.tick()?;
            let referrer = *id;
            let code = self.module_data(referrer)?.code.clone();
            let Some(request) = code.requests.get(*next_request) else {
                frames.pop();
                continue;
            };
            *next_request += 1;
            self.reserve_allocation(128)?;
            let dependency = resolver(self, referrer, request)?;
            self.module_data(dependency)?;
            self.module_data_mut(referrer)?
                .dependencies
                .push(dependency);
            self.begin_module_link(dependency, &mut seen, &mut frames, newly)?;
        }
        // Every environment and hoisted function exists before any alias resolution.
        for id in newly.iter().copied() {
            self.tick()?;
            self.ensure_allocation(512)?;
            let mut env = Environment::new(
                Some(self.realm.global_env),
                EnvKind::Module,
                Some(Value::Undefined),
            );
            env.module = Some(id.0);
            let env = self.heap.allocate_env(env);
            let body = self.module_data(id)?.code.body.clone();
            self.with_referrer(
                Some(crate::dynamic_import::SourceReferrer::Module(id)),
                |vm| vm.prepare(&body.scope, env),
            )?;
            self.module_data_mut(id)?.environment = Some(env);
            let ns = Value::Object(self.heap.allocate(Cell::Object(Object::new(
                ObjectKind::ModuleNamespace(ModuleNamespace {
                    module: id,
                    exports: BTreeMap::new(),
                }),
                None,
            ))));
            if let Some(tag) = self.well_known_symbols.get("toStringTag").cloned() {
                self.define_data_property_key(
                    ns.clone(),
                    crate::PropertyKey::Symbol(tag),
                    Value::from("Module"),
                    false,
                    false,
                    false,
                )?;
            }
            self.heap.object_mut(ns.as_object().unwrap())?.extensible = false;
            self.module_data_mut(id)?.namespace = Some(ns);
        }
        for id in newly.iter().copied() {
            let env = self.module_data(id)?.environment.unwrap();
            let code = self.module_data(id)?.code.clone();
            for import in &code.imports {
                self.tick()?;
                let target = self.dependency(id, &import.request)?;
                self.reserve_binding(&import.local)?;
                let resolution = if let Some(name) = &import.imported {
                    self.resolve_export(target, name)?
                } else {
                    Resolution::Resolved(ExportTarget::Namespace(target))
                };
                match resolution {
                    Resolution::Resolved(ExportTarget::Binding(target, name)) => {
                        self.reserve_allocation(64 + name.len() * 2)?;
                        self.heap
                            .environment_mut(env)
                            .aliases
                            .insert(import.local.clone(), (target, name));
                    }
                    Resolution::Resolved(ExportTarget::Namespace(module)) => {
                        let namespace = self.namespace_unchecked(module)?;
                        self.heap.environment_mut(env).bindings.insert(
                            import.local.clone(),
                            Binding {
                                value: namespace,
                                mutable: false,
                                initialized: true,
                                lexical: true,
                                strict_immutable: true,
                                deletable: false,
                            },
                        );
                    }
                    Resolution::Missing | Resolution::Ambiguous => {
                        return Err(JsError::new(
                            ErrorKind::SyntaxError,
                            "Missing or ambiguous imported export",
                        ));
                    }
                }
            }
        }
        for id in newly.iter().copied() {
            let code = self.module_data(id)?.code.clone();
            for export in &code.exports {
                if let Some(name) = export.exported()
                    && !matches!(self.resolve_export(id, name)?, Resolution::Resolved(_))
                {
                    return Err(JsError::new(
                        ErrorKind::SyntaxError,
                        "Missing or ambiguous re-export",
                    ));
                }
            }
            let mut exports = BTreeMap::new();
            for name in self.exported_names(id)? {
                if let Resolution::Resolved(target) = self.resolve_export(id, &name)? {
                    self.reserve_allocation(256 + name.len() * 2)?;
                    exports.insert(name, target);
                }
            }
            let namespace = self.namespace_unchecked(id)?.as_object().unwrap();
            if let ObjectKind::ModuleNamespace(data) = &mut self.heap.object_mut(namespace)?.kind {
                data.exports = exports;
            }
        }
        for id in newly.iter().copied() {
            self.module_data_mut(id)?.status = ModuleStatus::Linked;
        }
        Ok(())
    }
    fn begin_module_link(
        &mut self,
        id: ModuleId,
        seen: &mut BTreeSet<ModuleId>,
        frames: &mut Vec<(ModuleId, usize)>,
        newly: &mut Vec<ModuleId>,
    ) -> Result<(), JsError> {
        if seen.contains(&id) {
            return Ok(());
        }
        if seen.len() >= 1024 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Module graph size budget exhausted",
            ));
        }
        self.reserve_allocation(256)?;
        seen.insert(id);
        match self.module_data(id)?.status {
            ModuleStatus::Unlinked => {
                newly.push(id);
                self.module_data_mut(id)?.status = ModuleStatus::Linking;
                frames.push((id, 0));
            }
            ModuleStatus::Linking => return Err(JsError::type_error("Reentrant module linking")),
            _ => {}
        }
        Ok(())
    }
    fn namespace_unchecked(&self, id: ModuleId) -> Result<Value, JsError> {
        self.module_data(id)?.namespace.clone().ok_or_else(|| {
            JsError::new(
                ErrorKind::SyntaxError,
                "Module namespace requested before linking",
            )
        })
    }
    pub fn module_namespace(&mut self, id: ModuleId) -> Result<Value, JsError> {
        if matches!(
            self.module_data(id)?.status,
            ModuleStatus::Unlinked | ModuleStatus::Linking
        ) {
            return Err(JsError::type_error(
                "Module must be linked before its namespace is exposed",
            ));
        }
        self.namespace_unchecked(id)
    }
    fn exported_names(&mut self, root: ModuleId) -> Result<Vec<JsString>, JsError> {
        let mut pending = vec![root];
        let mut seen = BTreeSet::new();
        let mut names = BTreeSet::new();
        while let Some(id) = pending.pop() {
            self.tick()?;
            if !seen.insert(id) {
                continue;
            }
            let code = self.module_data(id)?.code.clone();
            for export in &code.exports {
                self.tick()?;
                if let Some(name) = export.exported() {
                    if id == root || name != &JsString::from("default") {
                        self.reserve_allocation(96 + name.len() * 2)?;
                        names.insert(name.clone());
                    }
                } else if let ExportEntry::Star { request } = export {
                    self.reserve_allocation(96)?;
                    pending.push(self.dependency(id, request)?);
                }
            }
        }
        Ok(names.into_iter().collect())
    }
    fn resolve_export(&mut self, root: ModuleId, name: &JsString) -> Result<Resolution, JsError> {
        let mut pending = vec![(root, name.clone())];
        let mut seen = BTreeSet::new();
        let mut found = None;
        while let Some((id, name)) = pending.pop() {
            self.tick()?;
            self.consume_work(name.len())?;
            self.reserve_allocation(96)?;
            if !seen.insert((id, name.clone())) {
                continue;
            }
            let data = self.module_data(id)?;
            let code = data.code.clone();
            let env = data
                .environment
                .ok_or_else(|| JsError::new(ErrorKind::SyntaxError, "Module has no environment"))?;
            let target = match code
                .export_indices
                .get(&name)
                .map(|index| &code.exports[*index])
            {
                Some(ExportEntry::Local { local, .. }) => {
                    if let Some(import) = code
                        .import_indices
                        .get(local)
                        .map(|index| &code.imports[*index])
                    {
                        let target = self.dependency(id, &import.request)?;
                        if let Some(imported) = &import.imported {
                            self.reserve_allocation(64)?;
                            pending.push((target, imported.clone()));
                            continue;
                        }
                        Some(ExportTarget::Namespace(target))
                    } else {
                        Some(ExportTarget::Binding(env, local.clone()))
                    }
                }
                Some(ExportEntry::Indirect {
                    request, imported, ..
                }) => {
                    self.reserve_allocation(64)?;
                    pending.push((self.dependency(id, request)?, imported.clone()));
                    continue;
                }
                Some(ExportEntry::Namespace { request, .. }) => {
                    Some(ExportTarget::Namespace(self.dependency(id, request)?))
                }
                Some(ExportEntry::Star { .. }) => unreachable!(),
                None => {
                    if name != JsString::from("default") {
                        for request in &code.star_requests {
                            self.reserve_allocation(64)?;
                            pending.push((self.dependency(id, request)?, name.clone()));
                        }
                    }
                    None
                }
            };
            if let Some(target) = target {
                if found.as_ref().is_some_and(|previous| previous != &target) {
                    return Ok(Resolution::Ambiguous);
                }
                found = Some(target);
            }
        }
        Ok(found.map_or(Resolution::Missing, Resolution::Resolved))
    }
    pub(crate) fn namespace_descriptor(
        &self,
        id: ObjectId,
        key: &JsString,
    ) -> Result<Option<Property>, JsError> {
        let ObjectKind::ModuleNamespace(ns) = &self.heap.object(id)?.kind else {
            return Ok(None);
        };
        let Some(target) = ns.exports.get(key) else {
            return Ok(None);
        };
        let value = match target {
            ExportTarget::Namespace(module) => self.namespace_unchecked(*module)?,
            ExportTarget::Binding(env, name) => {
                let binding = self
                    .heap
                    .environment(*env)
                    .bindings
                    .get(name)
                    .ok_or_else(|| {
                        JsError::new(ErrorKind::InternalError, "Missing namespace binding")
                    })?;
                if !binding.initialized {
                    return Err(JsError::new(
                        ErrorKind::ReferenceError,
                        "Module export is uninitialized",
                    ));
                }
                binding.value.clone()
            }
        };
        Ok(Some(Property {
            kind: PropertyKind::Data {
                value,
                writable: true,
            },
            enumerable: true,
            configurable: false,
        }))
    }
    pub(crate) fn define_namespace_property(
        &mut self,
        id: ObjectId,
        key: &JsString,
        property: &Property,
    ) -> Result<(), JsError> {
        let Some(current) = self.namespace_descriptor(id, key)? else {
            return Err(JsError::type_error("Module namespace is not extensible"));
        };
        let (
            PropertyKind::Data { value: current, .. },
            PropertyKind::Data {
                value,
                writable: true,
            },
        ) = (&current.kind, &property.kind)
        else {
            return Err(JsError::type_error(
                "Cannot redefine module namespace export",
            ));
        };
        if let (Value::String(a), Value::String(b)) = (current, value) {
            self.consume_work(a.len().min(b.len()))?;
        }
        let same = match (current, value) {
            (Value::Number(a), Value::Number(b)) => {
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
            }
            _ => current == value,
        };
        if property.configurable || !property.enumerable || !same {
            return Err(JsError::type_error(
                "Cannot redefine module namespace export",
            ));
        }
        Ok(())
    }
}

fn compile_module(source: &str, compilation_bytes: usize) -> Result<Rc<ModuleCode>, JsError> {
    let parsed = parser::modules::parse(source, compilation_bytes)?;
    let body = bytecode::compile_module_body(&parsed.body)?;
    let mut locals = BTreeSet::new();
    locals.extend(body.scope.vars.iter().cloned());
    locals.extend(body.scope.lexical.iter().map(|(name, _)| name.clone()));
    for import in &parsed.imports {
        if matches!(import.local.to_string_lossy().as_str(), "await")
            || bytecode::invalid_strict_binding(&import.local)
            || !locals.insert(import.local.clone())
        {
            return Err(JsError::new(
                ErrorKind::SyntaxError,
                "Invalid or conflicting module import binding",
            ));
        }
    }
    for export in &parsed.exports {
        if let ExportEntry::Local { local, .. } = export
            && !locals.contains(local)
        {
            return Err(JsError::new(
                ErrorKind::SyntaxError,
                "Export refers to an undeclared local binding",
            ));
        }
    }

    let requests = parsed.requests;
    let import_indices = parsed
        .imports
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.local.clone(), index))
        .collect();
    let export_indices = parsed
        .exports
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.exported().map(|name| (name.clone(), index)))
        .collect();
    let star_requests = parsed
        .exports
        .iter()
        .filter_map(|entry| {
            if let ExportEntry::Star { request } = entry {
                Some(*request)
            } else {
                None
            }
        })
        .collect();
    Ok(Rc::new(ModuleCode {
        body,
        requests,
        imports: parsed.imports,
        exports: parsed.exports,
        import_indices,
        export_indices,
        star_requests,
    }))
}
