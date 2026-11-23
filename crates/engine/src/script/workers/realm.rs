//! JavaScript and its event loop checkpoint for a dedicated worker thread.
mod timers;

use crate::script::{
    arg, dom_api, event_target, fatal, getter, message_ports::message_event, scheduler, webidl,
};
use rust_browser_javascript::{ErrorKind, JsError, JsString, StructuredClonePacket, Value, Vm};
use std::{rc::Rc, time::Instant};
use url::{Position, Url};

const NAME_SLOT: u32 = 0;
const LOCATION_SLOT: u32 = 1;
const MAX_MICROTASKS: usize = 4096;

pub(super) struct WorkerRealm {
    vm: Vm,
    events: event_target::Events,
    message_events: message_event::MessageEvents,
    timers: timers::Timers,
}

impl WorkerRealm {
    pub(super) fn new(
        mut vm: Vm,
        name: JsString,
        url: &Url,
        post: impl Fn(&mut Vm, &[Value]) -> Result<(), JsError> + 'static,
        close: impl Fn() + 'static,
    ) -> Result<Self, JsError> {
        dom_api::install_errors(&mut vm)?;
        let events = event_target::install_worker(&mut vm)?;
        let global = vm.global();
        let object = vm.get_global("Object")?;
        let set_prototype = vm.get_property(object.clone(), "setPrototypeOf")?;
        let freeze = vm.get_property(object, "freeze")?;
        let event_constructor = vm.get_global("Event")?;
        let target_constructor = vm.get_global("EventTarget")?;
        let message_events =
            message_event::install(&mut vm, &events, &event_constructor, &set_prototype, freeze)?;

        let worker_proto = vm.new_object_with_prototype(events.target_proto.clone())?;
        let worker = webidl::interface(
            &mut vm,
            worker_proto.clone(),
            "WorkerGlobalScope",
            0,
            |_, _, _, _| Err(JsError::type_error("Illegal WorkerGlobalScope constructor")),
        )?;
        let dedicated_proto = vm.new_object_with_prototype(worker_proto.clone())?;
        let dedicated = webidl::interface(
            &mut vm,
            dedicated_proto.clone(),
            "DedicatedWorkerGlobalScope",
            0,
            |_, _, _, _| {
                Err(JsError::type_error(
                    "Illegal DedicatedWorkerGlobalScope constructor",
                ))
            },
        )?;
        for (object, prototype) in [
            (worker.clone(), target_constructor),
            (dedicated, worker),
            (global.clone(), dedicated_proto),
        ] {
            vm.call(
                set_prototype.clone(),
                Value::Undefined,
                &[object, prototype],
            )?;
        }
        vm.mark_platform_object(&global)?;
        event_target::attach(&mut vm, global.clone())?;

        let location = install_location(&mut vm, url)?;
        vm.set_host_slot(global.clone(), LOCATION_SLOT, location)?;
        vm.set_host_slot(global.clone(), NAME_SLOT, name.into())?;
        getter(&mut vm, worker_proto.clone(), "self", |vm, this, _| {
            scope(vm, &this)?;
            Ok(vm.global())
        })?;
        getter(&mut vm, worker_proto, "location", |vm, this, _| {
            scope(vm, &this)?;
            Ok(vm.host_slot(&vm.global(), LOCATION_SLOT).unwrap())
        })?;
        let get_name = vm.new_native_function("get name", |vm, this, _| {
            scope(vm, &this)?;
            Ok(vm.host_slot(&vm.global(), NAME_SLOT).unwrap())
        });
        let set_name = vm.new_native_function("set name", |vm, this, args| {
            scope(vm, &this)?;
            vm.define_data_property(vm.global(), "name", arg(args, 0), true, true, true)?;
            Ok(Value::Undefined)
        });
        vm.define_data_property(set_name.clone(), "length", 1.into(), false, false, true)?;
        vm.define_accessor(global.clone(), "name", Some(get_name), Some(set_name))?;
        for kind in ["message", "messageerror"] {
            event_target::install_handler(&mut vm, global.clone(), kind, scope, |_, _| Ok(()))?;
        }
        webidl::operation(
            &mut vm,
            global.clone(),
            "postMessage",
            1,
            move |vm, this, args| {
                scope(vm, &this)?;
                webidl::required(args, 1, "DedicatedWorkerGlobalScope.postMessage")?;
                post(vm, args)?;
                Ok(Value::Undefined)
            },
        )?;
        webidl::operation(&mut vm, global.clone(), "close", 0, move |vm, this, _| {
            scope(vm, &this)?;
            close();
            Ok(Value::Undefined)
        })?;
        scheduler::install_queue_microtask(&mut vm, global.clone())?;
        let timers = timers::Timers::install(&mut vm, global)?;

        Ok(Self {
            vm,
            events,
            message_events,
            timers,
        })
    }

