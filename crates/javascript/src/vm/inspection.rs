//! Bounded, inert previews for a developer inspecting the result of a script.
//! Read Rust-owned descriptors directly so inspection cannot run page getters.
use super::{
    Binding, Chunk, Completion, EnvId, EnvKind, Environment, JsError, ObjectKind, PropertyKind,
    Value, Vm,
};
use crate::JsString;

fn string(value: &JsString, limit: usize) -> String {
    let units = value.as_utf16();
    let mut text = String::from_utf16_lossy(&units[..units.len().min(limit)]);
    if units.len() > limit {
        text.push('…');
    }
    text
}

impl Vm {
    /// Enables bounded snapshots of currently entered interpreted function calls.
    pub fn enable_inspection(&mut self) {
        self.inspection_enabled = true;
    }

    pub fn inspect_call_stack(&self) -> Option<std::rc::Rc<Vec<String>>> {
        if !self.inspection_enabled || self.active_calls.is_empty() {
            return None;
        }
        Some(std::rc::Rc::new(self.stack_names().collect()))
    }

    fn stack_names(&self) -> impl Iterator<Item = String> + '_ {
        self.active_calls.iter()
            .rev()
            .take(10)
            .map(|name| {
                name.as_ref()
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| "(anonymous)".to_owned(), |s| string(s, 80))
            })
    }

    pub(crate) fn allocate_error(&mut self, kind: crate::ErrorKind, prototype: crate::ObjectId) -> Result<Value, JsError> {
        let mut trace = kind.name().to_owned();
        for name in self.stack_names() {
            trace.push_str("\n    at ");
            trace.push_str(&name);
        }
        let trace = self.account_value(Value::from(trace))?;
        self.allocate_constructed(ObjectKind::Error(kind, trace.as_string().unwrap().clone()), prototype)
    }

    /// Evaluate a developer command with a read-only, command-local DOM selection.
    pub fn eval_with_inspected_node(
        &mut self,
        source: &str,
        node: Value,
    ) -> Result<Value, JsError> {
        self.eval_with_referrer(source, None, Some(node))
    }

    pub(super) fn declaration_environment(&self, env: EnvId) -> EnvId {
        let environment = self.heap.environment(env);
        if environment.kind == EnvKind::Inspector {
            environment
                .parent
                .expect("Inspector environment has a page realm")
        } else {
            env
        }
    }

    pub(super) fn run_inspected_command(
        &mut self,
        chunk: &Chunk,
        node: Value,
    ) -> Result<Completion, JsError> {
        let key = JsString::from("$0");
        let global = self.realm.global_env;
        if chunk.scope.lexical.iter().any(|(name, _)| name == &key)
            || chunk.scope.vars.contains(&key)
        {
            return self.run_chunk(chunk, global);
        }
        self.reserve_binding(&key)?;
        let mut scope = Environment::new(Some(global), EnvKind::Inspector, None);
        scope.bindings.insert(
            key.clone(),
            Binding {
                value: node,
                mutable: false,
                initialized: true,
                lexical: true,
                strict_immutable: true,
                deletable: false,
            },
        );
        let env = self.heap.allocate_env(scope);
        let result = self.run_chunk(chunk, env);
        // Escaping callbacks retain their own locals, but not the temporary selection.
        self.heap.environment_mut(env).bindings.remove(&key);
        result
    }

    /// Bounded, inert description of a rejected object. Inspect only selected
    /// own data descriptors; never run getters, proxy traps or coercions.
    pub fn inspect_rejection_reason(&self, value: &Value) -> Option<String> {
        let Value::Object(id) = value else {
            return None;
        };
        let object = self.heap.object(*id).ok()?;
        let kind = match &object.kind {
            ObjectKind::Error(kind, _) => Some(kind.name()),
            _ => None,
        };
        let mut fields = Vec::new();
        for name in ["name", "message", "status", "statusText", "readyState"] {
            let Some(prop) = object.properties.get(&JsString::from(name)) else {
                continue;
            };
            let PropertyKind::Data { value, .. } = &prop.kind else {
                continue;
            };
            let preview = match value {
                Value::String(text) => format!("{:?}", string(text, 160)),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".into(),
                _ => continue,
            };
            fields.push(format!("{name}: {preview}"));
        }
        if fields.is_empty() {
            return kind.map(str::to_owned);
        }
        Some(format!(
            "{} {{ {} }}",
            kind.unwrap_or("object"),
            fields.join(", ")
        ))
    }

    pub fn inspect_value(&self, value: &Value) -> String {
        self.preview_value(value, true)
    }

    fn preview_value(&self, value: &Value, expand: bool) -> String {
        match value {
            Value::String(text) => format!("{:?}", string(text, if expand { 256 } else { 48 })),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::BigInt(n) => format!("{n:?}"),
            Value::Symbol(id) => format!(
                "Symbol({})",
                id.description().map(|s| string(s, 48)).unwrap_or_default()
            ),
            Value::Object(id) => {
                let Ok(object) = self.heap.object(*id) else {
                    return "[unavailable]".into();
                };
                let name = match &object.kind {
                    ObjectKind::Array { length, .. } => format!("Array({length})"),
                    ObjectKind::Function { .. }
                    | ObjectKind::Native { .. }
                    | ObjectKind::Bound { .. } => "Function".into(),
                    ObjectKind::Promise(_) => "Promise".into(),
                    ObjectKind::RegExp(..) => "RegExp".into(),
                    ObjectKind::Date(_) => "Date".into(),
                    ObjectKind::Error(kind, _) => kind.name().into(),
                    ObjectKind::ArrayBuffer(_) => "ArrayBuffer".into(),
                    ObjectKind::TypedArray(_) => "TypedArray".into(),
                    _ => "Object".into(),
                };
                if !expand {
                    return name;
                }
                let mut fields = Vec::new();
                // Cap the work even if the object starts with many non-enumerable keys.
                for (key, prop) in object.properties.iter().take(32) {
                    if !prop.enumerable {
                        continue;
                    }
                    if fields.len() == 6 {
                        fields.push("…".into());
                        break;
                    }
                    let val = match &prop.kind {
                        PropertyKind::Data { value, .. } => self.preview_value(value, false),
                        PropertyKind::Accessor { .. } => "[accessor]".into(),
                    };
                    fields.push(format!("{}: {val}", string(key, 36)));
                }
                if fields.len() <= 6 && object.properties.len() > 32 {
                    fields.push("…".into());
                }
                if fields.is_empty() {
                    name
                } else {
                    format!("{name} {{ {} }}", fields.join(", "))
                }
            }
        }
    }
}
