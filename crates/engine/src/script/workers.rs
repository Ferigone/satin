//! Classic dedicated workers run in separate JavaScript VMs.
mod mailbox;
mod realm;

use super::{
    Shared, State, abort::dom_exception_error, arg, diagnostic, document_base, event_target, fatal,
    message_ports::MessagePorts, structured_clone, webidl,
};
use crate::loader::BlobWorkerPolicy;
use mailbox::{Budget, Envelope, MAX_PACKET_BYTES, Mailbox};
use realm::WorkerRealm;
use rust_browser_javascript::{
    ErrorKind, JsError, JsString, RootId, StructuredCloneError, StructuredClonePacket, Value, Vm,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ops::Range,
    rc::{Rc, Weak},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use url::Url;

const MAX_DOCUMENT_WORKERS: usize = 8;
const MAX_PROCESS_WORKERS: usize = 32;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
static ACTIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);

type Wake = Arc<dyn Fn() + Send + Sync>;

enum Notice {
    Message(StructuredClonePacket),
    Error(String),
}

struct Connection {
    to_worker: Mailbox<StructuredClonePacket>,
    to_window: Mailbox<Notice>,
    cancelled: Arc<AtomicBool>,
    done: AtomicBool,
    budget: Budget,
    wake: Arc<OnceLock<Wake>>,
}

impl Connection {
    fn wake_window(&self) {
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.to_worker.close();
        self.to_window.close();
    }

    fn report_error(&self, message: &str) {
        let message: String = message.chars().take(512).collect();
        if let Ok(charge) = self.budget.reserve(message.capacity() + 128)
            && self.to_window.send(Envelope(Notice::Error(message), charge))
        {
            self.wake_window();
        }
    }

    fn finish(&self) {
        self.to_worker.close();
        self.done.store(true, Ordering::Release);
        self.wake_window();
    }

    fn ready(&self) -> bool {
        self.done.load(Ordering::Acquire) || self.to_window.has_messages()
    }
}

struct ThreadPermit;