    pub(super) fn vm_mut(&mut self) -> &mut Vm {
        &mut self.vm
    }

    pub(super) fn evaluate(&mut self, source: &str, identifier: &str) -> Result<(), JsError> {
        run_task(&mut self.vm, |vm| {
            vm.eval_with_identifier(source, identifier).map(|_| ())
        })
    }

    pub(super) fn dispatch_packet(&mut self, packet: StructuredClonePacket) -> Result<(), JsError> {
        self.dispatch(|vm| match vm.deserialize_clone_packet(packet) {
            Ok(data) => ("message", data),
            Err(_) => ("messageerror", Value::Null),
        })
    }

    pub(super) fn timer_deadline(&self) -> Option<Instant> {
        self.timers.deadline()
    }

    pub(super) fn dispatch_timer(&mut self, now: Instant) -> Result<(), JsError> {
        let timers = &self.timers;
        run_task(&mut self.vm, |vm| timers.fire(vm, now))
    }

    #[cfg(test)]
    fn dispatch_message(&mut self, data: Value) -> Result<(), JsError> {
        self.dispatch(|_| ("message", data))
    }

    #[cfg(test)]
    fn dispatch_message_error(&mut self) -> Result<(), JsError> {
        self.dispatch(|_| ("messageerror", Value::Null))
    }

    fn dispatch(
        &mut self,
        payload: impl FnOnce(&mut Vm) -> (&'static str, Value),
    ) -> Result<(), JsError> {
        let events = &self.events;
        let message_events = &self.message_events;
        run_task(&mut self.vm, |vm| {
            let (kind, data) = payload(vm);
            let event = message_events.received_from_peer(vm, kind, data)?;
            let mut reported = None;
            let global = vm.global();
            events.dispatch_with_report(vm, global, event, |_, error| {
                if reported.is_none() {
                    reported = Some(error.clone());
                }
                Ok(())
            })?;
            reported.map_or(Ok(()), Err)
        })
    }
}

fn scope(vm: &Vm, receiver: &Value) -> Result<(), JsError> {
    if receiver.is_nullish() || *receiver == vm.global() {
        Ok(())
    } else {
        Err(JsError::type_error("Illegal worker global receiver"))
    }
}

fn install_location(vm: &mut Vm, url: &Url) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "WorkerLocation", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal WorkerLocation constructor"))
    })?;
    for name in [
        "href", "origin", "protocol", "host", "hostname", "port", "pathname", "search", "hash",
    ] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            let url = worker_url(vm, &this)?;
            let text = match name {
                "href" => url.as_str().into(),
                "origin" => url.origin().ascii_serialization(),
                "protocol" => format!("{}:", url.scheme()),
                "host" => url[Position::BeforeHost..Position::AfterPort].into(),
                "hostname" => url.host_str().unwrap_or_default().into(),
                "port" => url.port().map_or_else(String::new, |port| port.to_string()),
                "pathname" => url.path().into(),
                "search" => url
                    .query()
                    .filter(|query| !query.is_empty())
                    .map_or_else(String::new, |query| format!("?{query}")),
                _ => url
                    .fragment()
                    .filter(|fragment| !fragment.is_empty())
                    .map_or_else(String::new, |fragment| format!("#{fragment}")),
            };
            Ok(text.into())
        })?;
    }
    webidl::operation(vm, prototype.clone(), "toString", 0, |vm, this, _| {
        Ok(worker_url(vm, &this)?.as_str().into())
    })?;
    let location = vm.new_object_with_prototype(prototype)?;
    vm.mark_platform_object(&location)?;
    vm.set_host_data(location.clone(), Rc::new(url.clone()))?;
    Ok(location)
}

fn worker_url(vm: &Vm, value: &Value) -> Result<Rc<Url>, JsError> {
    vm.host_data(value)
        .ok_or_else(|| JsError::type_error("Illegal WorkerLocation receiver"))
}

