use crate::dom::{AttributeNamespace, Document, NodeId, mutation::Mutation};
use rust_browser_javascript::{ErrorKind, JsError, RootId};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    rc::Rc,
};

const OBSERVER_LIMIT: usize = 1024;
const REGISTRATION_LIMIT: usize = 4096;
const TRANSIENT_LIMIT: usize = 8192;
const FILTER_BYTES: usize = 1024 * 1024;
const RECORD_LIMIT: usize = 4096;
const RECORD_BYTES: usize = 4 * 1024 * 1024;
const TRACKING_WORK: usize = 1024 * 1024;

pub(super) type ObserverId = u64;

#[derive(Default)]
pub(super) struct Options {
    pub attributes: bool,
    pub character_data: bool,
    pub child_list: bool,
    pub subtree: bool,
    pub attribute_old_value: bool,
    pub character_data_old_value: bool,
    pub attribute_filter: Option<BTreeSet<String>>,
}
impl Options {
    fn filter_bytes(&self) -> usize {
        self.attribute_filter.as_ref().map_or(0, |entries| {
            entries.iter().map(|name| name.len() + 64).sum()
        })
    }
    fn interested(&self, change: &Mutation<'_>) -> Option<bool> {
        match change {
            Mutation::Children { .. } => self.child_list.then_some(false),
            Mutation::Attribute {
                name, namespace, ..
            } => (self.attributes
                && self
                    .attribute_filter
                    .as_ref()
                    .is_none_or(|filter| namespace.is_none() && filter.contains(*name)))
            .then_some(self.attribute_old_value),
            Mutation::CharacterData { .. } => {
                self.character_data.then_some(self.character_data_old_value)
            }
            Mutation::Removing { .. } => unreachable!("removals create transient registrations"),
        }
    }
}

struct Registration {
    observer: ObserverId,
    options: Rc<Options>,
    source: Option<NodeId>,
}

struct Observer {
    root: RootId,
    targets: BTreeSet<NodeId>,
    transients: BTreeSet<NodeId>,
    records: Vec<Record>,
}
impl Observer {
    fn new(root: RootId) -> Self {
        Self {
            root,
            targets: BTreeSet::new(),
            transients: BTreeSet::new(),
            records: Vec::new(),
        }
    }
}

pub(super) struct Record {
    pub change: Rc<Change>,
    pub old_value: Option<Rc<str>>,
    bytes: usize,
}
impl Record {
    pub(super) fn size(&self) -> usize {
        self.bytes
    }
}

#[derive(Default)]
pub(super) struct Change {
    pub kind: &'static str,
    pub target: Option<NodeId>,
    pub added: Rc<[NodeId]>,
    pub removed: Rc<[NodeId]>,
    pub previous: Option<NodeId>,
    pub next: Option<NodeId>,
    pub name: Option<String>,
    pub namespace: Option<AttributeNamespace>,
}
impl Change {
    fn from_mutation(change: &Mutation<'_>) -> Self {
        match change {
            Mutation::Children {
                target,
                added,
                removed,
                previous,
                next,
            } => Self {
                kind: "childList",
                target: Some(*target),
                added: Rc::from(*added),
                removed: Rc::from(*removed),
                previous: *previous,
                next: *next,
                ..Self::default()
            },
            Mutation::Attribute {
                target,
                name,
                namespace,
                ..
            } => Self {
                kind: "attributes",
                target: Some(*target),
                name: Some((*name).into()),
                namespace: *namespace,
                ..Self::default()
            },
            Mutation::CharacterData { target, .. } => Self {
                kind: "characterData",
                target: Some(*target),
                ..Self::default()
            },
            Mutation::Removing { .. } => unreachable!("removals create transient registrations"),
        }
    }
}

#[derive(Default)]
pub(in crate::script) struct Observers {
    observers: HashMap<ObserverId, Observer>,
    registrations: HashMap<NodeId, Vec<Registration>>,
    pending: BTreeSet<ObserverId>,
    registration_count: usize,
    transient_count: usize,
    filter_bytes: usize,
    record_count: usize,
    record_bytes: usize,
    tracking_work: usize,
    overflow: bool,
    checkpoint_active: bool,
    pub(super) queued: bool,
    pub(super) next_id: ObserverId,
    pub(super) job: Option<rust_browser_javascript::Value>,
}

