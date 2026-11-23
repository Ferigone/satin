use crate::{JsError, JsString, ObjectId, PropertyKey, Value, Vm};
use std::{
    collections::{BTreeSet, VecDeque},
    mem::size_of,
};

const SEEN_KEY_BYTES: usize = 96;

pub(crate) struct ForInKeys {
    current: Option<ObjectId>,
    pending: Option<VecDeque<JsString>>,
    seen: BTreeSet<JsString>,
    depth: usize,
}

impl ForInKeys {
    pub(crate) fn new(vm: &mut Vm, source: Value) -> Result<Self, JsError> {
        let current = if source.is_nullish() {
            None
        } else {
            vm.coerce_object(source)?.as_object()
        };
        Ok(Self {
            current,
            pending: None,
            seen: BTreeSet::new(),
            depth: 0,
        })
    }

    pub(crate) fn next(&mut self, vm: &mut Vm) -> Result<Option<JsString>, JsError> {
        while let Some(id) = self.current {
            if self.depth == 1024 {
                return Ok(None);
            }
            // Snapshot each object's own names once, then inspect their
            // current descriptors only as they are visited.
            if self.pending.is_none() {
                vm.tick()?;
                self.pending = Some(vm.own_keys(Value::Object(id), false)?.into());
            }
            while let Some(key) = self.pending.as_mut().unwrap().pop_front() {
                vm.tick()?;
                if !self.seen.contains(&key)
                    && let Some(enumerable) =
                        vm.own_enumerability(id, &PropertyKey::String(key.clone()))?
                {
                    // Deleted properties do not shadow an inherited name.
                    // Existing, non-enumerable properties still do.
                    vm.reserve_allocation(SEEN_KEY_BYTES)?;
                    self.seen.insert(key.clone());
                    if enumerable {
                        return Ok(Some(key));
                    }
                }
            }
            self.current = vm.heap.object(id)?.prototype;
            self.pending = None;
            self.depth += 1;
        }
        Ok(None)
    }

    pub(crate) fn visit(&self, visit: &mut impl FnMut(&Value)) {
        if let Some(id) = self.current {
            visit(&Value::Object(id));
        }
        for key in self.pending.iter().flatten().chain(&self.seen) {
            visit(&Value::String(key.clone()));
        }
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        self.pending.as_ref().map_or(0, VecDeque::capacity) * size_of::<JsString>()
            + self.seen.len() * SEEN_KEY_BYTES
    }
}
