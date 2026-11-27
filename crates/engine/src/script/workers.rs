//! Classic dedicated workers run in separate JavaScript VMs.
mod fetch;
mod mailbox;
mod realm;

use super::{
    Shared, State, abort::dom_exception_error, arg, diagnostic, document_base, event_target, fatal,
    message_ports::MessagePorts, structured_clone, webidl,
};
use crate::{
    loader::{ConnectPolicy, DocumentPolicy, LoadedPage},
    network::CancelSignal,
};
use fetch::Network;
use mailbox::{Budget, Envelope, MAX_PACKET_BYTES, Mailbox, Received, Reservation};
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
        Arc, OnceLock, Weak as ArcWeak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
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

type HostTask = Box<dyn FnOnce(&mut Vm) -> Result<(), JsError> + Send>;

enum WorkerTask {
    Message(StructuredClonePacket),
    Host(HostTask),
}

struct Connection {
    to_worker: Mailbox<WorkerTask>,
    to_window: Mailbox<Notice>,
    cancelled: Arc<AtomicBool>,
    lifecycle: CancelSignal,
    done: AtomicBool,
    budget: Budget,
    task_budget: Budget,
    wake: Arc<OnceLock<Wake>>,
}

impl Connection {
    fn new(budget: Budget, task_budget: Budget, wake: Arc<OnceLock<Wake>>) -> Self {
        Self {
            to_worker: Mailbox::default(),
            to_window: Mailbox::default(),
            cancelled: Arc::new(AtomicBool::new(false)),
            lifecycle: CancelSignal::new(),
            done: AtomicBool::new(false),
            budget,
            task_budget,
            wake,
        }
    }

    fn wake_window(&self) {
        if !self.cancelled.load(Ordering::Relaxed)
            && let Some(wake) = self.wake.get()
        {
            wake();
        }
    }

    fn close_ingress(&self) {
        self.to_worker.close();
        self.lifecycle.cancel();
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.to_window.close();
        self.close_ingress();
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
        self.close_ingress();
        self.done.store(true, Ordering::Release);
        self.wake_window();
    }

    fn ready(&self) -> bool {
        self.done.load(Ordering::Acquire) || self.to_window.has_messages()
    }
}

/// A worker-side host API reserves a bounded result slot before starting async
/// work. Neither the sender nor an in-flight permit keeps the worker alive.
#[allow(
    dead_code,
    reason = "Integration point for the separate Worker Fetch host"
)]
#[derive(Clone)]
pub(super) struct WorkerTaskSender {
    connection: ArcWeak<Connection>,
    lifecycle: CancelSignal,
}

#[allow(
    dead_code,
    reason = "Integration point for the separate Worker Fetch host"
)]
impl WorkerTaskSender {
    fn new(connection: &Arc<Connection>) -> Self {
        Self {
            connection: Arc::downgrade(connection),
            lifecycle: connection.lifecycle.clone(),
        }
    }

    /// Give each request a child token so aborting one does not affect others.
    pub(super) fn cancellation(&self) -> CancelSignal {
        self.lifecycle.child_token()
    }

    /// `bytes` must cover the maximum owned Rust payload for this completion.
    /// `Ok(None)` means the worker is closed; no new work should be started.
    pub(super) fn reserve(&self, bytes: usize) -> Result<Option<WorkerTaskPermit>, JsError> {
        let Some(connection) = self
            .connection
            .upgrade()
            .filter(|_| !self.lifecycle.is_cancelled())
        else {
            return Ok(None);
        };
        Ok(Some(WorkerTaskPermit {
            connection: self.connection.clone(),
            reservation: connection.task_budget.reserve(bytes)?,
        }))
    }
}

#[allow(
    dead_code,
    reason = "Integration point for the separate Worker Fetch host"
)]
pub(super) struct WorkerTaskPermit {
    connection: ArcWeak<Connection>,
    reservation: Reservation,
}