impl Observers {
    pub(in crate::script) fn active(&self) -> bool {
        !self.observers.is_empty()
    }

    pub(super) fn can_track(&self) -> bool {
        self.active() && self.tracking_work <= TRACKING_WORK
    }

    pub(in crate::script) fn pending_checkpoint(&self) -> bool {
        self.queued || self.needs_job()
    }

    pub(super) fn needs_job(&self) -> bool {
        self.overflow || !self.pending.is_empty()
    }

    pub(super) fn observe(
        &mut self,
        id: ObserverId,
        target: NodeId,
        options: Options,
        root: impl FnOnce() -> RootId,
    ) -> Result<(), JsError> {
        let previous = self.registrations.get(&target).and_then(|list| {
            list.iter()
                .find(|item| item.observer == id && item.source.is_none())
        });
        let adding = previous.is_none();
        let bytes = self.filter_bytes - previous.map_or(0, |item| item.options.filter_bytes())
            + options.filter_bytes();
        if bytes > FILTER_BYTES
            || adding && self.registration_count >= REGISTRATION_LIMIT
            || !self.observers.contains_key(&id) && self.observers.len() >= OBSERVER_LIMIT
        {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "MutationObserver registration budget exceeded",
            ));
        }
        self.filter_bytes = bytes;
        if adding {
            self.observers
                .entry(id)
                .or_insert_with(|| Observer::new(root()))
                .targets
                .insert(target);
            self.registrations
                .entry(target)
                .or_default()
                .push(Registration {
                    observer: id,
                    options: Rc::new(options),
                    source: None,
                });
            self.registration_count += 1;
        } else {
            self.clear_transients(id, Some(target));
            let item = self
                .registrations
                .get_mut(&target)
                .unwrap()
                .iter_mut()
                .find(|item| item.observer == id && item.source.is_none())
                .unwrap();
            item.options = Rc::new(options);
        }
        Ok(())
    }

    pub(super) fn disconnect(&mut self, id: ObserverId) -> Option<RootId> {
        self.clear_transients(id, None);
        self.take_records(id);
        self.pending.remove(&id);
        let observer = self.observers.remove(&id)?;
        for target in observer.targets {
            let list = self.registrations.get_mut(&target).unwrap();
            let index = list
                .iter()
                .position(|item| item.observer == id && item.source.is_none())
                .unwrap();
            self.filter_bytes -= list.swap_remove(index).options.filter_bytes();
            self.registration_count -= 1;
            if list.is_empty() {
                self.registrations.remove(&target);
            }
        }
        Some(observer.root)
    }

    pub(super) fn take_records(&mut self, id: ObserverId) -> Vec<Record> {
        let Some(observer) = self.observers.get_mut(&id) else {
            return Vec::new();
        };
        let records = std::mem::take(&mut observer.records);
        self.record_count -= records.len();
        self.record_bytes -= records.iter().map(|record| record.bytes).sum::<usize>();
        records
    }

    pub(super) fn begin_delivery(&mut self) -> (BTreeSet<ObserverId>, bool) {
        self.queued = false;
        (
            std::mem::take(&mut self.pending),
            std::mem::take(&mut self.overflow),
        )
    }

    pub(super) fn begin_checkpoint(&mut self) -> bool {
        !std::mem::replace(&mut self.checkpoint_active, true)
    }

    pub(super) fn finish_checkpoint(&mut self) {
        self.checkpoint_active = false;
        self.tracking_work = 0;
    }

    pub(super) fn delivery(&mut self, id: ObserverId) -> Option<(RootId, Vec<Record>)> {
        let root = self.observers.get(&id)?.root;
        let records = self.take_records(id);
        self.clear_transients(id, None);
        (!records.is_empty()).then_some((root, records))
    }

    pub(super) fn discard_pending(&mut self) {
        // The VM discarded the notification job as part of aborting a task.
        self.begin_delivery();
        self.tracking_work = 0;
        for id in self.observers.keys().copied().collect::<Vec<_>>() {
            self.take_records(id);
            self.clear_transients(id, None);
        }
    }

    fn clear_transients(&mut self, id: ObserverId, source: Option<NodeId>) {
        let Some(observer) = self.observers.get_mut(&id) else {
            return;
        };
        observer.transients.retain(|target| {
            let list = self.registrations.get_mut(target).unwrap();
            let before = list.len();
            list.retain(|item| {
                item.observer != id
                    || item.source.is_none()
                    || source.is_some() && item.source != source
            });
            self.transient_count -= before - list.len();
            let remains = list
                .iter()
                .any(|item| item.observer == id && item.source.is_some());
            if list.is_empty() {
                self.registrations.remove(target);
            }
            remains
        });
    }

    fn visit_ancestors(
        &mut self,
        document: &Document,
        target: NodeId,
        per_registration: usize,
        mut visit: impl FnMut(NodeId, &Registration),
    ) -> bool {
        let mut current = Some(target);
        while let Some(node) = current {
            let list = self.registrations.get(&node);
            self.tracking_work = self
                .tracking_work
                .saturating_add(1 + list.map_or(0, |list| list.len() * per_registration));
            if self.tracking_work > TRACKING_WORK {
                self.overflow = true;
                return false;
            }
            for item in list.into_iter().flatten() {
                visit(node, item);
            }
            current = document.nodes[node.0].parent;
        }
        true
    }

    fn add_transients(&mut self, document: &Document, parent: NodeId, child: NodeId) {
        let mut sources = BTreeMap::new();
        if !self.visit_ancestors(document, parent, 1, |node, item| {
            if item.options.subtree {
                sources.insert(
                    (item.observer, item.source.unwrap_or(node)),
                    item.options.clone(),
                );
            }
        }) {
            return;
        }
        if sources.is_empty() {
            return;
        }
        self.tracking_work = self
            .tracking_work
            .saturating_add(self.registrations.get(&child).map_or(0, Vec::len));
        if self.tracking_work > TRACKING_WORK {
            self.overflow = true;
            return;
        }
        let entries = self.registrations.entry(child).or_default();
        let mut seen = entries
            .iter()
            .filter_map(|item| item.source.map(|source| (item.observer, source)))
            .collect::<BTreeSet<_>>();
        for ((id, source), options) in sources {
            if self.transient_count >= TRANSIENT_LIMIT {
                self.overflow = true;
                break;
            }
            if !seen.insert((id, source)) {
                continue;
            }
            self.transient_count += 1;
            entries.push(Registration {
                observer: id,
                options,
                source: Some(source),
            });
            self.observers
                .get_mut(&id)
                .unwrap()
                .transients
                .insert(child);
            self.pending.insert(id);
        }
        if entries.is_empty() {
            self.registrations.remove(&child);
        }
    }

    pub(in crate::script) fn note(&mut self, document: &Document, change: Mutation<'_>) {
        if !self.can_track() {
            return;
        }
        let (target, bytes, old, filter_work) = match &change {
            Mutation::Removing { parent, child } => {
                self.add_transients(document, *parent, *child);
                return;
            }
            Mutation::Children {
                target,
                added,
                removed,
                ..
            } => {
                if added.is_empty() && removed.is_empty() {
                    return;
                }
                (
                    *target,
                    (added.len() + removed.len()) * std::mem::size_of::<NodeId>(),
                    None,
                    1,
                )
            }
            Mutation::Attribute {
                target, name, old, ..
            } => (*target, name.len(), *old, 1 + name.len().min(4096)),
            Mutation::CharacterData { target, old } => (*target, 0, Some(*old), 1),
        };
        let mut interested = BTreeMap::new();
        if !self.visit_ancestors(document, target, filter_work, |node, item| {
            if (node == target || item.options.subtree)
                && let Some(old) = item.options.interested(&change)
            {
                *interested.entry(item.observer).or_insert(false) |= old;
            }
        }) {
            return;
        }
        let mut payload = None;
        let mut old_value: Option<Rc<str>> = None;
        for (id, want_old) in interested {
            let requested_old = old.filter(|_| want_old);
            let cost = 128 + bytes + requested_old.map_or(0, str::len);
            if self.record_count >= RECORD_LIMIT || self.record_bytes + cost > RECORD_BYTES {
                self.overflow = true;
                continue;
            }
            self.record_count += 1;
            self.record_bytes += cost;
            self.observers.get_mut(&id).unwrap().records.push(Record {
                change: payload
                    .get_or_insert_with(|| Rc::new(Change::from_mutation(&change)))
                    .clone(),
                old_value: requested_old
                    .map(|value| old_value.get_or_insert_with(|| Rc::from(value)).clone()),
                bytes: cost,
            });
            self.pending.insert(id);
        }
    }
}
