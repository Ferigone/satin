use crate::{JsString, ObjectId, Value};

const ROWS: usize = 128;
const KEYS_PER_ROW: usize = 2;

/// Identity-only memoization of plain own data reads. Neither the receiver
/// nor the value is a GC root; clear before tracing and on any object mutation.
pub(super) struct PropertyCache {
    rows: [[Option<Entry>; KEYS_PER_ROW]; ROWS],
}

struct Entry {
    receiver: ObjectId,
    key: JsString,
    value: Value,
}

impl PropertyCache {
    pub(super) fn new() -> Self {
        Self { rows: std::array::from_fn(|_| Default::default()) }
    }

    fn row(receiver: ObjectId) -> usize {
        receiver.index as usize % ROWS
    }

    fn lane(key: &JsString) -> usize {
        let address = key.storage_id();
        ((address >> 4) ^ (address >> 7)) % KEYS_PER_ROW
    }

    pub(super) fn get(&self, receiver: ObjectId, key: &JsString) -> Option<Value> {
        let entry = self.rows[Self::row(receiver)][Self::lane(key)].as_ref()?;
        (entry.receiver == receiver && entry.key.storage_id() == key.storage_id())
            .then(|| entry.value.clone())
    }

    pub(super) fn insert(&mut self, receiver: ObjectId, key: &JsString, value: &Value) {
        self.rows[Self::row(receiver)][Self::lane(key)] = Some(Entry {
            receiver,
            // Keep the key identity alive so its address cannot be reused.
            key: key.clone(),
            value: value.clone(),
        });
    }

    pub(super) fn invalidate(&mut self, receiver: ObjectId) {
        for entry in &mut self.rows[Self::row(receiver)] {
            if entry.as_ref().is_some_and(|entry| entry.receiver == receiver) {
                *entry = None;
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.rows.iter_mut().flatten().for_each(|entry| *entry = None);
    }
}
