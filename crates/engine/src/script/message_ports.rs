//! Same-document message channels. Endpoint and packet references are VM
//! edges, while the task source roots only currently runnable queues.
pub(super) mod message_event;

use super::{arg, event_target, getter, structured_clone, webidl};
use message_event::MessageEvents;
use rust_browser_javascript::{
    ErrorKind, JsError, PlatformTransfer, RootId, StructuredCloneError, Value, Vm,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

const MAX_PORT_MESSAGES: usize = 256;
const MAX_DOCUMENT_MESSAGES: usize = 2048;

const PORT_ENDPOINT: u32 = 20;
const CHANNEL_LEFT: u32 = 0;
const CHANNEL_RIGHT: u32 = 1;
const ENDPOINT_OWNER: u32 = 0;
const ENDPOINT_PEER: u32 = 1;
const ENDPOINT_HEAD: u32 = 2;
const ENDPOINT_TAIL: u32 = 3;
const PACKET_NEXT: u32 = 0;
const PACKET_PAYLOAD: u32 = 1;

struct Channel;

#[derive(Default)]
struct Port {
    enabled: Cell<bool>,
    detached: Cell<bool>,
}

#[derive(Default)]
struct Endpoint {
    reserved: Rc<Cell<usize>>,
    scheduled: Cell<Option<(u64, RootId)>>,
}

#[derive(Default)]
struct Packet {
    serial: Cell<u64>,
    reservation: RefCell<Option<MessageReservation>>,
}

#[derive(Default)]
struct TaskSource {
    serial: Cell<u64>,
    reserved: Rc<Cell<usize>>,
    ready: RefCell<BTreeMap<u64, RootId>>,
}

struct MessageReservation {
    document: Rc<Cell<usize>>,
    endpoint: Rc<Cell<usize>>,
}

impl Drop for MessageReservation {
    fn drop(&mut self) {
        self.document.set(self.document.get() - 1);
        self.endpoint.set(self.endpoint.get() - 1);
    }
}

#[derive(Clone)]
pub(super) struct MessagePorts {
    port_prototype: Value,
    events: event_target::Events,
    message_events: MessageEvents,
    tasks: Rc<TaskSource>,
}

fn brand(vm: &Vm, object: &Value) -> Result<Rc<Port>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("Illegal MessagePort receiver"))
}

fn endpoint(vm: &Vm, port: &Value) -> Option<Value> {
    vm.host_slot(port, PORT_ENDPOINT)
        .filter(|value| value.as_object().is_some())
}

fn slot(vm: &Vm, object: &Value, index: u32) -> Value {
    vm.host_slot(object, index)
        .expect("initialized message slot")
}

// All replacements after clone commit use already allocated slots and contain
// only internal object handles or null. No author code or allocation can run.
fn replace(vm: &mut Vm, object: &Value, index: u32, value: Value) {
    vm.set_host_slot(object.clone(), index, value)
        .expect("preallocated message slot");
}

impl MessagePorts {
    pub(super) fn received_from_network(
        &self,
        vm: &mut Vm,
        data: Value,
        origin: String,
    ) -> Result<Value, JsError> {
        self.message_events.received_from_network(vm, data, origin)
    }

    pub(super) fn received_from_peer(
        &self,
        vm: &mut Vm,
        kind: &str,
        data: Value,
    ) -> Result<Value, JsError> {
        self.message_events.received_from_peer(vm, kind, data)
    }

    fn new_port(&self, vm: &mut Vm) -> Result<Value, JsError> {
        let object = vm.new_object_with_prototype(self.port_prototype.clone())?;
        vm.mark_platform_object(&object)?;
        event_target::attach(vm, object.clone())?;
        vm.set_host_data(object.clone(), Rc::new(Port::default()))?;
        vm.set_host_slot(object.clone(), PORT_ENDPOINT, Value::Null)?;
        Ok(object)
    }

    fn new_endpoint(vm: &mut Vm, owner: &Value) -> Result<Value, JsError> {
        let value = vm.new_object_with_prototype(Value::Null)?;
        vm.set_host_data(value.clone(), Rc::new(Endpoint::default()))?;
        for (index, item) in [
            (ENDPOINT_OWNER, owner.clone()),
            (ENDPOINT_PEER, Value::Null),
            (ENDPOINT_HEAD, Value::Null),
            (ENDPOINT_TAIL, Value::Null),
        ] {
            vm.set_host_slot(value.clone(), index, item)?;
        }
        replace(vm, owner, PORT_ENDPOINT, value.clone());
        Ok(value)
    }

