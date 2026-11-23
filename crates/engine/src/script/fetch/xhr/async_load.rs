//! Async XHR uses the same job IDs, transport pipe, and realm tasks as Fetch.
use super::{
    Host, Ready, Request, TEXT_SLOT, content_length, receiver, response_size, response_text,
};
use crate::network::{
    CancelSignal, FETCH_BODY_LIMIT, FETCH_HEADER_LIMIT, FetchBody, FetchError, FetchJob,
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
    received: Vec<u8>,
    request_size: usize,
    last_progress: Option<Instant>,
}

impl Pending {
    pub(in crate::script::fetch) fn size(&self) -> usize {
        self.request_size + self.received.capacity()
    }
}

impl Host {
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
                    received: Vec::new(),
                    request_size,
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
        self.cleanup_failed_delivery(vm, id, result)
    }

    fn take(&self, id: u64, cancel: bool) -> Option<Pending> {
        let mut host = self.fetch.borrow_mut();
        let pending = host.pending_xhr.remove(&id)?;
        if cancel {
            host.ready.retain(|job| job.id != id);
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
        if data.borrow().pending != Some(id) {
            // An event handler can abort or replace this request.
            return Ok(());
        }
        if let Some(pipe) = transfer {
            self.fetch
                .borrow_mut()
                .pending_xhr
                .get_mut(&id)
                .unwrap()
                .pipe = Some(pipe);
            return self.receive_body(vm, id);
        }
        if let Some(body) = body
            && !body.is_empty()
        {
            if !self.append(vm, id, &body)? {
                return self.fail(vm, id, "error");
            }
            self.notify_chunk(vm, id)?;
        }
        self.finish(vm, id)
    }

    pub(in crate::script::fetch) fn progress(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let result = self.receive_body(vm, id);
        self.cleanup_failed_delivery(vm, id, result)
    }

    fn receive_body(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        if let Some(pipe) = self
            .fetch
            .borrow()
            .pending_xhr
            .get(&id)
            .and_then(|p| p.pipe.as_ref())
        {
            pipe.acknowledge();
        } else {
            return Ok(());
        }
        let mut changed = false;
        let ended = loop {
            let result = self
                .fetch
                .borrow_mut()
                .pending_xhr
                .get_mut(&id)
                .unwrap()
                .pipe
                .as_mut()
                .unwrap()
                .read();
            match result {
                Poll::Ready(Ok(Some(bytes))) => {
                    if !self.append(vm, id, &bytes)? {
                        return self.fail(vm, id, "error");
                    }
                    changed = true;
                }
                Poll::Ready(Err(_)) => return self.fail(vm, id, "error"),
                Poll::Ready(Ok(None)) => break true,
                Poll::Pending => break false,
            }
        };
        if changed {
            self.notify_chunk(vm, id)?;
        }
        if ended {
            self.finish(vm, id)?;
        }
        Ok(())
    }

    fn append(&self, vm: &mut Vm, id: u64, bytes: &[u8]) -> Result<bool, JsError> {
        let (new_len, capacity) = {
            let host = self.fetch.borrow();
            let received = &host.pending_xhr[&id].received;
            (received.len() + bytes.len(), received.capacity())
        };
        if new_len > FETCH_BODY_LIMIT {
            return Ok(false);
        }
        vm.charge_work(bytes.len())?;
        let mut growth = 0;
        if new_len > capacity {
            // Stay within both the per-response cap and the realm's aggregate budget.
            growth = capacity
                .max(32 * 1024)
                .min(FETCH_BODY_LIMIT - capacity)
                .max(new_len - capacity);
            super::super::budget(vm, &self.fetch, growth)?;
        }
        let mut host = self.fetch.borrow_mut();
        let received = &mut host.pending_xhr.get_mut(&id).unwrap().received;
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
            let now = Instant::now();
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
        if data.borrow().pending == Some(id) {
            self.fire_progress(vm, xhr, "progress", loaded, total)?;
        }
        Ok(())
    }

    fn finish(&self, vm: &mut Vm, id: u64) -> Result<(), JsError> {
        let Some(xhr) = self.target(vm, id) else {
            return Ok(());
        };
        let data = receiver(vm, &xhr)?;
        let loaded = self.fetch.borrow().pending_xhr[&id].received.len();
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
            && let Some(pending) = self.fetch.borrow().pending_xhr.get(&id)
        {
            return response_text(vm, response, &pending.received);
        }
        Ok("".into())
    }
}
