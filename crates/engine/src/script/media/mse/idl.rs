use super::*;
use crate::script::{arg, event_target, getter};
use std::sync::Arc;

fn exception(vm: &mut Vm, state: &Shared, name: &str, text: &str) -> JsError {
    let constructor = state.borrow().media.exception.clone();
    super::super::abort::dom_exception_error(vm, constructor, name, text)
        .unwrap_or_else(|error| error)
}

fn state_error(vm: &mut Vm, state: &Shared, text: &str) -> JsError {
    exception(vm, state, "InvalidStateError", text)
}

fn owner(vm: &Vm, value: &Value) -> Result<(Value, Rc<Source>), JsError> {
    let object = vm
        .host_slot(value, OWNER)
        .expect("buffer owns its media source");
    let source = brand::<Source>(vm, &object, "MediaSource")?;
    Ok((object, source))
}

fn is_supported(input: &str) -> bool {
    !super::super::can_play_type(input).is_empty()
}

fn require_open(vm: &mut Vm, state: &Shared, source: &Source) -> Result<MediaKey, JsError> {
    source
        .key
        .get()
        .filter(|_| source.ready.get() == ReadyState::Open)
        .ok_or_else(|| state_error(vm, state, "MediaSource is not open"))
}

fn active_key(vm: &mut Vm, state: &Shared, source: &Source) -> Result<MediaKey, JsError> {
    source
        .key
        .get()
        .filter(|_| source.ready.get() != ReadyState::Closed)
        .ok_or_else(|| state_error(vm, state, "MediaSource is not attached"))
}

fn require_idle(vm: &mut Vm, state: &Shared, source_value: &Value) -> Result<(), JsError> {
    let list = list_for(vm, source_value, BUFFER_LIST);
    if list_values(vm, &list)?.iter().any(|value| {
        vm.host_data::<Buffer>(value)
            .is_some_and(|buffer| buffer.pending.get().is_some())
    }) {
        return Err(state_error(vm, state, "A SourceBuffer is updating"));
    }
    Ok(())
}

fn new_list(vm: &mut Vm, prototype: Value) -> Result<Value, JsError> {
    let list = vm.new_object_with_prototype(prototype)?;
    vm.set_host_data(list.clone(), Rc::new(List::default()))?;
    event_target::attach(vm, list.clone())?;
    Ok(list)
}

