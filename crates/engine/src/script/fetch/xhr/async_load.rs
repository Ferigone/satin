//! Async XHR uses the same job IDs, transport pipe, and realm tasks as Fetch.
use super::{
    Host, Ready, Request, TEXT_SLOT, content_length, receiver, response_size, response_text,
};
use crate::network::{
    CancelSignal, FETCH_HEADER_LIMIT, FETCH_RESPONSE_BODY_LIMIT, FetchBody, FetchError, FetchJob,
    StreamingResponse, WebRequest,
};
use rust_browser_javascript::{JsError, RootId, Value, Vm};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};

pub(in crate::script::fetch) struct Pending {
    root: RootId,
    cancel: CancelSignal,
    pipe: Option<FetchBody>,
    buffered: Option<Arc<Vec<u8>>>,
    scheduled: Option<Task>,
    author_timeout: AuthorTimeout,
    received: Vec<u8>,
    // Only a primitive string is cached; it never holds a VM object outside the GC.
    cached_text: Option<Value>,
    pub(in crate::script::fetch) request_size: usize,
    last_progress: Option<Instant>,
}

impl Pending {
    fn body_capacity(&self) -> usize {
        self.received.capacity() + self.buffered.as_ref().map_or(0, |bytes| bytes.capacity())
    }
}

#[derive(Clone, Copy)]
enum Task {
    Read,
    End,
    Error,
    Timeout,
}

#[derive(Default)]
struct AuthorTimeout {
    started: Option<Instant>,
    deadline: Option<Instant>,
}

impl AuthorTimeout {
    fn set(&mut self, millis: u32) {
        self.deadline = self
            .started
            .filter(|_| millis != 0)
            .and_then(|start| start.checked_add(Duration::from_millis(u64::from(millis))));
    }

    fn stop(&mut self) {
        self.started = None;
        self.deadline = None;
    }
}

// Async XHR retains whole bodies until the final progress and load events. Bound
// all such allocations per document independently of Fetch request/upload data.
const LIVE_XHR_BODY_LIMIT: usize = 2 * FETCH_RESPONSE_BODY_LIMIT;

impl Host {
    fn can_grow_bodies(&self, extra: usize) -> bool {
        let bodies = self.fetch.borrow();
        let used: usize = bodies
            .pending_xhr
            .values()
            .map(Pending::body_capacity)
            .sum();
        used.saturating_add(extra) <= LIVE_XHR_BODY_LIMIT
    }

    pub(super) fn queue(
        &self,
        vm: &mut Vm,
        xhr: Value,
        data: &Rc<RefCell<Request>>,
        mut request: WebRequest,
    ) -> Result<(), JsError> {
        let request_size = super::super::bytes(&request.headers)
            + request.url.as_str().len()
            + request.client_url.as_str().len()
            + request.method.len()
            + 512
            + request.body.as_ref().map_or(0, |body| body.capacity());
        super::super::budget(vm, &self.fetch, request_size)?;
        if self.fetch.borrow().pending_requests() >= super::super::PENDING {
            return Err(self.exception(vm, "NetworkError", "Fetch pending request limit reached"));
        }
        let id = {
            let mut host = self.fetch.borrow_mut();
            let id = host.next_id()?;
            let cancel = CancelSignal::new();
            request.signal = Some(cancel.clone());
            host.pending_xhr.insert(
                id,
                Pending {
                    root: vm.root(xhr.clone()),
                    cancel,
                    pipe: None,
                    buffered: None,
                    scheduled: None,
                    author_timeout: Default::default(),
                    received: Vec::new(),
                    request_size,
                    cached_text: None,
                    last_progress: None,
                },
            );
            host.ready.push(FetchJob {
                id,
                request,
                upload: None,
            });
            let mut data = data.borrow_mut();
            data.request = None;
            data.pending = Some(id);
            id
        };
        let result = self.fire_progress(vm, xhr, "loadstart", 0, 0);
        if result.is_ok()
            && let Some(pending) = self.fetch.borrow_mut().pending_xhr.get_mut(&id)
        {
            // loadstart is synchronous and can change timeout or replace this
            // request. Measure the remaining request's fetch from here.
            pending.author_timeout.started = Some(self.now());
            pending.author_timeout.set(data.borrow().timeout_ms);
        }
        self.cleanup_failed_delivery(vm, id, result)
    }

