//! Serialize session-history state without retaining objects from a realm.
//! Spare array indices, special numbers, repeated references, and cycles are
//! preserved. Frames visit properties in the same depth-first order as the
//! structured serialization algorithm.
use rust_browser_javascript::{JsError, JsString, ObjectId, PropertyKey, Value, Vm};
use std::collections::HashMap;

const BYTE_LIMIT: usize = 512 * 1024;
const NODE_LIMIT: usize = 4096;

enum Atom {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    BigInt(rust_browser_javascript::JsBigInt),
    String(JsString),
    Reference(usize),
}

struct Node {
    length: Option<u32>,
    entries: Vec<(JsString, Atom)>,
}

pub(crate) struct StateData {
    root: Atom,
    nodes: Vec<Node>,
    bytes: usize,
}

pub(crate) enum SerializeError {
    Uncloneable,
    Limit,
    JavaScript(JsError),
}

impl From<JsError> for SerializeError {
    fn from(error: JsError) -> Self {
        Self::JavaScript(error)
    }
}

struct Frame {
    source: Value,
    node: usize,
    keys: std::vec::IntoIter<JsString>,
}

struct Serializer<F> {
    permit: F,
    nodes: Vec<Node>,
    seen: HashMap<ObjectId, usize>,
    bytes: usize,
}

impl<F: Fn(ObjectId) -> bool> Serializer<F> {
    fn charge(&mut self, vm: &mut Vm, bytes: usize) -> Result<(), SerializeError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > BYTE_LIMIT {
            return Err(SerializeError::Limit);
        }
        vm.charge_work(bytes)?;
        Ok(())
    }

    fn atom(&mut self, vm: &mut Vm, value: Value) -> Result<(Atom, Option<Frame>), SerializeError> {
        let atom = match value {
            Value::Undefined => Atom::Undefined,
            Value::Null => Atom::Null,
            Value::Bool(value) => Atom::Bool(value),
            Value::Number(value) => Atom::Number(value),
            Value::BigInt(value) => {
                self.charge(vm, value.bit_length().div_ceil(8) as usize)?;
                Atom::BigInt(value)
            }
            Value::String(value) => {
                self.charge(vm, value.len() * 2)?;
                Atom::String(value)
            }
            Value::Symbol(_) => return Err(SerializeError::Uncloneable),
            Value::Object(id) => {
                if let Some(&node) = self.seen.get(&id) {
                    return Ok((Atom::Reference(node), None));
                }
                if !vm.supports_ordinary_clone(&value) || !(self.permit)(id) {
                    return Err(SerializeError::Uncloneable);
                }
                if self.nodes.len() == NODE_LIMIT {
                    return Err(SerializeError::Limit);
                }
                let length = if vm.is_array(&value) {
                    Some(
                        vm.get_property(value.clone(), "length")?
                            .as_number()
                            .unwrap() as u32,
                    )
                } else {
                    None
                };
                let keys = vm.enumerable_own_keys(value.clone())?;
                self.charge(vm, 96 + keys.len().saturating_mul(48))?;
                let node = self.nodes.len();
                self.nodes.push(Node {
                    length,
                    entries: Vec::with_capacity(keys.len()),
                });
                self.seen.insert(id, node);
                return Ok((
                    Atom::Reference(node),
                    Some(Frame {
                        source: value,
                        node,
                        keys: keys.into_iter(),
                    }),
                ));
            }
        };
        Ok((atom, None))
    }
}

impl StateData {
    pub(crate) fn capture(
        vm: &mut Vm,
        value: Value,
        is_enumerable: &Value,
        permit: impl Fn(ObjectId) -> bool,
    ) -> Result<Self, SerializeError> {
        let mut serializer = Serializer {
            permit,
            nodes: vec![],
            seen: HashMap::new(),
            bytes: 32,
        };
        let (root, first) = serializer.atom(vm, value)?;
        let mut stack = first.into_iter().collect::<Vec<_>>();
        while let Some(frame) = stack.last_mut() {
            let Some(key) = frame.keys.next() else {
                stack.pop();
                continue;
            };
            let key_value = Value::String(key.clone());
            // Reading a prior property can invoke a getter that deletes or
            // hides this key. It must never fall through to the prototype.
            if !vm
                .call(is_enumerable.clone(), frame.source.clone(), &[key_value])?
                .is_truthy()
            {
                continue;
            }
            serializer.charge(vm, key.len() * 2)?;
            let value =
                vm.get_property_key(frame.source.clone(), PropertyKey::String(key.clone()))?;
            let (value, next) = serializer.atom(vm, value)?;
            serializer.nodes[frame.node].entries.push((key, value));
            stack.extend(next);
        }
        Ok(Self {
            root,
            nodes: serializer.nodes,
            bytes: serializer.bytes,
        })
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn restore(&self, vm: &mut Vm) -> Result<Value, JsError> {
        vm.charge_work(self.bytes)?;
        let mut objects = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let object = if let Some(length) = node.length {
                let array = vm.new_array(&[]);
                vm.set_property(array.clone(), "length", length.into())?;
                array
            } else {
                vm.new_object()
            };
            objects.push(object);
        }
        for (object, node) in objects.iter().zip(&self.nodes) {
            for (key, atom) in &node.entries {
                vm.define_data_property_key(
                    object.clone(),
                    PropertyKey::String(key.clone()),
                    atom.restore(&objects),
                    true,
                    true,
                    true,
                )?;
            }
        }
        Ok(self.root.restore(&objects))
    }
}

impl Atom {
    fn restore(&self, objects: &[Value]) -> Value {
        match self {
            Self::Undefined => Value::Undefined,
            Self::Null => Value::Null,
            Self::Bool(value) => (*value).into(),
            Self::Number(value) => (*value).into(),
            Self::BigInt(value) => Value::BigInt(value.clone()),
            Self::String(value) => Value::String(value.clone()),
            Self::Reference(node) => objects[*node].clone(),
        }
    }
}