    fn channel(&self, vm: &mut Vm, channel: &Value) -> Result<(), JsError> {
        vm.set_host_data(channel.clone(), Rc::new(Channel))?;
        let left = self.new_port(vm)?;
        let right = self.new_port(vm)?;
        let a = Self::new_endpoint(vm, &left)?;
        let b = Self::new_endpoint(vm, &right)?;
        replace(vm, &a, ENDPOINT_PEER, b.clone());
        replace(vm, &b, ENDPOINT_PEER, a);
        vm.set_host_slot(channel.clone(), CHANNEL_LEFT, left)?;
        vm.set_host_slot(channel.clone(), CHANNEL_RIGHT, right)?;
        Ok(())
    }

    fn start(&self, vm: &mut Vm, value: &Value) -> Result<(), JsError> {
        let port = brand(vm, value)?;
        if !port.enabled.replace(true)
            && let Some(endpoint) = endpoint(vm, value)
        {
            self.schedule(vm, &endpoint);
        }
        Ok(())
    }

    fn close(vm: &mut Vm, value: &Value) -> Result<(), JsError> {
        let port = brand(vm, value)?;
        if !port.detached.replace(true)
            && let Some(endpoint) = endpoint(vm, value)
        {
            let peer = slot(vm, &endpoint, ENDPOINT_PEER);
            if !peer.is_nullish() {
                replace(vm, &peer, ENDPOINT_PEER, Value::Null);
                replace(vm, &endpoint, ENDPOINT_PEER, Value::Null);
            }
        }
        Ok(())
    }

    fn schedule(&self, vm: &mut Vm, endpoint: &Value) {
        let queue = vm.host_data::<Endpoint>(endpoint).unwrap();
        let owner = slot(vm, endpoint, ENDPOINT_OWNER);
        let head = slot(vm, endpoint, ENDPOINT_HEAD);
        if queue.scheduled.get().is_some()
            || head.is_nullish()
            || !vm.host_data::<Port>(&owner).unwrap().enabled.get()
        {
            return;
        }
        let serial = vm.host_data::<Packet>(&head).unwrap().serial.get();
        let root = vm.root(endpoint.clone());
        self.tasks.ready.borrow_mut().insert(serial, root);
        queue.scheduled.set(Some((serial, root)));
    }

    fn cancel_schedule(&self, vm: &mut Vm, endpoint: &Value) {
        let queue = vm.host_data::<Endpoint>(endpoint).unwrap();
        if let Some((serial, root)) = queue.scheduled.take() {
            self.tasks.ready.borrow_mut().remove(&serial);
            vm.unroot(root);
        }
    }

    pub(super) fn has_tasks(&self) -> bool {
        !self.tasks.ready.borrow().is_empty()
    }

    pub(super) fn dispatch_next(&self, vm: &mut Vm) -> Result<bool, JsError> {
        let Some((_, root)) = self.tasks.ready.borrow_mut().pop_first() else {
            return Ok(false);
        };
        let endpoint = vm.unroot(root).expect("scheduled port root");
        let queue = vm.host_data::<Endpoint>(&endpoint).unwrap();
        queue.scheduled.set(None);
        let target = slot(vm, &endpoint, ENDPOINT_OWNER);
        let packet = slot(vm, &endpoint, ENDPOINT_HEAD);
        let next = slot(vm, &packet, PACKET_NEXT);
        replace(vm, &endpoint, ENDPOINT_HEAD, next.clone());
        if next.is_nullish() {
            replace(vm, &endpoint, ENDPOINT_TAIL, Value::Null);
        }
        replace(vm, &packet, PACKET_NEXT, Value::Null);
        vm.host_data::<Packet>(&packet)
            .unwrap()
            .reservation
            .borrow_mut()
            .take();
        self.schedule(vm, &endpoint);
        let payload = slot(vm, &packet, PACKET_PAYLOAD);
        let event = self.message_events.received(vm, payload)?;
        self.events.dispatch(vm, target, event)?;
        Ok(true)
    }