    fn take(&self, id: u64, cancel: bool) -> Option<Pending> {
        let mut host = self.fetch.borrow_mut();
        let pending = host.pending_xhr.remove(&id)?;
        if cancel {
            host.ready.retain(|job| job.id != id);
            host.ready_xhr.retain(|queued| *queued != id);
            pending.cancel.cancel();
        }
        Some(pending)
    }

    pub(super) fn discard(&self, vm: &mut Vm, id: u64) {
        if let Some(pending) = self.take(id, true) {
            vm.unroot(pending.root);
        }
    }

    fn target(&self, vm: &Vm, id: u64) -> Option<Value> {
        let host = self.fetch.borrow();
        vm.root_value(host.pending_xhr.get(&id)?.root)
    }

    fn schedule(&self, id: u64, task: Task) {
        let mut host = self.fetch.borrow_mut();
        if let Some(pending) = host.pending_xhr.get_mut(&id)
            && pending.scheduled.is_none()
        {
            // One queue entry per bounded pending request.
            pending.scheduled = Some(task);
            host.ready_xhr.push_back(id);
        }
    }

    pub(super) fn update_timeout(&self, id: u64, millis: u32, now: Instant) {
        // In particular, zero or extending the timeout cannot undo expiry
        // that occurred before this setter ran. Neither path runs JS here.
        if self.latch_timeout(id, now) {
            return;
        }
        if let Some(pending) = self.fetch.borrow_mut().pending_xhr.get_mut(&id) {
            pending.author_timeout.set(millis);
        }
        self.latch_timeout(id, now);
    }

    fn latch_timeout(&self, id: u64, now: Instant) -> bool {
        let mut host = self.fetch.borrow_mut();
        let Some(pending) = host.pending_xhr.get_mut(&id) else {
            return false;
        };
        if matches!(pending.scheduled, Some(Task::Timeout)) {
            return true;
        }
        if pending
            .author_timeout
            .deadline
            .is_none_or(|deadline| deadline > now)
        {
            return false;
        }
        pending.author_timeout.stop();
        pending.cancel.cancel();
        pending.scheduled = Some(Task::Timeout);
        host.ready.retain(|job| job.id != id);
        host.ready_xhr.retain(|queued| *queued != id);
        host.ready_xhr.push_back(id);
        true
    }

    pub(in crate::script::fetch) fn next_timeout(&self) -> Option<Instant> {
        self.fetch
            .borrow()
            .pending_xhr
            .values()
            .filter_map(|pending| pending.author_timeout.deadline)
            .min()
    }

    pub(in crate::script::fetch) fn has_due_timeout(&self) -> bool {
        self.next_timeout()
            .is_some_and(|deadline| deadline <= self.now())
    }

    pub(in crate::script::fetch) fn run_next_task(&self, vm: &mut Vm) -> Result<(), JsError> {
        let now = self.now();
        let mut due = self
            .fetch
            .borrow()
            .pending_xhr
            .iter()
            .filter_map(|(&id, pending)| {
                pending
                    .author_timeout
                    .deadline
                    .filter(|deadline| *deadline <= now)
                    .map(|deadline| (deadline, id))
            })
            .collect::<Vec<_>>();
        due.sort_unstable();
        for (_, id) in due {
            self.latch_timeout(id, now);
        }
        let (id, task) = {
            let mut host = self.fetch.borrow_mut();
            let Some(id) = host.ready_xhr.pop_front() else {
                return Ok(());
            };
            let Some(task) = host
                .pending_xhr
                .get_mut(&id)
                .and_then(|pending| pending.scheduled.take())
            else {
                return Ok(());
            };
            (id, task)
        };
        let result = match task {
            Task::Read => self.receive_body(vm, id),
            Task::End => self.finish(vm, id),
            Task::Error => self.fail(vm, id, "error"),
            Task::Timeout => self.fail(vm, id, "timeout"),
        };
        self.cleanup_failed_delivery(vm, id, result)
    }