impl ThreadPermit {
    fn acquire() -> Option<Self> {
        ACTIVE_WORKERS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_PROCESS_WORKERS).then_some(count + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ThreadPermit {
    fn drop(&mut self) {
        ACTIVE_WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

struct WorkerObject(u64);

struct ActiveWorker {
    object: RootId,
    connection: Arc<Connection>,
}

#[derive(Default)]
struct Store {
    next_id: u64,
    last_dispatch: u64,
    active: BTreeMap<u64, ActiveWorker>,
    budget: Budget,
    wake: Arc<OnceLock<Wake>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        for worker in self.active.values() {
            worker.connection.stop();
        }
    }
}

#[derive(Clone)]
pub(super) struct Workers {
    store: Rc<RefCell<Store>>,
    document: Weak<RefCell<State>>,
    events: event_target::Events,
    messages: MessagePorts,
    policy: Rc<Cell<BlobWorkerPolicy>>,
    exception: RootId,
}

struct Source {
    bytes: Arc<Vec<u8>>,
    range: Range<usize>,
}

fn exception(vm: &mut Vm, class: RootId, name: &str, message: &str) -> JsError {
    let class = vm.root_value(class).expect("rooted DOMException");
    dom_exception_error(vm, class, name, message).unwrap_or_else(|error| error)
}

fn clone_error(vm: &mut Vm, class: RootId, error: StructuredCloneError) -> JsError {
    match error {
        StructuredCloneError::DataClone(message) => exception(vm, class, "DataCloneError", message),
        StructuredCloneError::JavaScript(error) => error,
    }
}

fn post<T>(
    vm: &mut Vm,
    args: &[Value],
    budget: Option<&Budget>,
    class: RootId,
    queue: Option<&Mailbox<T>>,
    notice: impl FnOnce(StructuredClonePacket) -> T,
) -> Result<bool, JsError> {
    webidl::required(args, 1, "Worker.postMessage")?;
    let transfer = structured_clone::transfer_argument(vm, arg(args, 1))?;
    let (packet, reservation) = vm
        .serialize_clone_packet_with_transfer(arg(args, 0), &transfer, MAX_PACKET_BYTES, |bytes| {
            budget.map(|budget| budget.reserve(bytes)).transpose()
        })
        .map_err(|error| clone_error(vm, class, error))?;
    Ok(queue.zip(reservation).is_some_and(|(queue, reservation)| {
        queue.send(Envelope(notice(packet), reservation))
    }))
}

fn run_worker(
    connection: &Arc<Connection>,
    name: Vec<u16>,
    address: Url,
    source: Source,
) -> Result<(), JsError> {
    if connection.cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut vm = Vm::new();
    vm.set_interrupt_flag(connection.cancelled.clone());
    let parent = connection.clone();
    let closing = Rc::new(Cell::new(false));
    let close_state = closing.clone();
    let exception = Rc::new(Cell::new(None));
    let send_exception = exception.clone();
    let mut worker = WorkerRealm::new(
        vm,
        JsString::from_utf16(name),
        &address,
        move |vm, args| {
            if post(
                vm,
                args,
                Some(&parent.budget),
                send_exception.get().expect("worker DOMException installed"),
                Some(&parent.to_window),
                Notice::Message,
            )? {
                parent.wake_window();
            }
            Ok(())
        },
        move || close_state.set(true),
    )?;
    let vm = worker.vm_mut();
    let class = vm.get_global("DOMException")?;
    exception.set(Some(vm.root(class)));
    let source = String::from_utf8_lossy(&source.bytes[source.range]);
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    if let Err(error) = worker.evaluate(source, address.as_str()) {
        if fatal(&error) {
            return Err(error);
        }
        connection.report_error(&error.to_string());
    }
    while !closing.get() && !connection.cancelled.load(Ordering::Relaxed) {
        let Some(Envelope(message, _charge)) = connection.to_worker.receive() else {
            break;
        };
        let result = worker.dispatch_packet(message);
        if let Err(error) = result
            && !connection.cancelled.load(Ordering::Relaxed)
        {
            connection.report_error(&error.to_string());
        }
    }
    Ok(())
}

impl Workers {
    pub(super) fn restrict_policy(&self, policy: BlobWorkerPolicy) {
        if !policy.allows_blob() {
            self.policy.set(policy);
        }
    }

    pub(super) fn set_wake(&self, wake: Wake) {
        let _ = self.store.borrow().wake.set(wake);
    }

    pub(super) fn has_tasks(&self) -> bool {
        self.store
            .borrow()
            .active
            .values()
            .any(|worker| worker.connection.ready())
    }

    pub(super) fn has_workers(&self) -> bool {
        !self.store.borrow().active.is_empty()
    }

    fn terminate(&self, vm: &mut Vm, id: u64) {
        if let Some(worker) = self.store.borrow_mut().active.remove(&id) {
            worker.connection.stop();
            vm.unroot(worker.object);
        }
    }

    fn start(
        &self,
        vm: &mut Vm,
        object: Value,
        address: Url,
        name: Vec<u16>,
        source: Result<Source, &'static str>,
    ) -> Result<(), JsError> {
        let (id, connection) = {
            let mut state = self.store.borrow_mut();
            if state.active.len() == MAX_DOCUMENT_WORKERS {
                return Err(JsError::new(
                    ErrorKind::HeapLimit,
                    "Document worker budget exhausted",
                ));
            }
            state.next_id += 1;
            let id = state.next_id;
            let connection = Arc::new(Connection {
                to_worker: Mailbox::default(),
                to_window: Mailbox::default(),
                cancelled: Arc::new(AtomicBool::new(false)),
                done: AtomicBool::new(false),
                budget: state.budget.clone(),
                wake: state.wake.clone(),
            });
            vm.set_host_data(object.clone(), Rc::new(WorkerObject(id)))?;
            state.active.insert(
                id,
                ActiveWorker {
                    object: vm.root(object),
                    connection: connection.clone(),
                },
            );
            (id, connection)
        };
        match (source, ThreadPermit::acquire()) {
            (Ok(source), Some(permit)) => {
                let worker = connection.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name(format!("satin-worker-{id}"))
                    .spawn(move || {
                        let _permit = permit;
                        if let Err(error) = run_worker(&worker, name, address, source)
                            && !worker.cancelled.load(Ordering::Relaxed)
                        {
                            worker.report_error(&error.to_string());
                        }
                        worker.finish();
                    })
                {
                    connection.report_error(&format!("Could not start worker: {error}"));
                    connection.finish();
                }
            }
            (Err(error), _) => {
                connection.report_error(error);
                connection.finish();
            }
            (Ok(_), None) => {
                connection.report_error("Process worker budget exhausted");
                connection.finish();
            }
        }
        Ok(())
    }

    fn construct(
        &self,
        vm: &mut Vm,
        object: Value,
        args: &[Value],
    ) -> Result<Value, JsError> {
        webidl::required(args, 1, "Worker")?;
        let raw = webidl::usv_string(vm, &arg(args, 0), 256 * 1024)?;
        let options = arg(args, 1);
        if !options.is_nullish() && options.as_object().is_none() {
            return Err(JsError::type_error("Worker options must be a dictionary"));
        }
        let credentials = event_target::option(vm, &options, "credentials")?;
        if !credentials.is_undefined()
            && !matches!(
                vm.to_string(&credentials)?.to_string_lossy().as_str(),
                "omit" | "same-origin" | "include"
            )
        {
            return Err(JsError::type_error("Invalid Worker credentials"));
        }
        let name = event_target::option(vm, &options, "name")?;
        let name = if name.is_undefined() {
            Vec::new()
        } else {
            let name = vm.to_string(&name)?;
            vm.charge_work(name.len())?;
            name.as_utf16().to_vec()
        };
        let kind = event_target::option(vm, &options, "type")?;
        let classic = if kind.is_undefined() {
            true
        } else {
            match vm.to_string(&kind)?.to_string_lossy().as_str() {
                "classic" => true,
                "module" => false,
                _ => return Err(JsError::type_error("Invalid Worker type")),
            }
        };
        let document = self
            .document
            .upgrade()
            .ok_or_else(|| JsError::type_error("Worker has no active document"))?;
        let address = document_base(&document.borrow())
            .join(&raw)
            .map_err(|_| exception(vm, self.exception, "SyntaxError", "Invalid Worker URL"))?;
        // Parser-written metadata takes effect even for the next nested
        // script, before the document scheduler has drained parser results.
        let parser = document.borrow().parser.upgrade();
        let policy_allows = self.policy.get().allows_blob()
            && parser.is_none_or(|parser| parser.borrow().worker_policy().allows_blob());
        let source = if address.scheme() != "blob" || !classic {
            Err("This Satin worker requires a classic Blob source")
        } else if !policy_allows {
            Err("Content Security Policy blocked the Blob worker")
        } else {
            document
                .borrow()
                .media
                .resolve_blob(vm, &address)
                .ok_or("Worker Blob URL is unavailable in this document")
                .and_then(|blob| {
                    let (bytes, range) = blob.shared_bytes();
                    if range.len() > MAX_SOURCE_BYTES {
                        return Err("Worker script exceeds the source size limit");
                    }
                    Ok(Source { bytes, range })
                })
        };
        vm.mark_platform_object(&object)?;
        event_target::attach(vm, object.clone())?;
        self.start(vm, object.clone(), address, name, source)?;
        Ok(object)
    }

    pub(super) fn dispatch_next(&self, vm: &mut Vm) -> Result<bool, JsError> {
        let (id, root, connection) = {
            let mut store = self.store.borrow_mut();
            let selected = store
                .active
                .range((std::ops::Bound::Excluded(store.last_dispatch), std::ops::Bound::Unbounded))
                .chain(store.active.iter())
                .find(|(_, worker)| worker.connection.ready());
            let Some((&id, worker)) = selected else {
                return Ok(false);
            };
            let root = worker.object;
            let connection = worker.connection.clone();
            store.last_dispatch = id;
            (id, root, connection)
        };
        if let Some(Envelope(notice, _charge)) = connection.to_window.try_receive() {
            let object = vm.root_value(root).expect("rooted active Worker");
            match notice {
                Notice::Message(packet) => {
                    let event = match vm.deserialize_clone_packet(packet) {
                        Ok(data) => self.messages.received_from_peer(vm, "message", data)?,
                        Err(_) => self.messages.received_from_peer(vm, "messageerror", Value::Null)?,
                    };
                    self.events.dispatch(vm, object, event)?;
                }
                Notice::Error(message) => {
                    let event = vm.new_object_with_prototype(self.events.event_proto.clone())?;
                    let event = event_target::init_event(vm, event, "error".into(), [false, true, false], true)?;
                    vm.set_property(event.clone(), "message", Value::from(message.as_str()))?;
                    if self.events.dispatch(vm, object, event)?
                        && let Some(document) = self.document.upgrade()
                    {
                        diagnostic(&document, format!("Worker: {message}"));
                    }
                }
            }
        } else if connection.done.load(Ordering::Acquire) {
            self.terminate(vm, id);
        }
        Ok(true)
    }
}

fn brand(vm: &Vm, object: &Value) -> Result<Rc<WorkerObject>, JsError> {
    vm.host_data(object)
        .ok_or_else(|| JsError::type_error("Illegal Worker receiver"))
}

pub(super) fn install(
    vm: &mut Vm,
    document: &Shared,
    events: event_target::Events,
    messages: MessagePorts,
    policy: BlobWorkerPolicy,
) -> Result<Workers, JsError> {
    let prototype = vm.new_object_with_prototype(events.target_proto.clone())?;
    let exception = vm.get_global("DOMException")?;
    let workers = Workers {
        store: Rc::new(RefCell::new(Store::default())),
        document: Rc::downgrade(document),
        events,
        messages,
        policy: Rc::new(Cell::new(policy)),
        exception: vm.root(exception),
    };
    let constructor = workers.clone();
    let intrinsic = prototype.clone();
    let worker = webidl::interface(vm, prototype.clone(), "Worker", 1, move |vm, _, args, target| {
        let target = target.ok_or_else(|| JsError::type_error("Worker requires new"))?;
        let object = webidl::instance(vm, target, &intrinsic)?;
        constructor.construct(vm, object, args)
    })?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    let event_target = vm.get_global("EventTarget")?;
    vm.call(set_prototype, Value::Undefined, &[worker, event_target])?;
    let messaging = workers.clone();
    webidl::operation(vm, prototype.clone(), "postMessage", 1, move |vm, this, args| {
        let id = brand(vm, &this)?.0;
        let link = messaging.store.borrow().active.get(&id).map(|entry| entry.connection.clone());
        post(
            vm,
            args,
            link.as_ref().map(|link| &link.budget),
            messaging.exception,
            link.as_ref().map(|link| &link.to_worker),
            |packet| packet,
        )?;
        Ok(Value::Undefined)
    })?;
    let termination = workers.clone();
    webidl::operation(vm, prototype.clone(), "terminate", 0, move |vm, this, _| {
        termination.terminate(vm, brand(vm, &this)?.0);
        Ok(Value::Undefined)
    })?;
    for kind in ["message", "messageerror", "error"] {
        event_target::install_handler(
            vm,
            prototype.clone(),
            kind,
            |vm, this| brand(vm, this).map(|_| ()),
            |_, _| Ok(()),
        )?;
    }
    Ok(workers)
}
