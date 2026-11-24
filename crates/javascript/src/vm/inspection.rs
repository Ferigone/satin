//! Bounded, inert previews for a developer inspecting the result of a script.
//! Read Rust-owned descriptors directly so inspection cannot run page getters.
use super::{ObjectKind, PropertyKind, Value, Vm};
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
    /// Standalone VMs that do not install inspection skip all tracking.
    pub fn enable_inspection(&mut self) {
        self.inspected_calls.get_or_insert_with(Vec::new);
    }

    pub fn inspect_call_stack(&self) -> Option<std::rc::Rc<Vec<String>>> {
        let calls = self.inspected_calls.as_ref()?;
        if calls.is_empty() {
            return None;
        }
        let names = calls
            .iter()
            .rev()
            .take(10)
            .map(|name| {
                name.as_ref()
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| "(anonymous)".to_owned(), |s| string(s, 80))
            })
            .collect();
        Some(std::rc::Rc::new(names))
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
                    ObjectKind::RegExp(_) => "RegExp".into(),
                    ObjectKind::Date(_) => "Date".into(),
                    ObjectKind::Error(kind) => kind.name().into(),
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