fn run_task(
    vm: &mut Vm,
    operation: impl FnOnce(&mut Vm) -> Result<(), JsError>,
) -> Result<(), JsError> {
    let result = vm.with_task(|vm| {
        let operation = operation(vm);
        if operation.as_ref().is_err_and(fatal) {
            vm.clear_jobs();
            return operation;
        }
        let checkpoint = checkpoint(vm);
        if checkpoint.as_ref().is_err_and(fatal) {
            return checkpoint;
        }
        operation.and(checkpoint)
    });
    vm.collect_garbage();
    result
}

fn checkpoint(vm: &mut Vm) -> Result<(), JsError> {
    let mut first_error = None;
    for _ in 0..MAX_MICROTASKS {
        if !vm.has_pending_jobs() {
            break;
        }
        if let Err(error) = vm.drain_jobs(1) {
            if fatal(&error) {
                vm.clear_jobs();
                return Err(error);
            }
            first_error.get_or_insert(error);
        }
    }
    if vm.has_pending_jobs() {
        vm.clear_jobs();
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Worker microtask checkpoint budget exhausted",
        ));
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const URL: &str = "blob:https://example.test/worker#source";

    fn realm(output: &Rc<RefCell<Vec<String>>>, closed: &Rc<Cell<bool>>) -> WorkerRealm {
        let output = output.clone();
        let closed = closed.clone();
        WorkerRealm::new(
            Vm::new(),
            "named".into(),
            &Url::parse(URL).unwrap(),
            move |vm, args| {
                output
                    .borrow_mut()
                    .push(vm.to_string(&args[0])?.to_string_lossy());
                Ok(())
            },
            move || closed.set(true),
        )
        .unwrap()
    }

    #[test]
    fn worker_global_has_its_own_event_target_and_location() {
        let output = Rc::default();
        let closed = Rc::default();
        let mut realm = realm(&output, &closed);
        realm
            .evaluate(
                r#"
            postMessage([
                self === globalThis,
                self instanceof DedicatedWorkerGlobalScope,
                self instanceof WorkerGlobalScope,
                self instanceof EventTarget,
                typeof window, typeof document, self.name,
                location instanceof WorkerLocation,
                location.href, location.protocol, location.origin,
                location.host, location.pathname, location.hash,
                String(location), new DOMException('payload', 'DataCloneError').code
            ].join('|'));
            self.name = 'changed';
            postMessage(name);
            "#,
                URL,
            )
            .unwrap();
        assert_eq!(
            *output.borrow(),
            [
                "true|true|true|true|undefined|undefined|named|true|blob:https://example.test/worker#source|blob:|https://example.test||https://example.test/worker|#source|blob:https://example.test/worker#source|25",
                "changed",
            ]
        );
        assert!(!closed.get());
    }

    #[test]
    fn worker_runs_message_listeners_and_microtasks_in_order() {
        let output = Rc::default();
        let closed = Rc::default();
        let mut realm = realm(&output, &closed);
        realm
            .evaluate(
                r#"
            queueMicrotask(() => postMessage('startup'));
            addEventListener('message', function(event) {
                postMessage('listener ' + event.data);
            });
            onmessage = function(event) {
                postMessage('handler ' + event.data);
                if (event instanceof MessageEvent && event.isTrusted &&
                    this === self && event.target === self && event.source === null &&
                    event.ports.length === 0 && Object.isFrozen(event.ports)) {
                    Promise.resolve().then(() => postMessage('microtask ' + event.data));
                }
                if (event.data === 'second') close();
            };
            onmessageerror = event => postMessage(event.type + ' ' + event.data);
            "#,
                URL,
            )
            .unwrap();
        realm.dispatch_message("first".into()).unwrap();
        realm.dispatch_message("second".into()).unwrap();
        realm.dispatch_message_error().unwrap();
        assert_eq!(
            *output.borrow(),
            [
                "startup",
                "listener first",
                "handler first",
                "microtask first",
                "listener second",
                "handler second",
                "microtask second",
                "messageerror null",
            ]
        );
        assert!(closed.get());
    }

    #[test]
    fn a_throwing_listener_reports_the_error_and_finishes_the_checkpoint() {
        let output = Rc::default();
        let mut realm = realm(&output, &Rc::default());
        realm
            .evaluate(
                r#"
            addEventListener('message', () => {
                queueMicrotask(() => postMessage('checkpoint'));
                throw Error('listener failed');
            });
            addEventListener('message', () => postMessage('next listener'));
            "#,
                URL,
            )
            .unwrap();
        let error = realm.dispatch_message(Value::Undefined).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Error);
        assert!(error.message.contains("listener failed"));
        assert_eq!(*output.borrow(), ["next listener", "checkpoint"]);
    }
}