fn install_object_urls(vm: &mut Vm, state: &Shared) -> Result<(), JsError> {
    let url = vm.get_global("URL")?;
    let host = state.clone();
    webidl::operation(vm, url.clone(), "createObjectURL", 1, move |vm, _, args| {
        webidl::required(args, 1, "URL.createObjectURL")?;
        let value = arg(args, 0);
        if vm.host_data::<Source>(&value).is_none() && vm.host_data::<blobs::Blob>(&value).is_none()
        {
            return Err(JsError::type_error("Expected a Blob or MediaSource"));
        }
        let mut realm = host.borrow_mut();
        let origin = realm.url.clone();
        realm
            .media
            .mse
            .create_url(vm, &origin, value)
            .map(Into::into)
    })?;
    let host = state.clone();
    webidl::operation(vm, url, "revokeObjectURL", 1, move |vm, _, args| {
        webidl::required(args, 1, "URL.revokeObjectURL")?;
        let raw = webidl::usv_string(vm, &arg(args, 0), 256 * 1024)?;
        if let Ok(mut url) = Url::parse(&raw) {
            url.set_fragment(None);
            if let Some(root) = host.borrow_mut().media.mse.urls.remove(url.as_str()) {
                vm.unroot(root);
            }
        }
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn install_lists(vm: &mut Vm, prototype: &Value) -> Result<(), JsError> {
    webidl::interface(
        vm,
        prototype.clone(),
        "SourceBufferList",
        0,
        |_, _, _, _| Err(JsError::type_error("Illegal SourceBufferList constructor")),
    )?;
    getter(vm, prototype.clone(), "length", |vm, this, _| {
        let list = brand::<List>(vm, &this, "SourceBufferList")?;
        Ok((list.count.get() as u32).into())
    })?;
    for index in 0..MAX_BUFFERS {
        getter(
            vm,
            prototype.clone(),
            &index.to_string(),
            move |vm, this, _| {
                let list = brand::<List>(vm, &this, "SourceBufferList")?;
                Ok(if index < list.count.get() {
                    vm.host_slot(&this, FIRST_ENTRY + index as u32)
                        .unwrap_or_default()
                } else {
                    Value::Undefined
                })
            },
        )?;
    }
    let array = vm.get_global("Array")?;
    let array = vm.get_property(array, "prototype")?;
    let values = vm.get_property(array, "values")?;
    vm.define_data_property_key(
        prototype.clone(),
        webidl::well_known_key(vm, "iterator"),
        values,
        true,
        false,
        true,
    )?;
    for event in ["addsourcebuffer", "removesourcebuffer"] {
        event_target::install_handler(
            vm,
            prototype.clone(),
            event,
            |vm, this| brand::<List>(vm, this, "SourceBufferList").map(|_| ()),
            |_, _| Ok(()),
        )?;
    }
    Ok(())
}

fn install_ranges(vm: &mut Vm, state: &Shared, prototype: &Value) -> Result<(), JsError> {
    webidl::interface(vm, prototype.clone(), "TimeRanges", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal TimeRanges constructor"))
    })?;
    getter(vm, prototype.clone(), "length", |vm, this, _| {
        Ok((brand::<Ranges>(vm, &this, "TimeRanges")?.values.len() as u32).into())
    })?;
    for (method, start) in [("start", true), ("end", false)] {
        let host = state.clone();
        webidl::operation(vm, prototype.clone(), method, 1, move |vm, this, args| {
            let ranges = brand::<Ranges>(vm, &this, "TimeRanges")?;
            webidl::required(args, 1, method)?;
            let index = vm.to_number(&arg(args, 0))?;
            let index = index.trunc().rem_euclid(4294967296.) as usize;
            let &(left, right) = ranges.values.get(index).ok_or_else(|| {
                exception(vm, &host, "IndexSizeError", "Time range does not exist")
            })?;
            Ok(if start { left } else { right }.into())
        })?;
    }
    Ok(())
}

fn add_buffer(
    vm: &mut Vm,
    state: &Shared,
    object: Value,
    prototype: Value,
    mime: String,
) -> Result<Value, JsError> {
    let source = brand::<Source>(vm, &object, "MediaSource")?;
    let key = require_open(vm, state, &source)?;
    let list = list_for(vm, &object, BUFFER_LIST);
    let mut entries = list_values(vm, &list)?;
    if entries.len() == MAX_BUFFERS {
        return Err(exception(
            vm,
            state,
            "QuotaExceededError",
            "SourceBuffer count limit exceeded",
        ));
    }
    if mime.is_empty() {
        return Err(JsError::type_error("An empty media type is invalid"));
    }
    if !is_supported(&mime) {
        return Err(exception(
            vm,
            state,
            "NotSupportedError",
            "Unsupported SourceBuffer format",
        ));
    }

    let value = vm.new_object_with_prototype(prototype)?;
    event_target::attach(vm, value.clone())?;
    vm.set_host_slot(value.clone(), OWNER, object)?;
    let mut realm = state.borrow_mut();
    let media = &mut realm.media;
    media.check_command_room(1)?;
    media.mse.ensure_tasks(1)?;
    media.mse.next_buffer += 1;
    let id = media.mse.next_buffer;
    vm.set_host_data(
        value.clone(),
        Rc::new(Buffer {
            id,
            ranges: RefCell::default(),
            pending: Cell::default(),
            active: Cell::new(false),
            removed: Cell::new(false),
            timestamp_offset: Cell::new(0.),
            append_window: Cell::new((0., f64::INFINITY)),
        }),
    )?;
    entries.push(value.clone());
    replace_list(vm, list.clone(), &entries)?;
    media.commands.push_back(MediaCommand::AddBuffer {
        key,
        buffer: id,
        mime,
    });
    media.mse.event(vm, &list, "addsourcebuffer");
    Ok(value)
}

fn remove_buffer(vm: &mut Vm, state: &Shared, object: Value, value: Value) -> Result<(), JsError> {
    let source = brand::<Source>(vm, &object, "MediaSource")?;
    let buffer = brand::<Buffer>(vm, &value, "SourceBuffer")?;
    let list = list_for(vm, &object, BUFFER_LIST);
    let mut entries = list_values(vm, &list)?;
    let Some(index) = entries
        .iter()
        .position(|entry| entry.as_object() == value.as_object())
    else {
        return Err(exception(
            vm,
            state,
            "NotFoundError",
            "SourceBuffer is not in this MediaSource",
        ));
    };
    let key = active_key(vm, state, &source)?;
    let active = list_for(vm, &object, ACTIVE_LIST);
    let mut active_entries = list_values(vm, &active)?;
    let mut realm = state.borrow_mut();
    let media = &mut realm.media;
    media.check_command_room(1)?;
    media.mse.ensure_tasks(4)?;
    entries.remove(index);
    replace_list(vm, list.clone(), &entries)?;
    media.commands.push_back(MediaCommand::RemoveBuffer {
        key,
        buffer: buffer.id,
    });
    buffer.removed.set(true);
    buffer.ranges.borrow_mut().clear();
    if media.mse.release_pending(&buffer).is_some() {
        media.mse.event(vm, &value, "abort");
        media.mse.event(vm, &value, "updateend");
    }
    if let Some(index) = active_entries
        .iter()
        .position(|entry| entry.as_object() == value.as_object())
    {
        active_entries.remove(index);
        replace_list(vm, active.clone(), &active_entries)?;
        media.mse.event(vm, &active, "removesourcebuffer");
    }
    media.mse.event(vm, &list, "removesourcebuffer");
    Ok(())
}

fn media_source_methods(
    vm: &mut Vm,
    state: &Shared,
    prototype: &Value,
    buffer: &Value,
) -> Result<(), JsError> {
    let host = state.clone();
    let buffer_prototype = buffer.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "addSourceBuffer",
        1,
        move |vm, this, args| {
            webidl::required(args, 1, "addSourceBuffer")?;
            let mime = webidl::usv_string(vm, &arg(args, 0), 4096)?;
            add_buffer(vm, &host, this, buffer_prototype.clone(), mime)
        },
    )?;
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "removeSourceBuffer",
        1,
        move |vm, this, args| {
            webidl::required(args, 1, "removeSourceBuffer")?;
            remove_buffer(vm, &host, this, arg(args, 0))?;
            Ok(Value::Undefined)
        },
    )?;
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "endOfStream",
        0,
        move |vm, this, args| {
            let reason = match arg(args, 0) {
                Value::Undefined => None,
                value => match webidl::usv_string(vm, &value, 32)?.as_str() {
                    "network" => Some("network"),
                    "decode" => Some("decode"),
                    _ => return Err(JsError::type_error("Invalid EndOfStreamError")),
                },
            };
            let source = brand::<Source>(vm, &this, "MediaSource")?;
            let key = require_open(vm, &host, &source)?;
            require_idle(vm, &host, &this)?;
            let mut realm = host.borrow_mut();
            let media = &mut realm.media;
            media.mse.ensure_tasks(1)?;
            media.enqueue(MediaCommand::EndStream { key, error: reason })?;
            source.ready.set(ReadyState::Ended);
            media.mse.event(vm, &this, "sourceended");
            Ok(Value::Undefined)
        },
    )?;
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "setLiveSeekableRange",
        2,
        move |vm, this, args| {
            webidl::required(args, 2, "setLiveSeekableRange")?;
            let start = vm.to_number(&arg(args, 0))?;
            let end = vm.to_number(&arg(args, 1))?;
            let source = brand::<Source>(vm, &this, "MediaSource")?;
            require_open(vm, &host, &source)?;
            if !start.is_finite() || !end.is_finite() || start < 0. || start > end {
                return Err(JsError::type_error("Invalid live seekable range"));
            }
            source.live.set(Some((start, end)));
            Ok(Value::Undefined)
        },
    )?;
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "clearLiveSeekableRange",
        0,
        move |vm, this, _| {
            let source = brand::<Source>(vm, &this, "MediaSource")?;
            require_open(vm, &host, &source)?;
            source.live.set(None);
            Ok(Value::Undefined)
        },
    )?;
    Ok(())
}

