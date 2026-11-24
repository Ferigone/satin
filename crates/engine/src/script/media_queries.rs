//! CSSOM media queries share the stylesheet evaluator and the document viewport.
use super::{arg, event_target, getter, layout_api::LayoutHost, webidl};
use rust_browser_javascript::{ErrorKind, JsError, JsString, RootId, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

const MAX_QUERY_BYTES: usize = 4096;
const MAX_RETAINED_QUERY_BYTES: usize = 4 * 1024 * 1024;
const CHANGE_FIELDS: u32 = 0x4d51_4c45;

type Listeners = Rc<RefCell<BTreeMap<u64, RootId>>>;

struct Query {
    media: String,
    resident_bytes: Rc<Cell<usize>>,
    layout: LayoutHost,
    order: u64,
    last_match: Cell<bool>,
    listeners: Listeners,
}

impl Drop for Query {
    fn drop(&mut self) {
        self.resident_bytes
            .set(self.resident_bytes.get() - self.media.len());
    }
}

impl Query {
    fn matches(&self) -> bool {
        let viewport = self.layout.viewport();
        crate::css::stylesheet_media_applies(&self.media, (viewport.width, viewport.height))
    }
}

fn query(vm: &Vm, object: &Value) -> Result<Rc<Query>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("Illegal MediaQueryList receiver"))
}

// A listener must continue receiving changes even if the page drops its reference.
pub(super) fn retain_if_observed(vm: &mut Vm, object: &Value, observed: bool) {
    let Some(query) = vm.host_data::<Query>(object) else {
        return;
    };
    let mut listeners = query.listeners.borrow_mut();
    if observed {
        listeners.entry(query.order).or_insert_with(|| {
            query.last_match.set(query.matches());
            vm.root(object.clone())
        });
    } else if let Some(root) = listeners.remove(&query.order) {
        vm.unroot(root);
    }
}

// EventTarget uses the object's native data for dispatch state. The separate
// slot brands this event subtype and keeps its string visible to the VM.
fn set_change(vm: &mut Vm, event: &Value, media: JsString, matches: bool) -> Result<(), JsError> {
    vm.set_host_slot(event.clone(), CHANGE_FIELDS, Value::String(media))?;
    vm.set_host_slot(event.clone(), CHANGE_FIELDS + 1, matches.into())
}

pub(super) struct MediaQueries {
    listeners: Listeners,
    events: event_target::Events,
    event_proto: Value,
}

impl MediaQueries {
    pub(super) fn install(
        vm: &mut Vm,
        window: &Value,
        layout: &LayoutHost,
        events: &event_target::Events,
    ) -> Result<Self, JsError> {
        let prototype = vm.new_object_with_prototype(events.target_proto.clone())?;
        webidl::interface(vm, prototype.clone(), "MediaQueryList", 0, |_, _, _, _| {
            Err(JsError::type_error("Illegal constructor"))
        })?;
        // Native closures hold VM handles which must survive changes to global properties.
        vm.root(prototype.clone());
        getter(vm, prototype.clone(), "media", |vm, this, _| {
            Ok(query(vm, &this)?.media.clone().into())
        })?;
        getter(vm, prototype.clone(), "matches", |vm, this, _| {
            Ok(query(vm, &this)?.matches().into())
        })?;
        for (legacy, current) in [
            ("addListener", "addEventListener"),
            ("removeListener", "removeEventListener"),
        ] {
            let operation = vm.get_property(events.target_proto.clone(), current)?;
            vm.root(operation.clone());
            webidl::operation(vm, prototype.clone(), legacy, 1, move |vm, this, args| {
                query(vm, &this)?;
                webidl::required(args, 1, legacy)?;
                vm.call(operation.clone(), this, &["change".into(), args[0].clone()])
            })?;
        }
        event_target::install_handler(
            vm,
            prototype.clone(),
            "change",
            |vm, this| query(vm, this).map(|_| ()),
            |_, _| Ok(()),
        )?;
        let listeners = Listeners::default();
        let active = listeners.clone();
        let layout = layout.clone();
        let next_order = Cell::new(0u64);
        let resident_bytes = Rc::new(Cell::new(0usize));
        webidl::operation(vm, window.clone(), "matchMedia", 1, move |vm, _, args| {
            webidl::required(args, 1, "matchMedia")?;
            let source = webidl::usv_string(vm, &args[0], MAX_QUERY_BYTES)?;
            let media = crate::css::serialize_media_query_list(&source);
            let retained = resident_bytes.get() + media.len();
            if retained > MAX_RETAINED_QUERY_BYTES {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "Retained media query strings exceed their budget",
                ));
            }
            let order = next_order.get();
            next_order.set(order + 1);
            let object = vm.new_object_with_prototype(prototype.clone())?;
            event_target::attach(vm, object.clone())?;
            resident_bytes.set(retained);
            vm.set_host_data(
                object.clone(),
                Rc::new(Query {
                    resident_bytes: resident_bytes.clone(),
                    media,
                    order,
                    layout: layout.clone(),
                    listeners: active.clone(),
                    last_match: Cell::new(false),
                }),
            )?;
            Ok(object)
        })?;
        let event_proto = vm.new_object_with_prototype(events.event_proto.clone())?;
        let intrinsic = event_proto.clone();
        webidl::interface(
            vm,
            event_proto.clone(),
            "MediaQueryListEvent",
            1,
            move |vm, _, args, target| {
                let target = target
                    .ok_or_else(|| JsError::type_error("MediaQueryListEvent requires new"))?;
                webidl::required(args, 1, "MediaQueryListEvent")?;
                let kind = event_target::kind(vm, &args[0])?;
                let init = arg(args, 1);
                let flags = event_target::flags(vm, &init)?;
                let matches = event_target::option(vm, &init, "matches")?.is_truthy();
                let media = event_target::option(vm, &init, "media")?;
                let media = if media == Value::Undefined {
                    "".into()
                } else {
                    vm.to_string(&media)?
                };
                let object = webidl::instance(vm, target, &intrinsic)?;
                event_target::init_event(vm, object.clone(), kind, flags, false)?;
                set_change(vm, &object, media, matches)?;
                Ok(object)
            },
        )?;
        for (name, slot) in [("media", CHANGE_FIELDS), ("matches", CHANGE_FIELDS + 1)] {
            getter(vm, event_proto.clone(), name, move |vm, this, _| {
                vm.host_slot(&this, slot)
                    .ok_or_else(|| JsError::type_error("Illegal MediaQueryListEvent receiver"))
            })?;
        }
        vm.root(event_proto.clone());
        Ok(Self {
            listeners,
            events: events.clone().without_document(),
            event_proto,
        })
    }

    pub(super) fn report_changes(&self, vm: &mut Vm) -> Result<(), JsError> {
        // Handlers can add or remove listeners, including listeners for a later list.
        let ordered = self
            .listeners
            .borrow()
            .values()
            .copied()
            .collect::<Vec<_>>();
        for root in ordered {
            let Some(object) = vm.root_value(root) else {
                continue;
            };
            let query = query(vm, &object)?;
            vm.charge_work(query.media.len())?;
            let matches = query.matches();
            if matches == query.last_match.replace(matches) {
                continue;
            }
            let event = vm.new_object_with_prototype(self.event_proto.clone())?;
            event_target::init_event(vm, event.clone(), "change".into(), [false; 3], true)?;
            set_change(vm, &event, query.media.as_str().into(), matches)?;
            self.events.dispatch(vm, object, event)?;
        }
        Ok(())
    }
}