    fn cleanup_failed_delivery(
        &self,
        vm: &mut Vm,
        id: u64,
        result: Result<(), JsError>,
    ) -> Result<(), JsError> {
        if result.is_err()
            && let Some(pending) = self.take(id, true)
        {
            if let Some(xhr) = vm.root_value(pending.root)
                && let Ok(data) = receiver(vm, &xhr)
            {
                let mut data = data.borrow_mut();
                data.pending = None;
                data.ready = Ready::Done;
                data.response = None;
            }
            vm.unroot(pending.root);
        }
        result
    }

    pub(in crate::script::fetch) fn cancel_all(&self, vm: &mut Vm) -> Result<(), JsError> {
        let ids: Vec<_> = self.fetch.borrow().pending_xhr.keys().copied().collect();
        let mut failure = None;
        for id in ids {
            if let Err(error) = self.fail(vm, id, "abort") {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn fail(&self, vm: &mut Vm, id: u64, kind: &str) -> Result<(), JsError> {
        let Some(pending) = self.take(id, true) else {
            return Ok(());
        };
        let result = (|| {
            let xhr = vm.root_value(pending.root).expect("rooted XMLHttpRequest");
            {
                let data = receiver(vm, &xhr)?;
                let mut data = data.borrow_mut();
                data.pending = None;
                data.ready = Ready::Done;
                data.response = None;
            }
            self.fire(vm, xhr.clone(), "readystatechange")?;
            self.fire_progress(vm, xhr.clone(), kind, 0, 0)?;
            self.fire_progress(vm, xhr, "loadend", 0, 0)
        })();
        vm.unroot(pending.root);
        result
    }

    pub(in crate::script::fetch) fn complete(
        &self,
        vm: &mut Vm,
        id: u64,
        result: Result<StreamingResponse, FetchError>,
    ) -> Result<(), JsError> {
        if self.latch_timeout(id, self.now()) {
            return Ok(());
        }
        let result = self.receive_head(vm, id, result);
        self.cleanup_failed_delivery(vm, id, result)
    }

    fn receive_head(
        &self,
        vm: &mut Vm,
        id: u64,
        result: Result<StreamingResponse, FetchError>,
    ) -> Result<(), JsError> {
        let Some(xhr) = self.target(vm, id) else {
            return Ok(());
        };
        let data = receiver(vm, &xhr)?;
        if data.borrow().ready != Ready::Opened {
            return Ok(());
        }
        let Ok(StreamingResponse {
            mut metadata,
            transfer,
        }) = result
        else {
            return self.fail(vm, id, "error");
        };
        if metadata.status == 0
            || metadata.url.len() > FETCH_HEADER_LIMIT
            || metadata.status_text.len() > FETCH_HEADER_LIMIT
            || super::super::bytes(&metadata.headers) > FETCH_HEADER_LIMIT * 2
        {
            return self.fail(vm, id, "error");
        }
        metadata.headers.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("set-cookie") && !name.eq_ignore_ascii_case("set-cookie2")
        });
        self.budget(vm, None, response_size(&metadata))?;
        let body = metadata.body.take();
        {
            let mut data = data.borrow_mut();
            data.ready = Ready::HeadersReceived;
            data.response = Some(metadata);
        }
        self.fire(vm, xhr, "readystatechange")?;
        if data.borrow().pending != Some(id) || self.latch_timeout(id, self.now()) {
            // A handler can abort, replace or time out this request.
            return Ok(());
        }
        if let Some(pipe) = transfer {
            self.fetch
                .borrow_mut()
                .pending_xhr
                .get_mut(&id)
                .unwrap()
                .pipe = Some(pipe);
            // The producer posts a read notification even if the entire
            // response has already arrived. Do not pull its body in this
            // headers task: that would run chunk/EOF before its checkpoint.
            return Ok(());
        }
        if let Some(body) = body {
            if body.len() > FETCH_RESPONSE_BODY_LIMIT || !self.can_grow_bodies(body.capacity()) {
                self.schedule(id, Task::Error);
            } else {
                self.fetch
                    .borrow_mut()
                    .pending_xhr
                    .get_mut(&id)
                    .unwrap()
                    .buffered = Some(body);
                self.schedule(id, Task::Read);
            }
            return Ok(());
        }
        // XHR handles a null body inline, unlike the close of a body stream.
        self.finish(vm, id)
    }

    pub(in crate::script::fetch) fn progress(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        if self.latch_timeout(id, self.now()) {
            return Ok(());
        }
        if self
            .fetch
            .borrow()
            .pending_xhr
            .get(&id)
            .is_some_and(|pending| pending.scheduled.is_some())
        {
            // A queued continuation already owns this read. It will
            // acknowledge the pipe and observe any newly arrived data.
            return Ok(());
        }
        let result = self.receive_body(vm, id);
        self.cleanup_failed_delivery(vm, id, result)
    }

    fn receive_body(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let buffered = self
            .fetch
            .borrow_mut()
            .pending_xhr
            .get_mut(&id)
            .and_then(|pending| pending.buffered.take());
        if let Some(body) = buffered {
            if body.is_empty() {
                return self.finish(vm, id);
            }
            let accepted = match Arc::try_unwrap(body) {
                Ok(bytes) => self.append_owned(vm, id, bytes)?,
                Err(shared) => self.append(vm, id, &shared)?,
            };
            if !accepted {
                return self.fail(vm, id, "error");
            }
            self.notify_chunk(vm, id)?;
            self.schedule(id, Task::End);
            return Ok(());
        }
        let result = {
            let mut host = self.fetch.borrow_mut();
            let Some(pipe) = host
                .pending_xhr
                .get_mut(&id)
                .and_then(|pending| pending.pipe.as_mut())
            else {
                return Ok(());
            };
            pipe.acknowledge();
            pipe.read()
        };
        match result {
            Poll::Ready(Ok(Some(bytes))) => {
                if !self.append_owned(vm, id, bytes)? {
                    return self.fail(vm, id, "error");
                }
                self.notify_chunk(vm, id)?;
                // A notification can represent more than one chunk or have
                // EOF already visible in the pipe. Continue in a separate
                // task only if work is already ready. We acknowledged the
                // pipe before reading, so a later arrival can notify us.
                let ready = self
                    .fetch
                    .borrow()
                    .pending_xhr
                    .get(&id)
                    .and_then(|pending| pending.pipe.as_ref())
                    .is_some_and(FetchBody::has_ready_read);
                if ready {
                    self.schedule(id, Task::Read);
                }
                Ok(())
            }
            Poll::Ready(Ok(None)) => self.finish(vm, id),
            Poll::Ready(Err(_)) => self.fail(vm, id, "error"),
            Poll::Pending => Ok(()),
        }
    }

    fn append_owned(&self, vm: &mut Vm, id: u64, bytes: Vec<u8>) -> Result<bool, JsError> {
        if bytes.capacity() <= FETCH_RESPONSE_BODY_LIMIT
            && self.fetch.borrow().pending_xhr[&id].received.capacity() == 0
        {
            // Keep a uniquely owned first chunk instead of copying it into a
            // second allocation. Count its capacity against the document's XHR budget.
            if !self.can_grow_bodies(bytes.capacity()) {
                return Ok(false);
            }
            let mut host = self.fetch.borrow_mut();
            let pending = host.pending_xhr.get_mut(&id).unwrap();
            pending.cached_text = None;
            pending.received = bytes;
            Ok(true)
        } else {
            self.append(vm, id, &bytes)
        }
    }

    fn append(&self, vm: &mut Vm, id: u64, bytes: &[u8]) -> Result<bool, JsError> {
        let (new_len, capacity) = {
            let host = self.fetch.borrow();
            let received = &host.pending_xhr[&id].received;
            (
                received.len().saturating_add(bytes.len()),
                received.capacity(),
            )
        };
        if new_len > FETCH_RESPONSE_BODY_LIMIT {
            return Ok(false);
        }
        vm.charge_buffer_work(bytes.len())?;
        let mut growth = 0;
        if new_len > capacity {
            // Stay within both the per-response cap and the realm's aggregate budget.
            growth = capacity
                .max(32 * 1024)
                .min(FETCH_RESPONSE_BODY_LIMIT - capacity)
                .max(new_len - capacity);
            if !self.can_grow_bodies(growth) {
                return Ok(false);
            }
        }
        let mut host = self.fetch.borrow_mut();
        let pending = host.pending_xhr.get_mut(&id).unwrap();
        pending.cached_text = None;
        let received = &mut pending.received;
        if growth > 0 {
            received.reserve_exact(capacity + growth - received.len());
        }
        received.extend_from_slice(bytes);
        Ok(true)
    }

    fn notify_chunk(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let Some(xhr) = self.target(vm, id) else {
            return Ok(());
        };
        let data = receiver(vm, &xhr)?;
        let loaded = {
            let mut host = self.fetch.borrow_mut();
            let pending = host.pending_xhr.get_mut(&id).unwrap();
            let now = self.now();
            if pending
                .last_progress
                .is_some_and(|last| now - last < Duration::from_millis(50))
            {
                return Ok(());
            }
            pending.last_progress = Some(now);
            pending.received.len()
        };
        let total = {
            let mut data = data.borrow_mut();
            data.ready = Ready::Loading;
            data.response.as_ref().map_or(0, content_length)
        };
        self.fire(vm, xhr.clone(), "readystatechange")?;
        if data.borrow().pending == Some(id) && !self.latch_timeout(id, self.now()) {
            self.fire_progress(vm, xhr, "progress", loaded, total)?;
        }
        Ok(())
    }

    fn finish(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let Some(xhr) = self.target(vm, id) else {
            return Ok(());
        };
        let data = receiver(vm, &xhr)?;
        let loaded = {
            let mut host = self.fetch.borrow_mut();
            let pending = host
                .pending_xhr
                .get_mut(&id)
                .expect("completed XMLHttpRequest");
            // This is the accepted terminal fetch task. Neither text
            // decoding nor an author callback here is part of fetch time.
            pending.author_timeout.stop();
            pending.received.len()
        };
        let total = data.borrow().response.as_ref().map_or(0, content_length);
        self.fire_progress(vm, xhr.clone(), "progress", loaded, total)?;
        if data.borrow().pending != Some(id) {
            return Ok(());
        }
        let text = {
            let data = data.borrow();
            self.partial_text(vm, &data)?
        };
        vm.set_host_slot(xhr.clone(), TEXT_SLOT, text)?;
        let pending = self.take(id, false).expect("completed XMLHttpRequest");
        let result = (|| {
            let xml = {
                let mut data = data.borrow_mut();
                data.pending = None;
                data.ready = Ready::Done;
                data.response
                    .as_ref()
                    .and_then(|response| self.xml_source(response, Arc::new(pending.received)))
            };
            if let Some(xml) = xml {
                self.budget(vm, None, xml.size())?;
                data.borrow_mut().xml = Some(xml);
            }
            self.success_events(vm, xhr, loaded, total)
        })();
        vm.unroot(pending.root);
        result
    }

    pub(super) fn partial_text(&self, vm: &mut Vm, data: &Request) -> Result<Value, JsError> {
        if let Some(id) = data.pending
            && let Some(response) = &data.response
            && let Some(pending) = self.fetch.borrow_mut().pending_xhr.get_mut(&id)
        {
            if let Some(cached) = &pending.cached_text {
                return Ok(cached.clone());
            }
            let text = response_text(vm, response, &pending.received)?;
            pending.cached_text = Some(text.clone());
            return Ok(text);
        }
        Ok("".into())
    }
}
