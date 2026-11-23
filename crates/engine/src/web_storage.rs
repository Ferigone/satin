//! Origin-scoped storage shared by the realms of a profile and a tab.
use ring::digest::{SHA256, digest};
use rust_browser_javascript::JsString;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::{Rc, Weak},
};

const AREA_BYTES: usize = 5 * 1024 * 1024;
const MEMORY_BYTES: usize = 32 * 1024 * 1024;
const EVENT_BYTES: usize = 16 * 1024 * 1024;
const ENTRY_COUNT: usize = 4096;
const EVENT_COUNT: usize = 2048;
const MAGIC: &[u8] = b"RBSTORE\x01";
pub const MAX_STORAGE_FILE_BYTES: usize = AREA_BYTES + ENTRY_COUNT * 8 + 1024;

pub trait StoragePersistence {
    fn load(&self, id: &str) -> Result<Option<Vec<u8>>, String>;
    fn save(&self, id: &str, data: &[u8]) -> Result<(), String>;
}

#[derive(Clone, Default)]
pub struct StorageProfile(Rc<Store>);

impl StorageProfile {
    pub fn with_persistence(persistence: Rc<dyn StoragePersistence>) -> Self {
        Self(Rc::new(Store {
            persistence: Some(persistence),
            ..Store::default()
        }))
    }