fn source_attributes(vm: &mut Vm, state: &Shared, prototype: &Value) -> Result<(), JsError> {
    getter(vm, prototype.clone(), "readyState", |vm, this, _| {
        Ok(
            match brand::<Source>(vm, &this, "MediaSource")?.ready.get() {
                ReadyState::Closed => "closed",
                ReadyState::Open => "open",
                ReadyState::Ended => "ended",
            }
            .into(),
        )
    })?;
    for (name, slot) in [
        ("sourceBuffers", BUFFER_LIST),
        ("activeSourceBuffers", ACTIVE_LIST),
    ] {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            brand::<Source>(vm, &this, "MediaSource")?;
            Ok(list_for(vm, &this, slot))
        })?;
    }
    let get = vm.new_native_function("get duration", |vm, this, _| {
        Ok(brand::<Source>(vm, &this, "MediaSource")?
            .duration
            .get()
            .into())
    });
    let host = state.clone();
    let set = vm.new_native_function("set duration", move |vm, this, args| {
        let duration = vm.to_number(&arg(args, 0))?;
        if duration.is_nan() || duration < 0. {
            return Err(JsError::type_error("Invalid media duration"));
        }
        let source = brand::<Source>(vm, &this, "MediaSource")?;
        let key = require_open(vm, &host, &source)?;
        require_idle(vm, &host, &this)?;
        let list = list_for(vm, &this, BUFFER_LIST);
        if list_values(vm, &list)?.iter().any(|value| {
            vm.host_data::<Buffer>(value).is_some_and(|buffer| {
                buffer
                    .ranges
                    .borrow()
                    .last()
                    .is_some_and(|range| range.1 > duration)
            })
        }) {
            return Err(state_error(
                vm,
                &host,
                "Duration is shorter than buffered media",
            ));
        }
        host.borrow_mut()
            .media
            .enqueue(MediaCommand::SetDuration { key, duration })?;
        source.duration.set(duration);
        Ok(Value::Undefined)
    });
    vm.define_accessor(prototype.clone(), "duration", Some(get), Some(set))?;
    for event in ["sourceopen", "sourceended", "sourceclose"] {
        event_target::install_handler(
            vm,
            prototype.clone(),
            event,
            |vm, this| brand::<Source>(vm, this, "MediaSource").map(|_| ()),
            |_, _| Ok(()),
        )?;
    }
    Ok(())
}