    fn reserve_packet(
        &self,
        vm: &mut Vm,
        endpoint: &Value,
    ) -> Result<(Value, MessageReservation), JsError> {
        let queue = vm.host_data::<Endpoint>(endpoint).unwrap();
        let document = &self.tasks.reserved;
        if queue.reserved.get() >= MAX_PORT_MESSAGES || document.get() >= MAX_DOCUMENT_MESSAGES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "MessagePort queue limit exceeded",
            ));
        }
        let packet = vm.new_object_with_prototype(Value::Null)?;
        vm.set_host_data(packet.clone(), Rc::new(Packet::default()))?;
        vm.set_host_slot(packet.clone(), PACKET_NEXT, Value::Null)?;
        vm.set_host_slot(packet.clone(), PACKET_PAYLOAD, Value::Null)?;
        document.set(document.get() + 1);
        queue.reserved.set(queue.reserved.get() + 1);
        let reservation = MessageReservation {
            document: document.clone(),
            endpoint: queue.reserved.clone(),
        };
        Ok((packet, reservation))
    }

    fn enqueue(
        &self,
        vm: &mut Vm,
        endpoint: &Value,
        packet: Value,
        payload: Value,
        reservation: MessageReservation,
    ) {
        let data = vm.host_data::<Packet>(&packet).unwrap();
        let serial = self.tasks.serial.get();
        self.tasks
            .serial
            .set(serial.checked_add(1).expect("message serial overflow"));
        data.serial.set(serial);
        *data.reservation.borrow_mut() = Some(reservation);
        replace(vm, &packet, PACKET_PAYLOAD, payload);
        let tail = slot(vm, endpoint, ENDPOINT_TAIL);
        if tail.is_nullish() {
            replace(vm, endpoint, ENDPOINT_HEAD, packet.clone());
        } else {
            replace(vm, &tail, PACKET_NEXT, packet.clone());
        }
        replace(vm, endpoint, ENDPOINT_TAIL, packet);
        self.schedule(vm, endpoint);
    }

    pub(super) fn prepare_transfer(
        &self,
        vm: &mut Vm,
        source: &Value,
    ) -> Result<Option<PlatformTransfer>, StructuredCloneError> {
        let Some(port) = vm.host_data::<Port>(source) else {
            return Ok(None);
        };
        let object = self.new_port(vm)?;
        let source = source.clone();
        let destination = object.clone();
        let validate = port.clone();
        let host = self.clone();
        Ok(Some(PlatformTransfer {
            object,
            validate: Box::new(move |_| {
                if validate.detached.get() {
                    Err(StructuredCloneError::DataClone("MessagePort is detached"))
                } else {
                    Ok(())
                }
            }),
            commit: Box::new(move |vm| {
                let endpoint = endpoint(vm, &source).expect("attached transfer source");
                host.cancel_schedule(vm, &endpoint);
                port.detached.set(true);
                replace(vm, &source, PORT_ENDPOINT, Value::Null);
                replace(vm, &destination, PORT_ENDPOINT, endpoint.clone());
                replace(vm, &endpoint, ENDPOINT_OWNER, destination);
            }),
        }))
    }

    fn discard(&self, vm: &mut Vm, transferred: Vec<Value>) -> Result<(), JsError> {
        for port in transferred {
            if vm.host_data::<Port>(&port).is_some()
                && let Some(endpoint) = endpoint(vm, &port)
            {
                Self::close(vm, &port)?;
                self.cancel_schedule(vm, &endpoint);
                replace(vm, &endpoint, ENDPOINT_HEAD, Value::Null);
                replace(vm, &endpoint, ENDPOINT_TAIL, Value::Null);
            }
        }
        Ok(())
    }

    fn post_message(
        &self,
        vm: &mut Vm,
        this: Value,
        args: &[Value],
        cloning: &structured_clone::CloneHost,
    ) -> Result<Value, JsError> {
        brand(vm, &this)?;
        webidl::required(args, 1, "MessagePort.postMessage")?;
        let transfer = structured_clone::transfer_argument(vm, arg(args, 1))?;
        if transfer.contains(&this) {
            return Err(cloning.data_clone_error(vm, "Cannot transfer the sending MessagePort"));
        }
        let message = args[0].clone();
        let connection = endpoint(vm, &this).and_then(|sender| {
            let peer = slot(vm, &sender, ENDPOINT_PEER);
            (!peer.is_nullish()).then_some((sender, peer))
        });
        if let Some((sender, peer)) = connection {
            let target = slot(vm, &peer, ENDPOINT_OWNER);
            if !transfer.contains(&target) {
                let (packet, reservation) = self.reserve_packet(vm, &peer)?;
                let ports = transfer
                    .iter()
                    .filter(|value| vm.host_data::<Port>(value).is_some())
                    .cloned()
                    .collect::<Vec<_>>();
                let ports = vm.try_new_array(&ports)?;
                // The envelope includes ports that are not referenced by the
                // message. Both fields then share one clone identity table.
                let payload = vm.try_new_array(&[message, ports])?;
                let cloned = cloning.clone_graph(vm, payload, &transfer, self)?;
                // Getters may close either end or transfer the receiver.
                // An attached sender still reaches its peer's current owner.
                if endpoint(vm, &this).as_ref() == Some(&sender)
                    && slot(vm, &sender, ENDPOINT_PEER) == peer
                {
                    self.enqueue(vm, &peer, packet, cloned.value, reservation);
                } else {
                    self.discard(vm, cloned.transferred)?;
                }
                return Ok(Value::Undefined);
            }
        }
        let cloned = cloning.clone_graph(vm, message, &transfer, self)?;
        self.discard(vm, cloned.transferred)?;
        Ok(Value::Undefined)
    }
}