#[allow(
    dead_code,
    reason = "Integration point for the separate Worker Fetch host"
)]
impl WorkerTaskPermit {
    /// Carry owned Rust data and, if needed, IDs for roots registered by the
    /// worker. The callback runs only on that worker, as one event-loop task.
    pub(super) fn complete(
        self,
        task: impl FnOnce(&mut Vm) -> Result<(), JsError> + Send + 'static,
    ) -> bool {
        self.connection.upgrade().is_some_and(|connection| {
            connection
                .to_worker
                .send(Envelope(WorkerTask::Host(Box::new(task)), self.reservation))
        })
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
    task_budget: Budget,
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
    network: Network,
    meta_connect_policy: Rc<RefCell<ConnectPolicy>>,
    exception: RootId,
}

struct Source {
    bytes: Arc<Vec<u8>>,
    range: Range<usize>,
    policy: DocumentPolicy,
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
    network: Option<Network>,
) -> Result<(), JsError> {
    if connection.cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut vm = Vm::with_limits(super::web_vm_limits());
    vm.set_wasm_compilation_allowed(source.policy.allows_wasm_compilation());
    vm.set_interrupt_flag(connection.cancelled.clone());
    let parent = connection.clone();
    let ingress = connection.clone();
    let network = network.map(|network| {
        let connection_ref = Arc::downgrade(connection);
        let wake: Wake = Arc::new(move || {
            if let Some(connection) = connection_ref.upgrade() {
                connection.to_worker.notify_external();
            }
        });
        (network, WorkerTaskSender::new(connection).cancellation(), wake)
    });
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
        move || {
            close_state.set(true);
            // Do not interrupt the JS task that called close() or its
            // checkpoint. Drop all future worker tasks and abort host I/O.
            ingress.close_ingress();
        },
        network,
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
    let mut prefer_fetch = false;
    let mut ran_timer = false;
    while !closing.get() && !connection.cancelled.load(Ordering::Relaxed) {
        worker.dispatch_fetch_jobs();
        // A continually due timer must not indefinitely delay an already
        // published transport event. Give it one turn after a timer task.
        let input = if ran_timer && let Some(event) = worker.next_event() {
            Received::External(event)
        } else {
            connection.to_worker.receive_until_or(
                worker.timer_deadline(),
                || worker.next_event(),
                prefer_fetch,
            )
        };
        ran_timer = matches!(&input, Received::Deadline);
        let result = match input {
            Received::Message(Envelope(task, _charge)) => {
                prefer_fetch = true;
                match task {
                    WorkerTask::Message(packet) => worker.dispatch_packet(packet),
                    WorkerTask::Host(task) => worker.dispatch_host_task(task),
                }
            }
            Received::External(event) => {
                prefer_fetch = false;
                worker.dispatch_event(event)
            }
            Received::Deadline => worker.dispatch_timer(Instant::now()),
            Received::Closed => break,
        };
        if let Err(error) = result
            && !connection.cancelled.load(Ordering::Relaxed)
        {
            connection.report_error(&error.to_string());
        }
    }
    Ok(())
}

impl Workers {
    pub(super) fn set_meta_connect_policy(&self, policy: &ConnectPolicy) {
        *self.meta_connect_policy.borrow_mut() = policy.clone();
    }

    pub(super) fn set_wake(&self, wake: Wake) {
        let slot = self.store.borrow().wake.clone();
        // Parser scripts can start a worker before the embedder supplies its
        // wake callback. Replay a wake for any work already waiting.
        if slot.set(wake.clone()).is_ok() && self.has_tasks() {
            wake();
        }
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
        network: Network,
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
            let connection = Arc::new(Connection::new(
                state.budget.clone(),
                state.task_budget.clone(),
                state.wake.clone(),
            ));
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
                        if let Err(error) = run_worker(&worker, name, address, source, Some(network))
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
        // Snapshot the creator's effective policy when constructing the
        // worker, not when its thread is scheduled or the Blob is created.
        let policy = document.borrow().document_policy;
        // Parser-written metadata takes effect even for the next nested
        // script, before the document scheduler has drained parser results.
        let parser = document.borrow().parser.upgrade();
        let network = if let Some(parser) = parser {
            self.network.inherit_meta(parser.borrow().connect_policy())
        } else {
            self.network
                .inherit_meta(&self.meta_connect_policy.borrow())
        };
        let source = if address.scheme() != "blob" || !classic {
            Err("This Satin worker requires a classic Blob source")
        } else if !policy.allows_blob() {
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
                    Ok(Source { bytes, range, policy })
                })
        };
        vm.mark_platform_object(&object)?;
        event_target::attach(vm, object.clone())?;
        self.start(vm, object.clone(), address, name, source, network)?;
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

#[cfg(test)]
mod worker_lifecycle_tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    fn connection() -> Arc<Connection> {
        Arc::new(Connection::new(
            Budget::default(),
            Budget::default(),
            Arc::default(),
        ))
    }

