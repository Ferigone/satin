use super::{element::Element, typed_array::LITTLE_ENDIAN};
use crate::{
    JsError, JsString, Value, Vm,
    bytecode::{ForCode, Op},
    heap::{EnvId, EnvKind, ObjectKind},
};

impl Vm {
    /// Resolve only bindings whose lookup cannot call author code. Proxies,
    /// with environments, module imports and lazy arguments use the interpreter.
    fn fill_binding(&self, mut scope: EnvId, name: &JsString) -> Option<EnvId> {
        loop {
            let env = self.heap.environment(scope);
            if !matches!(env.kind, EnvKind::Function | EnvKind::Block)
                || env.aliases.contains_key(name)
                || env.has_lazy_arguments(name)
            {
                return None;
            }
            if let Some(binding) = env.bindings.get(name) {
                return binding.initialized.then_some(scope);
            }
            scope = env.parent?;
        }
    }

    /// After the ordinary initializer, recognize a decrementing loop that
    /// assigns one numeric constant to a contiguous range. A real writable,
    /// unshared typed view has no per-index property hooks. Each committed
    /// block runs in descending index order and pays native buffer work.
    pub(crate) fn try_counted_fill(
        &mut self,
        code: &ForCode,
        env: EnvId,
    ) -> Result<Option<Value>, JsError> {
        if !code.lexical.is_empty() || code.update.is_some() {
            return Ok(None);
        }
        let Some(test) = &code.test else {
            return Ok(None);
        };
        let [
            Op::Name(index),
            Op::Update {
                increment: false,
                prefix: true,
            },
            Op::BinaryNumber(">=", bound),
        ] = test.ops.as_slice()
        else {
            return Ok(None);
        };
        let [
            Op::LoadName(array),
            Op::LoadName(subscript),
            Op::Property,
            Op::Push(Value::Number(value)),
            Op::Set,
            Op::StatementValue,
        ] = code.body.ops.as_slice()
        else {
            return Ok(None);
        };
        if *bound != 0.0 || subscript != index {
            return Ok(None);
        }
        let (Some(counter), Some(target)) =
            (self.fill_binding(env, index), self.fill_binding(env, array))
        else {
            return Ok(None);
        };
        let binding = &self.heap.environment(counter).bindings[index];
        let Value::Number(start) = binding.value else {
            return Ok(None);
        };
        if !binding.mutable || !start.is_finite() || start < 0.0 || start.fract() != 0.0 {
            return Ok(None);
        }
        let Value::Object(id) = self.heap.environment(target).bindings[array].value else {
            return Ok(None);
        };
        let ObjectKind::TypedArray(view) = self.heap.object(id)?.kind else {
            return Ok(None);
        };
        if matches!(view.element, Element::BigInt64 | Element::BigUint64)
            || !matches!(
                self.heap.object(view.view.buffer)?.kind,
                ObjectKind::ArrayBuffer(_)
            )
            || self.typed_buffer_immutable(id)?
        {
            return Ok(None);
        }
        let width = view.element.width();
        let Some(range) = self.view_range(view.view, width)? else {
            return Ok(None);
        };
        if start > (range.len() / width) as f64 {
            return Ok(None);
        }
        let number = Value::Number(*value);
        let bits = view.element.coerce_bits(self, &number)?;
        let pattern = view.element.store_bits(bits, LITTLE_ENDIAN);
        let all_one_byte = pattern[..width].iter().all(|v| *v == pattern[0]);
        self.tick()?;
        let mut end = start as usize;
        while end > 0 {
            let begin = end.saturating_sub(4096 / width);
            self.heap
                .environment_mut(counter)
                .bindings
                .get_mut(index)
                .unwrap()
                .value = Value::from(end - 1);
            self.binary_work((end - begin) * width)?;
            let storage = self
                .buffer_data_mut(view.view.buffer)?
                .bytes
                .as_mut()
                .unwrap();
            let block = &mut storage[range.start + begin * width..range.start + end * width];
            if all_one_byte {
                block.fill(pattern[0]);
            } else {
                for element in block.chunks_exact_mut(width) {
                    element.copy_from_slice(&pattern[..width]);
                }
            }
            end = begin;
        }
        self.heap
            .environment_mut(counter)
            .bindings
            .get_mut(index)
            .unwrap()
            .value = Value::Number(-1.0);
        Ok(Some(if start == 0.0 {
            Value::Undefined
        } else {
            number
        }))
    }
}
