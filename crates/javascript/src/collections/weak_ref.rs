//! Weak targets are compared against the GC's completed mark set. Only the
//! registry callback and held values are strong tracing edges.
use super::{arg, tag};
use crate::{
    ErrorKind, JsError, JsString, ObjectId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::{ObjectKind, TraceEdges, TraceTarget},
};

struct Registration {
    target: Option<Value>,
    holding: Value,
    token: Option<Value>,
}

pub(crate) struct Registry {
    callback: Value,
    cells: Vec<Registration>,
    pending: Vec<Registration>,
}

impl Registry {
    pub(crate) fn trace(&self, edges: &mut TraceEdges) {
        edges.value(&self.callback);
        for cell in self.cells.iter().chain(&self.pending) {
            edges.value(&cell.holding);
        }
    }

    pub(crate) fn strings(&self, strings: &mut Vec<JsString>) {
        strings.extend(
            self.cells
                .iter()
                .chain(&self.pending)
                .filter_map(|c| c.holding.as_string().cloned()),
        );
    }

    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + (self.cells.capacity() + self.pending.capacity())
                * std::mem::size_of::<Registration>()
    }

    pub(crate) fn sweep(&mut self, marked: impl Fn(TraceTarget) -> bool) -> bool {
        for cell in self.cells.iter_mut().chain(&mut self.pending) {
            if cell
                .token
                .as_ref()
                .and_then(TraceTarget::of_value)
                .is_some_and(|t| !marked(t))
            {
                cell.token = None;
            }
        }
        let mut index = self.cells.len();
        while index > 0 {
            index -= 1;
            let target = self.cells[index]
                .target
                .as_ref()
                .and_then(TraceTarget::of_value)
                .unwrap();
            if !marked(target) {
                let mut record = self.cells.swap_remove(index);
                record.target = None;
                self.pending.push(record);
            }
        }
        !self.pending.is_empty()
    }
}

fn weak_value(value: Value) -> Result<Value, JsError> {
    if TraceTarget::of_value(&value).is_none() {
        return Err(JsError::type_error(
            "Expected an object or an unregistered symbol",
        ));
    }
    Ok(value)
}

fn registry_id(vm: &Vm, value: Value) -> Result<ObjectId, JsError> {
    if let Some(id) = value.as_object()
        && matches!(
            vm.heap.object(id)?.kind,
            ObjectKind::FinalizationRegistry(_)
        )
    {
        return Ok(id);
    }
    Err(JsError::type_error(
        "Incompatible FinalizationRegistry receiver",
    ))
}

impl Vm {
    fn registry(&mut self, id: ObjectId) -> &mut Registry {
        let ObjectKind::FinalizationRegistry(registry) =
            &mut self.heap.object_mut(id).expect("checked registry").kind
        else {
            unreachable!()
        };
        registry
    }

    pub(crate) fn cleanup_registry(&mut self, id: ObjectId) -> Result<(), JsError> {
        // A callback may unregister other cells before the next cleanup.
        while let Some(record) = self.registry(id).pending.pop() {
            self.tick()?;
            let callback = self.registry(id).callback.clone();
            self.invoke(callback, Value::Undefined, &[record.holding])?;
        }
        Ok(())
    }
}

pub(super) fn install(vm: &mut Vm) {
    for name in ["WeakRef", "FinalizationRegistry"] {
        let prototype = vm.new_object();
        let intrinsic = prototype.as_object().unwrap();
        vm.root(prototype.clone());
        tag(vm, prototype.clone(), name);
        let ctor = constructor(vm, name, 1, prototype.clone(), move |_, _, _| {
            Err(JsError::type_error(format!("{name} requires new")))
        });
        replace_construct(vm, ctor, move |vm, _, args, new_target| {
            let argument = arg(args, 0);
            let kind = if name == "WeakRef" {
                ObjectKind::WeakRef(Some(weak_value(argument)?))
            } else {
                if !vm.is_callable(&argument) {
                    return Err(JsError::type_error("Cleanup callback is not callable"));
                }
                ObjectKind::FinalizationRegistry(Box::new(Registry {
                    callback: argument,
                    cells: Vec::new(),
                    pending: Vec::new(),
                }))
            };
            let proto = vm.prototype_from_constructor(new_target, intrinsic)?;
            vm.allocate_constructed(kind, proto)
        });
        if name == "WeakRef" {
            method(vm, prototype, "deref", 0, |vm, this, _| {
                if let Some(id) = this.as_object()
                    && let ObjectKind::WeakRef(target) = &vm.heap.object(id)?.kind
                {
                    // The embedding collects only between tasks, so dereferencing
                    // also keeps this value alive through the current job.
                    return Ok(target.clone().unwrap_or_default());
                }
                Err(JsError::type_error("Incompatible WeakRef receiver"))
            });
            continue;
        }
        method(vm, prototype.clone(), "register", 2, |vm, this, args| {
            let id = registry_id(vm, this)?;
            let target = weak_value(arg(args, 0))?;
            let holding = arg(args, 1);
            if holding == target {
                return Err(JsError::type_error("Target cannot be its own holding"));
            }
            let token = arg(args, 2);
            let token = if token.is_undefined() {
                None
            } else {
                Some(weak_value(token)?)
            };
            let registry = vm.registry(id);
            if registry.cells.len() + registry.pending.len() >= vm.limits.max_array_length as usize
            {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Finalization registry entry limit",
                ));
            }
            vm.reserve_allocation(std::mem::size_of::<Registration>() * 3)?;
            let holding = vm.account_value(holding)?;
            vm.registry(id).cells.push(Registration {
                target: Some(target),
                holding,
                token,
            });
            Ok(Value::Undefined)
        });
        method(vm, prototype, "unregister", 1, |vm, this, args| {
            let id = registry_id(vm, this)?;
            let token = weak_value(arg(args, 0))?;
            let registry = vm.registry(id);
            let count = registry.cells.len() + registry.pending.len();
            vm.consume_work(count)?;
            let registry = vm.registry(id);
            registry
                .cells
                .retain(|cell| cell.token.as_ref() != Some(&token));
            registry
                .pending
                .retain(|cell| cell.token.as_ref() != Some(&token));
            Ok((registry.cells.len() + registry.pending.len() != count).into())
        });
    }
}