    /// Persist dirty origins while retaining a failed write for a later retry.
    pub fn flush(&self) -> Result<(), String> {
        let Some(persistence) = &self.0.persistence else {
            return Ok(());
        };
        let pending: Vec<_> = self.0.retained.borrow().values().cloned().collect();
        let mut first_error = None;
        for area in pending {
            let result = persistence.save(
                &area.file_id(),
                &area
                    .contents
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .encode(&area.origin),
            );
            match result {
                Ok(()) => {
                    self.0.retained.borrow_mut().remove(&area.origin);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Default)]
pub struct TabStorage {
    local: StorageProfile,
    session: Rc<Store>,
}

impl TabStorage {
    pub fn new(local: StorageProfile) -> Self {
        Self {
            local,
            session: Rc::default(),
        }
    }

    pub(crate) fn connect(&self, origin: &str, session: bool, inbox: &Rc<Inbox>) -> Connection {
        let store = if session {
            &self.session
        } else {
            &self.local.0
        };
        let area = store.area(origin);
        {
            let mut subscribers = area.subscribers.borrow_mut();
            subscribers.retain(|receiver| receiver.strong_count() > 0);
            subscribers.push(Rc::downgrade(inbox));
        }
        Connection {
            store: store.clone(),
            area,
            inbox: inbox.clone(),
            session,
        }
    }
}

#[derive(Default)]
struct Store {
    persistence: Option<Rc<dyn StoragePersistence>>,
    areas: RefCell<BTreeMap<String, Weak<Area>>>,
    // Disk-backed stores retain dirty entries; in-memory stores retain nonempty areas.
    retained: RefCell<BTreeMap<String, Rc<Area>>>,
    retained_bytes: Cell<usize>,
    event_bytes: Rc<Cell<usize>>,
}

impl Store {
    fn area(&self, origin: &str) -> Rc<Area> {
        let mut areas = self.areas.borrow_mut();
        if let Some(area) = areas.get(origin).and_then(Weak::upgrade) {
            return area;
        }
        areas.retain(|_, area| area.strong_count() > 0);
        let area = Rc::new(Area {
            origin: origin.into(),
            contents: RefCell::new(None),
            subscribers: RefCell::default(),
        });
        areas.insert(origin.into(), Rc::downgrade(&area));
        area
    }
}

pub(crate) type Inbox = RefCell<VecDeque<Rc<Notification>>>;

struct Area {
    origin: String,
    contents: RefCell<Option<Entries>>,
    subscribers: RefCell<Vec<Weak<Inbox>>>,
}

impl Area {
    fn file_id(&self) -> String {
        let hash = digest(&SHA256, self.origin.as_bytes());
        hash.as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn recipients(&self, source: &Rc<Inbox>) -> Vec<Rc<Inbox>> {
        let mut subscribers = self.subscribers.borrow_mut();
        let mut recipients = Vec::new();
        subscribers.retain(|receiver| {
            let Some(receiver) = receiver.upgrade() else {
                return false;
            };
            if !Rc::ptr_eq(&receiver, source) {
                recipients.push(receiver);
            }
            true
        });
        recipients
    }
}

#[derive(Default)]
struct Entries {
    values: BTreeMap<JsString, JsString>,
    order: Vec<JsString>,
    bytes: usize,
}

impl Entries {
    fn size(&self) -> usize {
        self.bytes + self.order.len() * 96
    }

    fn encode(&self, origin: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes + self.order.len() * 8 + origin.len() + 16);
        out.extend_from_slice(MAGIC);
        write_count(&mut out, origin.len());
        out.extend_from_slice(origin.as_bytes());
        write_count(&mut out, self.order.len());
        for key in &self.order {
            for string in [key, &self.values[key]] {
                write_count(&mut out, string.len());
                for unit in string.as_utf16() {
                    out.extend_from_slice(&unit.to_le_bytes());
                }
            }
        }
        out
    }

    fn decode(mut input: &[u8], origin: &str) -> Option<Self> {
        if input.len() > MAX_STORAGE_FILE_BYTES {
            return None;
        }
        input = input.strip_prefix(MAGIC)?;
        let length = take_count(&mut input)?;
        if input.split_off(..length)? != origin.as_bytes() {
            return None;
        }
        let count = take_count(&mut input)?;
        if count > ENTRY_COUNT {
            return None;
        }
        let mut entries = Self::default();
        for _ in 0..count {
            let key = take_string(&mut input, &mut entries.bytes)?;
            let value = take_string(&mut input, &mut entries.bytes)?;
            if entries.values.insert(key.clone(), value).is_some() {
                return None;
            }
            entries.order.push(key);
        }
        input.is_empty().then_some(entries)
    }
}

fn write_count(out: &mut Vec<u8>, count: usize) {
    out.extend_from_slice(&(count as u32).to_le_bytes());
}

fn take_count(input: &mut &[u8]) -> Option<usize> {
    let bytes = input.split_off(..4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?) as usize)
}

fn take_string(input: &mut &[u8], used: &mut usize) -> Option<JsString> {
    let bytes = take_count(input)?.checked_mul(2)?;
    *used = used.checked_add(bytes)?;
    if *used > AREA_BYTES {
        return None;
    }
    let raw = input.split_off(..bytes)?;
    let (pairs, _) = raw.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
    Some(JsString::from_utf16(units))
}

fn len(value: &JsString) -> usize {
    value.len() * 2
}

pub(crate) enum StorageFailure {
    Unavailable(String),
    Quota,
}

pub(crate) enum Update {
    Set(JsString, JsString),
    Remove(JsString),
    Clear,
}

pub(crate) struct Notification {
    pub(crate) session: bool,
    pub(crate) key: Option<JsString>,
    pub(crate) old_value: Option<JsString>,
    pub(crate) new_value: Option<JsString>,
    pub(crate) url: String,
    usage: Rc<Cell<usize>>,
    bytes: usize,
}

impl Drop for Notification {
    fn drop(&mut self) {
        self.usage.set(self.usage.get() - self.bytes);
    }
}

pub(crate) struct Connection {
    store: Rc<Store>,
    area: Rc<Area>,
    inbox: Rc<Inbox>,
    session: bool,
}

impl Connection {
    pub(crate) fn ensure_loaded(&self) -> Result<(), StorageFailure> {
        if self.area.contents.borrow().is_some() {
            return Ok(());
        }
        let mut entries = Entries::default();
        if let Some(persistence) = &self.store.persistence
            && let Some(bytes) = persistence
                .load(&self.area.file_id())
                .map_err(StorageFailure::Unavailable)?
        {
            entries = Entries::decode(&bytes, &self.area.origin).ok_or_else(|| {
                StorageFailure::Unavailable("Cannot load invalid origin storage data".into())
            })?;
        }
        *self.area.contents.borrow_mut() = Some(entries);
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.area.contents.borrow().as_ref().unwrap().order.len()
    }

    pub(crate) fn key(&self, index: usize) -> Option<JsString> {
        self.area
            .contents
            .borrow()
            .as_ref()
            .unwrap()
            .order
            .get(index)
            .cloned()
    }

    pub(crate) fn keys(&self) -> Vec<JsString> {
        self.area.contents.borrow().as_ref().unwrap().order.clone()
    }

    pub(crate) fn get(&self, key: &JsString) -> Option<JsString> {
        self.area
            .contents
            .borrow()
            .as_ref()
            .unwrap()
            .values
            .get(key)
            .cloned()
    }

    pub(crate) fn update(&self, update: Update, url: String) -> Result<(), StorageFailure> {
        let mut slot = self.area.contents.borrow_mut();
        let entries = slot.as_mut().unwrap();
        let mut count = entries.order.len();
        let (key, old_value, new_value, size) = match &update {
            Update::Set(key, value) => {
                let old = entries.values.get(key);
                if old == Some(value) {
                    return Ok(());
                }
                let retained = if let Some(old) = old {
                    entries.bytes - len(old)
                } else {
                    count += 1;
                    entries.bytes + len(key)
                };
                (
                    Some(key.clone()),
                    old.cloned(),
                    Some(value.clone()),
                    retained + len(value),
                )
            }
            Update::Remove(key) => {
                let Some(old) = entries.values.get(key) else {
                    return Ok(());
                };
                count -= 1;
                (
                    Some(key.clone()),
                    Some(old.clone()),
                    None,
                    entries.bytes - len(key) - len(old),
                )
            }
            Update::Clear => {
                if count == 0 {
                    return Ok(());
                }
                count = 0;
                (None, None, None, 0)
            }
        };
        if count > ENTRY_COUNT || size > AREA_BYTES {
            return Err(StorageFailure::Quota);
        }
        let memory = self.store.persistence.is_none();
        let retained = if memory {
            let bytes = self.store.retained_bytes.get() - entries.size() + size + count * 96;
            if bytes > MEMORY_BYTES {
                return Err(StorageFailure::Quota);
            }
            bytes
        } else {
            0
        };
        let recipients = self.area.recipients(&self.inbox);
        let notice = if recipients.is_empty() {
            None
        } else {
            let bytes = 128
                + url.len()
                + [&key, &old_value, &new_value]
                    .iter()
                    .map(|value| value.as_ref().map_or(0, len))
                    .sum::<usize>();
            let allocated = self.store.event_bytes.get() + bytes;
            if allocated > EVENT_BYTES
                || recipients
                    .iter()
                    .any(|target| target.borrow().len() >= EVENT_COUNT)
            {
                return Err(StorageFailure::Quota);
            }
            self.store.event_bytes.set(allocated);
            Some(Rc::new(Notification {
                session: self.session,
                key,
                old_value,
                new_value,
                url,
                usage: self.store.event_bytes.clone(),
                bytes,
            }))
        };
        match update {
            Update::Set(key, value) => {
                let old = entries.values.insert(key.clone(), value);
                if old.is_none() {
                    entries.order.push(key);
                }
            }
            Update::Remove(key) => {
                entries.values.remove(&key);
                entries.order.retain(|name| name != &key);
            }
            Update::Clear => *entries = Entries::default(),
        }
        entries.bytes = size;
        let mut pinned = self.store.retained.borrow_mut();
        if memory && count == 0 {
            pinned.remove(&self.area.origin);
        } else {
            pinned.insert(self.area.origin.clone(), self.area.clone());
        }
        self.store.retained_bytes.set(retained);
        if let Some(notice) = notice {
            for target in recipients {
                target.borrow_mut().push_back(notice.clone());
            }
        }
        Ok(())
    }
}