enum Edit {
    Append(Arc<[u8]>),
    Remove { start: f64, end: f64 },
}

fn edit(
    vm: &mut Vm,
    state: &Shared,
    value: Value,
    buffer: Rc<Buffer>,
    operation: Edit,
) -> Result<Value, JsError> {
    let (source_value, source) = owner(vm, &value)?;
    let key = active_key(vm, state, &source)?;
    if buffer.removed.get() || buffer.pending.get().is_some() {
        return Err(state_error(
            vm,
            state,
            "SourceBuffer is removed or updating",
        ));
    }
    let bytes = match &operation {
        Edit::Append(bytes) => bytes.len(),
        Edit::Remove { start, end } => {
            let duration = source.duration.get();
            if duration.is_nan() || *start < 0. || *start > duration || end <= start {
                return Err(JsError::type_error("Invalid media removal range"));
            }
            0
        }
    };
    if state.borrow().media.mse.pending_bytes + bytes > MAX_PENDING_BYTES {
        return Err(exception(
            vm,
            state,
            "QuotaExceededError",
            "Pending media data exceeds 32 MiB",
        ));
    }
    let mut realm = state.borrow_mut();
    let media = &mut realm.media;
    media.check_command_room(1)?;
    media.mse.ensure_tasks(2)?;
    media.mse.next_operation += 1;
    let id = media.mse.next_operation;
    let removing = matches!(&operation, Edit::Remove { .. });
    let command = match operation {
        Edit::Append(bytes) => MediaCommand::Append {
            key,
            buffer: buffer.id,
            operation: id,
            bytes,
            timestamp_offset: buffer.timestamp_offset.get(),
            append_window: buffer.append_window.get(),
        },
        Edit::Remove { start, end } => MediaCommand::RemoveRange {
            key,
            buffer: buffer.id,
            operation: id,
            start,
            end,
        },
    };
    if source.ready.replace(ReadyState::Open) == ReadyState::Ended {
        media.mse.event(vm, &source_value, "sourceopen");
    }
    buffer.pending.set(Some(Operation {
        id,
        bytes,
        removing,
    }));
    media.mse.pending_bytes += bytes;
    media.commands.push_back(command);
    media.mse.event(vm, &value, "updatestart");
    Ok(Value::Undefined)
}