    #[test]
    fn a_host_completion_wakes_the_worker_and_runs_its_checkpoint_before_close() {
        let connection = connection();
        let sender = WorkerTaskSender::new(&connection);
        let cancel = sender.cancellation();
        let (notify, wakes) = mpsc::channel();
        assert!(
            connection
                .wake
                .set(Arc::new(move || {
                    let _ = notify.send(());
                }))
                .is_ok()
        );
        let source = b"var log = ['script']; setTimeout(() => postMessage('late'), 60000); postMessage('ready');";
        let source = Source {
            bytes: Arc::new(source.to_vec()),
            range: 0..source.len(),
            policy: DocumentPolicy::default(),
        };
        let (done, finished) = mpsc::channel();
        let worker = connection.clone();
        let thread = thread::spawn(move || {
            let result = run_worker(
                &worker,
                vec![],
                Url::parse("blob:https://example.test/host-task").unwrap(),
                source,
                None,
            )
            .map_err(|error| error.to_string());
            worker.finish();
            done.send(result).unwrap();
        });

        let mut receiver = Vm::new();
        wakes.recv_timeout(Duration::from_secs(5)).unwrap();
        let Some(Envelope(Notice::Message(packet), _)) = connection.to_window.try_receive() else {
            panic!("worker did not start");
        };
        assert_eq!(
            receiver.deserialize_clone_packet(packet).unwrap(),
            Value::from("ready")
        );
        let owner = thread.thread().id();
        let first = sender.reserve(1024).unwrap().unwrap();
        let second = sender.reserve(1024).unwrap().unwrap();
        assert!(first.complete(move |vm| {
            assert_eq!(std::thread::current().id(), owner);
            vm.eval("log.push('first'); queueMicrotask(() => log.push('checkpoint'));")?;
            Ok(())
        }));
        assert!(second.complete(|vm| {
            vm.eval("close(); postMessage(log.join(','));")?;
            Ok(())
        }));

        finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        assert!(cancel.is_cancelled());
        let Some(Envelope(Notice::Message(packet), _)) = connection.to_window.try_receive() else {
            panic!("worker did not post the host task transcript");
        };
        assert_eq!(
            receiver.deserialize_clone_packet(packet).unwrap(),
            Value::from("script,first,checkpoint")
        );
        assert!(!connection.to_window.has_messages());
    }

    #[test]
    fn closed_workers_drop_queued_and_late_host_completions() {
        let connection = connection();
        let sender = WorkerTaskSender::new(&connection);
        let request_cancel = sender.cancellation();
        request_cancel.cancel();
        let other_request_cancel = sender.cancellation();
        assert!(!other_request_cancel.is_cancelled());

        let queued = sender.reserve(16 * 1024 * 1024).unwrap().unwrap();
        let late = sender.reserve(16 * 1024 * 1024).unwrap().unwrap();
        assert!(sender.reserve(1).is_err());
        assert!(queued.complete(|_| panic!("discarded completion ran")));
        connection.stop();
        assert!(other_request_cancel.is_cancelled());
        assert!(sender.reserve(1).unwrap().is_none());
        assert!(!late.complete(|_| panic!("late completion ran")));
        assert!(matches!(
            connection.to_worker.receive_until(None),
            Received::Closed
        ));
        assert!(connection.task_budget.reserve(32 * 1024 * 1024).is_ok());
    }
}

pub(super) fn install(
    vm: &mut Vm,
    document: &Shared,
    events: event_target::Events,
    messages: MessagePorts,
    page: &LoadedPage,
) -> Result<Workers, JsError> {
    let prototype = vm.new_object_with_prototype(events.target_proto.clone())?;
    let exception = vm.get_global("DOMException")?;
    let workers = Workers {
        store: Rc::new(RefCell::new(Store::default())),
        document: Rc::downgrade(document),
        events,
        messages,
        network: Network::from_page(page),
        meta_connect_policy: Rc::new(RefCell::new(page.meta_connect_policy.clone())),
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
            WorkerTask::Message,
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