pub(super) fn install(
    vm: &mut Vm,
    events: event_target::Events,
    cloning: structured_clone::CloneHost,
) -> Result<MessagePorts, JsError> {
    let event_constructor = vm.get_global("Event")?;
    let target_constructor = vm.get_global("EventTarget")?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object.clone(), "setPrototypeOf")?;
    let freeze = vm.get_property(object, "freeze")?;
    let message_events =
        message_event::install(vm, &events, &event_constructor, &set_prototype, freeze)?;
    let port_prototype = vm.new_object_with_prototype(events.target_proto.clone())?;
    vm.root(port_prototype.clone());
    let host = MessagePorts {
        port_prototype: port_prototype.clone(),
        events,
        message_events,
        tasks: Rc::new(TaskSource::default()),
    };
    let port_constructor = webidl::interface(
        vm,
        port_prototype.clone(),
        "MessagePort",
        0,
        |_, _, _, _| Err(JsError::type_error("Illegal MessagePort constructor")),
    )?;
    vm.call(
        set_prototype,
        Value::Undefined,
        &[port_constructor, target_constructor],
    )?;
    let posting = host.clone();
    webidl::operation(
        vm,
        port_prototype.clone(),
        "postMessage",
        1,
        move |vm, this, args| posting.post_message(vm, this, args, &cloning),
    )?;
    let starting = host.clone();
    webidl::operation(
        vm,
        port_prototype.clone(),
        "start",
        0,
        move |vm, this, _| {
            starting.start(vm, &this)?;
            Ok(Value::Undefined)
        },
    )?;
    webidl::operation(vm, port_prototype.clone(), "close", 0, |vm, this, _| {
        MessagePorts::close(vm, &this)?;
        Ok(Value::Undefined)
    })?;
    let starting = host.clone();
    event_target::install_handler(
        vm,
        port_prototype.clone(),
        "message",
        |vm, this| brand(vm, this).map(|_| ()),
        move |vm, this| starting.start(vm, this),
    )?;
    event_target::install_handler(
        vm,
        port_prototype,
        "messageerror",
        |vm, this| brand(vm, this).map(|_| ()),
        |_, _| Ok(()),
    )?;

    let channel_prototype = vm.new_object();
    let intrinsic = channel_prototype.clone();
    let channels = host.clone();
    webidl::interface(
        vm,
        channel_prototype.clone(),
        "MessageChannel",
        0,
        move |vm, _, _, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("MessageChannel requires new"))?;
            let object = webidl::instance(vm, target, &intrinsic)?;
            channels.channel(vm, &object)?;
            Ok(object)
        },
    )?;
    for (name, index) in [("port1", CHANNEL_LEFT), ("port2", CHANNEL_RIGHT)] {
        getter(vm, channel_prototype.clone(), name, move |vm, this, _| {
            if vm.host_data::<Channel>(&this).is_none() {
                return Err(JsError::type_error("Illegal MessageChannel receiver"));
            }
            Ok(slot(vm, &this, index))
        })?;
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use crate::{LoadedPage, script::ScriptRuntime};

    #[test]
    fn reachable_tasks_survive_collection_and_disabled_cycles_release_their_quota() {
        let mut runtime = ScriptRuntime::new(&LoadedPage::from_html(
            "https://example.test/",
            "<p>Ports</p>",
        ))
        .unwrap();
        runtime
            .eval(
                "var delivered = 0; {
                    const pending = new MessageChannel();
                    pending.port2.onmessage = event => { delivered = event.data; };
                    pending.port1.postMessage(42);
                }
                for (let index = 0; index < 16; index++) {
                    const disabled = new MessageChannel();
                    disabled.port1.postMessage({port: index});
                }",
            )
            .unwrap();
        assert_eq!(runtime.messages.tasks.reserved.get(), 17);
        runtime.vm.collect_garbage();
        assert_eq!(runtime.messages.tasks.reserved.get(), 1);
        assert!(runtime.messages.has_tasks());
        runtime.pump();
        assert_eq!(runtime.eval("delivered").unwrap().as_number(), Some(42.0));
        assert_eq!(runtime.messages.tasks.reserved.get(), 0);
        assert!(!runtime.messages.has_tasks());
    }
}