fn buffer_methods(vm: &mut Vm, state: &Shared, prototype: &Value) -> Result<(), JsError> {
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "appendBuffer",
        1,
        move |vm, this, args| {
            let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
            webidl::required(args, 1, "appendBuffer")?;
            let value = arg(args, 0);
            let length = vm.fixed_buffer_source_bytes(&value)?.len();
            if length > MAX_APPEND_BYTES
                || host.borrow().media.mse.pending_bytes + length > MAX_PENDING_BYTES
            {
                return Err(exception(
                    vm,
                    &host,
                    "QuotaExceededError",
                    "SourceBuffer append exceeds the media byte limit",
                ));
            }
            vm.charge_work(length.div_ceil(64))?;
            let bytes = Arc::from(vm.fixed_buffer_source_bytes(&value)?);
            edit(vm, &host, this, buffer, Edit::Append(bytes))
        },
    )?;
    let host = state.clone();
    webidl::operation(vm, prototype.clone(), "remove", 2, move |vm, this, args| {
        let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
        webidl::required(args, 2, "SourceBuffer.remove")?;
        let start = vm.to_number(&arg(args, 0))?;
        let end = vm.to_number(&arg(args, 1))?;
        if !start.is_finite() || end.is_nan() {
            return Err(JsError::type_error("Invalid media removal range"));
        }
        edit(vm, &host, this, buffer, Edit::Remove { start, end })
    })?;
    let host = state.clone();
    webidl::operation(vm, prototype.clone(), "abort", 0, move |vm, this, _| {
        let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
        let (_, source) = owner(vm, &this)?;
        let key = require_open(vm, &host, &source)?;
        if buffer.removed.get()
            || buffer
                .pending
                .get()
                .is_some_and(|operation| operation.removing)
        {
            return Err(state_error(
                vm,
                &host,
                "SourceBuffer is removed or removing data",
            ));
        }
        let mut realm = host.borrow_mut();
        let media = &mut realm.media;
        media.mse.ensure_tasks(2)?;
        media.enqueue(MediaCommand::AbortBuffer {
            key,
            buffer: buffer.id,
        })?;
        if media.mse.release_pending(&buffer).is_some() {
            media.mse.event(vm, &this, "abort");
            media.mse.event(vm, &this, "updateend");
        }
        buffer.append_window.set((0., f64::INFINITY));
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn buffer_attributes(vm: &mut Vm, state: &Shared, prototype: &Value) -> Result<(), JsError> {
    getter(vm, prototype.clone(), "updating", |vm, this, _| {
        Ok(brand::<Buffer>(vm, &this, "SourceBuffer")?
            .pending
            .get()
            .is_some()
            .into())
    })?;
    let host = state.clone();
    getter(vm, prototype.clone(), "buffered", move |vm, this, _| {
        let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
        if buffer.removed.get() {
            return Err(state_error(vm, &host, "SourceBuffer was removed"));
        }
        let ranges = buffer.ranges.borrow().clone();
        make_ranges(vm, &host.borrow().media.mse, ranges)
    })?;
    let get = vm.new_native_function("get mode", |vm, this, _| {
        brand::<Buffer>(vm, &this, "SourceBuffer")?;
        Ok("segments".into())
    });
    let host = state.clone();
    let set = vm.new_native_function("set mode", move |vm, this, args| {
        let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
        let mode = webidl::usv_string(vm, &arg(args, 0), 32)?;
        if buffer.removed.get() || buffer.pending.get().is_some() {
            return Err(state_error(
                vm,
                &host,
                "SourceBuffer is removed or updating",
            ));
        }
        match mode.as_str() {
            "segments" => Ok(Value::Undefined),
            "sequence" => Err(exception(
                vm,
                &host,
                "NotSupportedError",
                "Sequence mode is unavailable for this decoder",
            )),
            _ => Err(JsError::type_error("Invalid AppendMode")),
        }
    });
    vm.define_accessor(prototype.clone(), "mode", Some(get), Some(set))?;
    for (kind, name) in [
        (0, "timestampOffset"),
        (1, "appendWindowStart"),
        (2, "appendWindowEnd"),
    ] {
        let get = vm.new_native_function(name, move |vm, this, _| {
            let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
            let window = buffer.append_window.get();
            Ok(match kind {
                0 => buffer.timestamp_offset.get(),
                1 => window.0,
                _ => window.1,
            }
            .into())
        });
        let host = state.clone();
        let set = vm.new_native_function(name, move |vm, this, args| {
            let buffer = brand::<Buffer>(vm, &this, "SourceBuffer")?;
            let number = vm.to_number(&arg(args, 0))?;
            if buffer.removed.get() || buffer.pending.get().is_some() {
                return Err(state_error(
                    vm,
                    &host,
                    "SourceBuffer is removed or updating",
                ));
            }
            let (start, end) = buffer.append_window.get();
            match kind {
                0 if number.is_finite() => buffer.timestamp_offset.set(number),
                1 if number >= 0. && number < end => buffer.append_window.set((number, end)),
                2 if number > start => buffer.append_window.set((start, number)),
                _ => return Err(JsError::type_error("Invalid timestamp or append window")),
            }
            Ok(Value::Undefined)
        });
        vm.define_accessor(prototype.clone(), name, Some(get), Some(set))?;
    }
    for event in ["updatestart", "update", "updateend", "error", "abort"] {
        event_target::install_handler(
            vm,
            prototype.clone(),
            event,
            |vm, this| brand::<Buffer>(vm, this, "SourceBuffer").map(|_| ()),
            |_, _| Ok(()),
        )?;
    }
    Ok(())
}

pub(in crate::script::media) fn install(
    vm: &mut Vm,
    state: &Shared,
    events: event_target::Events,
) -> Result<(), JsError> {
    let source = vm.new_object_with_prototype(events.target_proto.clone())?;
    let buffer = vm.new_object_with_prototype(events.target_proto.clone())?;
    let list = vm.new_object_with_prototype(events.target_proto.clone())?;
    let ranges = vm.new_object();
    for prototype in [&source, &buffer, &list, &ranges] {
        vm.root(prototype.clone());
    }
    {
        let mut realm = state.borrow_mut();
        realm.media.mse.events = Some(events.without_document());
        realm.media.mse.ranges_prototype = ranges.clone();
    }
    let source_prototype = source.clone();
    let list_prototype = list.clone();
    let constructor = webidl::interface(
        vm,
        source.clone(),
        "MediaSource",
        0,
        move |vm, _, _, new_target| {
            let new_target =
                new_target.ok_or_else(|| JsError::type_error("MediaSource requires new"))?;
            let object = webidl::instance(vm, new_target, &source_prototype)?;
            vm.set_host_data(
                object.clone(),
                Rc::new(Source {
                    duration: Cell::new(f64::NAN),
                    ..Source::default()
                }),
            )?;
            event_target::attach(vm, object.clone())?;
            for slot in [BUFFER_LIST, ACTIVE_LIST] {
                let list = new_list(vm, list_prototype.clone())?;
                vm.set_host_slot(object.clone(), slot, list)?;
            }
            Ok(object)
        },
    )?;
    webidl::operation(vm, constructor, "isTypeSupported", 1, |vm, _, args| {
        webidl::required(args, 1, "MediaSource.isTypeSupported")?;
        let value = webidl::usv_string(vm, &arg(args, 0), 4096)?;
        Ok(is_supported(&value).into())
    })?;
    webidl::interface(vm, buffer.clone(), "SourceBuffer", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal SourceBuffer constructor"))
    })?;
    install_object_urls(vm, state)?;
    install_lists(vm, &list)?;
    install_ranges(vm, state, &ranges)?;
    source_attributes(vm, state, &source)?;
    media_source_methods(vm, state, &source, &buffer)?;
    buffer_attributes(vm, state, &buffer)?;
    buffer_methods(vm, state, &buffer)
}
