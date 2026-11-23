//! Lexical private names and non-property storage for class elements.
use crate::{
    ErrorKind, JsError, JsString, Value, Vm,
    heap::{EnvId, EnvKind, Environment},
    vm::Reference,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct PrivateId(u64);

#[derive(Clone)]
pub(crate) enum PrivateElement {
    Field(Value),
    Method(Value),
    Accessor {
        get: Option<Value>,
        set: Option<Value>,
    },
}

impl PrivateElement {
    pub(crate) const ESTIMATED_BYTES: usize = 192;

    pub(crate) fn visit(&self, mut visit: impl FnMut(&Value)) {
        match self {
            Self::Field(value) | Self::Method(value) => visit(value),
            Self::Accessor { get, set } => get.iter().chain(set).for_each(visit),
        }
    }
}

pub(crate) fn name_bytes(name: &JsString) -> usize {
    128 + name.len() * 2
}

impl Vm {
    /// Configures the private-element host hook for an exotic receiver.
    /// The host calls this before exposing the receiver to JavaScript.
    pub fn forbid_private_elements(&mut self, receiver: &Value) -> Result<(), JsError> {
        let receiver = receiver
            .as_object()
            .ok_or_else(|| JsError::type_error("Private element host hook requires an object"))?;
        self.heap.object_mut(receiver)?.private_elements_forbidden = true;
        Ok(())
    }

    pub(crate) fn private_environment(
        &mut self,
        parent: EnvId,
        names: &[JsString],
    ) -> Result<EnvId, JsError> {
        if names.is_empty() {
            return Ok(parent);
        }
        self.ensure_allocation(256)?;
        let env = self
            .heap
            .allocate_env(Environment::new(Some(parent), EnvKind::Block, None));
        for name in names {
            self.consume_work(1 + name.len())?;
            let id = PrivateId(self.next_private_id);
            self.next_private_id = self.next_private_id.checked_add(1).ok_or_else(|| {
                JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Private name identity budget exhausted",
                )
            })?;
            self.account_value(Value::String(name.clone()))?;
            self.reserve_allocation(name_bytes(name))?;
            self.heap
                .environment_mut(env)
                .private_names
                .insert(name.clone(), id);
        }
        Ok(env)
    }

    pub(crate) fn private_eval_names(
        &mut self,
        mut env: EnvId,
    ) -> Result<BTreeSet<JsString>, JsError> {
        let mut names = BTreeSet::new();
        loop {
            self.tick()?;
            let scope = self.heap.environment(env);
            let work = scope
                .private_names
                .keys()
                .map(|name| 1 + name.len())
                .sum::<usize>();
            let parent = scope.parent;
            let count = scope.private_names.len();
            self.consume_work(work)?;
            self.reserve_allocation(count * 128)?;
            names.extend(self.heap.environment(env).private_names.keys().cloned());
            let Some(outer) = parent else {
                return Ok(names);
            };
            env = outer;
        }
    }

    pub(crate) fn resolve_private(
        &mut self,
        mut env: EnvId,
        name: &JsString,
    ) -> Result<PrivateId, JsError> {
        self.consume_work(name.len())?;
        loop {
            self.tick()?;
            let scope = self.heap.environment(env);
            if let Some(id) = scope.private_names.get(name) {
                return Ok(*id);
            }
            env = scope.parent.ok_or_else(|| {
                JsError::new(ErrorKind::InternalError, "Missing compiled private name")
            })?;
        }
    }

    pub(crate) fn private_reference(
        &mut self,
        env: EnvId,
        receiver: Value,
        name: &JsString,
    ) -> Result<Reference, JsError> {
        Ok(Reference::Private(
            receiver,
            self.resolve_private(env, name)?,
        ))
    }

    pub(crate) fn private_in(
        &mut self,
        env: EnvId,
        object: Value,
        name: &JsString,
    ) -> Result<bool, JsError> {
        let object = object.as_object().ok_or_else(|| {
            JsError::type_error("Right-hand side of private in must be an object")
        })?;
        let id = self.resolve_private(env, name)?;
        Ok(self.heap.object(object)?.private_elements.contains_key(&id))
    }

    pub(crate) fn private_add(
        &mut self,
        receiver: &Value,
        id: PrivateId,
        element: PrivateElement,
    ) -> Result<(), JsError> {
        let receiver = receiver.as_object().ok_or_else(missing_brand)?;
        let object = self.heap.object(receiver)?;
        if object.private_elements_forbidden {
            return Err(JsError::type_error(
                "Receiver does not accept private elements",
            ));
        }
        if object.private_elements.contains_key(&id) {
            return Err(JsError::type_error("Private element already initialized"));
        }
        if let PrivateElement::Field(value) = &element {
            self.account_value(value.clone())?;
        }
        self.reserve_allocation(PrivateElement::ESTIMATED_BYTES)?;
        self.heap
            .object_mut(receiver)?
            .private_elements
            .insert(id, element);
        Ok(())
    }

    pub(crate) fn private_get(&mut self, receiver: Value, id: PrivateId) -> Result<Value, JsError> {
        let receiver_id = receiver.as_object().ok_or_else(missing_brand)?;
        let element = self
            .heap
            .object(receiver_id)?
            .private_elements
            .get(&id)
            .ok_or_else(missing_brand)?;
        match element {
            PrivateElement::Field(value) | PrivateElement::Method(value) => Ok(value.clone()),
            PrivateElement::Accessor { get: Some(get), .. } => {
                self.invoke(get.clone(), receiver, &[])
            }
            PrivateElement::Accessor { .. } => {
                Err(JsError::type_error("Private accessor has no getter"))
            }
        }
    }

    pub(crate) fn private_set(
        &mut self,
        receiver: Value,
        id: PrivateId,
        value: Value,
    ) -> Result<(), JsError> {
        let receiver_id = receiver.as_object().ok_or_else(missing_brand)?;
        let element = self
            .heap
            .object(receiver_id)?
            .private_elements
            .get(&id)
            .ok_or_else(missing_brand)?;
        match element {
            PrivateElement::Field(_) => {
                let value = self.account_value(value)?;
                self.heap
                    .object_mut(receiver_id)?
                    .private_elements
                    .insert(id, PrivateElement::Field(value));
                Ok(())
            }
            PrivateElement::Accessor { set: Some(set), .. } => {
                self.invoke(set.clone(), receiver, &[value])?;
                Ok(())
            }
            PrivateElement::Method(_) => {
                Err(JsError::type_error("Cannot assign to a private method"))
            }
            PrivateElement::Accessor { .. } => {
                Err(JsError::type_error("Private accessor has no setter"))
            }
        }
    }
}

fn missing_brand() -> JsError {
    JsError::type_error("Receiver does not have the private element")
}
