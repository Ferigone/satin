//! A request body is locked once and read from a scheduled transport demand.
use super::{Shared, StreamHost, fatal};
use crate::network::{
    FETCH_BODY_LIMIT, FETCH_CHUNK_BYTES, FetchError, FetchErrorKind, UploadBody, UploadSender,
    upload_pipe,
};
use hyper::body::Bytes;
use rust_browser_javascript::{ErrorKind, JsError, RootId, Value, Vm};
use std::rc::Rc;

pub(super) struct Upload {
    reader: RootId,
    body: RootId,
    sender: UploadSender,
    bytes: Bytes,
    reading: bool,
    ended: bool,
}

impl Upload {
    pub(super) fn start(
        vm: &mut Vm,
        streams: &StreamHost,
        body: Value,
        inherited: bool,
    ) -> Result<(Self, UploadBody), JsError> {
        let reader = streams.lock_upload(vm, &body, inherited)?;
        let (sender, transport) = upload_pipe();
        Ok((
            Self {
                reader: vm.root(reader),
                body: vm.root(body),
                sender,
                bytes: Bytes::new(),
                reading: false,
                ended: false,
            },
            transport,
        ))
    }

    fn send_next(&mut self) {
        let count = self.bytes.len().min(FETCH_CHUNK_BYTES);
        let bytes = self.bytes.split_to(count);
        if self.sender.data(bytes).is_err() {
            self.bytes = Bytes::new();
        }
    }

    fn fail(&mut self, error: FetchError) {
        self.reading = false;
        self.bytes = Bytes::new();
        let _ = self.sender.fail(error);
    }

    pub(super) fn cancel(
        &mut self,
        vm: &mut Vm,
        streams: &StreamHost,
        reason: Value,
    ) -> Result<(), JsError> {
        if !self.ended {
            self.ended = true;
            self.bytes = Bytes::new();
            let body = vm.root_value(self.body).unwrap();
            streams.cancel_upload(vm, &body, reason)?;
        }
        Ok(())
    }

    pub(super) fn release(self, vm: &mut Vm) {
        vm.unroot(self.reader);
        vm.unroot(self.body);
    }
}

pub(super) fn demand(vm: &mut Vm, state: &Shared, id: u64) -> Result<(), JsError> {
    let (streams, reader) = {
        let mut state = state.borrow_mut();
        let streams = state.streams.clone();
        let Some(upload) = state
            .pending
            .get_mut(&id)
            .and_then(|pending| pending.upload.as_mut())
        else {
            return Ok(());
        };
        if upload.reading || upload.ended {
            return Ok(());
        }
        if !upload.bytes.is_empty() {
            upload.send_next();
            return Ok(());
        }
        upload.reading = true;
        (streams, vm.root_value(upload.reader).unwrap())
    };
    let owner = Rc::downgrade(state);
    let result = streams.pull_upload(
        vm,
        &reader,
        Box::new(move |vm, result| {
            if let Some(state) = owner.upgrade() {
                accept(vm, &state, id, result)?;
            }
            Ok(())
        }),
    );
    if let Err(error) = result {
        fail(state, id, "Fetch upload source failed");
        if fatal(&error) {
            return Err(error);
        }
    }
    Ok(())
}

fn accept(
    vm: &mut Vm,
    state: &Shared,
    id: u64,
    result: Result<Option<Value>, Value>,
) -> Result<(), JsError> {
    // Called at the read request's chunk steps. Snapshot before the producer
    // regains control and can mutate or transfer the delivered Uint8Array.
    if !state.borrow().pending.contains_key(&id) {
        return Ok(());
    }
    let bytes = match result {
        Ok(Some(chunk)) => match snapshot(vm, &chunk) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                fail(state, id, &error.message);
                return if fatal(&error) { Err(error) } else { Ok(()) };
            }
        },
        Ok(None) => None,
        Err(_) => {
            fail(state, id, "Fetch upload stream errored");
            return Ok(());
        }
    };
    let mut state = state.borrow_mut();
    if let Some(upload) = state
        .pending
        .get_mut(&id)
        .and_then(|pending| pending.upload.as_mut())
    {
        upload.reading = false;
        if let Some(bytes) = bytes {
            upload.bytes = bytes;
            upload.send_next();
        } else {
            upload.ended = true;
            let _ = upload.sender.end();
        }
    }
    Ok(())
}

fn fail(state: &Shared, id: u64, error: &str) {
    if let Some(upload) = state
        .borrow_mut()
        .pending
        .get_mut(&id)
        .and_then(|pending| pending.upload.as_mut())
    {
        upload.fail(FetchError::new(FetchErrorKind::Network, error));
    }
}

fn snapshot(vm: &mut Vm, chunk: &Value) -> Result<Bytes, JsError> {
    if !vm.is_uint8_array(chunk) {
        return Err(JsError::type_error(
            "Fetch upload chunks must be Uint8Array objects",
        ));
    }
    // Web IDL's byte copy of a detached or out-of-bounds view is empty.
    let length = match vm.buffer_source_len(chunk) {
        Ok(length) => length.unwrap_or_default(),
        Err(error) if error.kind == ErrorKind::TypeError => return Ok(Bytes::new()),
        Err(error) => return Err(error),
    };
    if length > FETCH_BODY_LIMIT {
        return Err(JsError::type_error("Fetch upload chunk exceeds 4 MiB"));
    }
    Ok(Bytes::from(
        vm.copy_buffer_source(chunk)?.unwrap_or_default(),
    ))
}
