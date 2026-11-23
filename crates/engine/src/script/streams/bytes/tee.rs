//! A byte tee can supply its requesting branch's buffer to the upstream source.
use super::super::{Lock, OWNER, Stream, Tee};
use super::{
    ArrayBufferViewKind, Item, JsError, PullInto, ReaderKind, Shared, Source, Status, StreamHost,
    Value, Vm, accept_response, append, arg, clear_slot, clone_item, head, item_length, shift,
    wants,
};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

const WAITING_BRANCHES: [u32; 2] = [18, 19];

#[derive(Clone)]
pub(super) struct Loan {
    branch: Weak<RefCell<Stream>>,
    slot: u32,
}

fn pair(source: &Shared) -> Option<Rc<RefCell<Tee>>> {
    match &source.borrow().lock {
        Lock::Tee(tee) => Some(tee.clone()),
        _ => None,
    }
}

fn needs_chunk(branch: &Shared) -> bool {
    let state = branch.borrow();
    state.status == Status::Readable && state.queue.is_empty() && wants(&state)
}

fn trace_waiters(vm: &mut Vm, source: &Shared, tee: &Rc<RefCell<Tee>>) -> Result<(), JsError> {
    let owner = source.borrow().value();
    let branches = tee.borrow().branches.clone();
    for (branch, slot) in branches.iter().zip(WAITING_BRANCHES) {
        if let Some(branch) = branch.upgrade().filter(needs_chunk) {
            vm.set_host_slot(owner.clone(), slot, branch.borrow().value())?;
        }
    }
    Ok(())
}

pub(in crate::script::streams) fn clear_waiters(vm: &mut Vm, owner: &Value) -> Result<(), JsError> {
    for slot in WAITING_BRANCHES {
        clear_slot(vm, owner, slot)?;
    }
    Ok(())
}

pub(in crate::script::streams) fn pull_from_branch(
    vm: &mut Vm,
    source: &Shared,
    branch: &Shared,
) -> Result<bool, JsError> {
    if let Some(tee) = pair(source) {
        let busy = {
            let tee = tee.borrow();
            tee.byte_reading || tee.byte_delivering
        };
        if busy {
            trace_waiters(vm, source, &tee)?;
            return Ok(false);
        }
    }
    begin(vm, source, Some(branch))?;
    Ok(true)
}

pub(in crate::script::streams) fn begin(
    vm: &mut Vm,
    source: &Shared,
    preferred: Option<&Shared>,
) -> Result<(), JsError> {
    let auto_allocate = {
        let state = source.borrow();
        if !state.bytes
            || state.closing
            || !matches!(state.source, Source::Custom(_) | Source::Branch { .. })
        {
            return Ok(());
        }
        state.auto_allocate
    };
    let Some(tee) = pair(source) else {
        return Ok(());
    };
    let branches = {
        let tee = tee.borrow();
        if tee.byte_reading || tee.byte_delivering {
            return Ok(());
        }
        tee.branches.clone()
    };
    let chosen = preferred
        .filter(|branch| needs_chunk(branch))
        .cloned()
        .or_else(|| branches.iter().filter_map(Weak::upgrade).find(needs_chunk));
    let Some(branch) = chosen else { return Ok(()) };
    // The parent may be kept alive by an underlying source callback while
    // a branch is reachable only through its pending async consumer.
    trace_waiters(vm, source, &tee)?;
    if head(source).is_some() {
        // A previously released author reader can still have an active request.
        tee.borrow_mut().byte_reading = true;
        return Ok(());
    }
    if let Some(pull) = head(&branch)
        .filter(|pull| matches!(pull.reader, ReaderKind::Byob | ReaderKind::TeeByob) && !pull.lent)
    {
        let object = branch.borrow().value();
        let buffer = vm.transfer_array_buffer(&pull.buffer(vm, &object))?;
        let mut request = PullInto::new(
            pull.buffer_length,
            pull.offset()..pull.range.end,
            ArrayBufferViewKind::UINT8,
            1,
            ReaderKind::TeeByob,
        );
        request.loan = Some(Loan {
            branch: Rc::downgrade(&branch),
            slot: pull.slot,
        });
        append(vm, source, request, buffer)?;
        clear_slot(vm, &object, pull.slot)?;
        branch.borrow_mut().pull_intos.front_mut().unwrap().lent = true;
    } else if let Some(size) = auto_allocate {
        let size = usize::try_from(size)
            .map_err(|_| super::range_error("Byte stream buffer is too large"))?;
        let buffer = vm.new_zeroed_array_buffer(size)?;
        let request = PullInto::new(
            size,
            0..size,
            ArrayBufferViewKind::UINT8,
            1,
            ReaderKind::TeeDefault,
        );
        append(vm, source, request, buffer)?;
    }
    tee.borrow_mut().byte_reading = true;
    Ok(())
}

pub(in crate::script::streams) fn delivering(source: &Shared) -> bool {
    pair(source).is_some_and(|tee| tee.borrow().byte_delivering)
}

pub(in crate::script::streams) fn waits_for_close(source: &Shared) -> bool {
    delivering(source) || head(source).is_some_and(|pull| pull.reader == ReaderKind::TeeByob)
}

pub(in crate::script::streams) fn owns_loan(source: &Shared) -> bool {
    head(source).is_some_and(|pull| pull.reader == ReaderKind::TeeByob && !pull.lent)
}

pub(in crate::script::streams) fn responded(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
) -> Result<(), JsError> {
    let (pull, buffer) = shift(vm, source)?.expect("active tee pull");
    let is_closed = source.borrow().status == Status::Closed;
    if is_closed && pull.loan.is_none() {
        return Ok(());
    }
    let view = vm.new_array_buffer_view(
        &buffer,
        pull.range.start..pull.offset(),
        ArrayBufferViewKind::UINT8,
    )?;
    schedule(host, vm, source, view, pull.loan)
}

pub(in crate::script::streams) fn deliver(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    item: Item,
) -> Result<(), JsError> {
    if let Some(pull) = head(source) {
        if pull.reader == ReaderKind::TeeDefault {
            shift(vm, source)?;
        } else if pull.reader == ReaderKind::TeeByob {
            let object = source.borrow().value();
            let buffer = pull.buffer(vm, &object);
            let length = item_length(vm, &item)?;
            let count = length.min(pull.range.len());
            let leftover = super::copy_into_view(vm, &buffer, pull.offset(), item, count)?;
            if let Some(leftover) = leftover {
                host.enqueue_front(vm, source, leftover, (length - count) as f64)?;
            }
            source.borrow_mut().pull_intos.front_mut().unwrap().filled = count;
            return responded(host, vm, source);
        }
    }
    let view = item.into_value(vm)?;
    schedule(host, vm, source, view, None)
}

fn schedule(
    host: &StreamHost,
    vm: &mut Vm,
    source: &Shared,
    view: Value,
    loan: Option<Loan>,
) -> Result<(), JsError> {
    let tee = pair(source).expect("tee source remains locked");
    tee.borrow_mut().byte_delivering = true;
    let owner = Rc::downgrade(source);
    let host = host.clone();
    let finish = vm.new_native_function("", move |vm, _, args| {
        let Some(source) = owner.upgrade() else {
            return Ok(Value::Undefined);
        };
        let view = arg(args, 0);
        let distribution = if source.borrow().status == Status::Errored {
            Ok(())
        } else {
            if let Some(loan) = &loan {
                distribute_loan(&host, vm, &tee, loan, view)
            } else {
                host.distribute_tee(vm, &tee, Item::Js(view), true)
            }
        };
        {
            let mut tee = tee.borrow_mut();
            tee.byte_reading = false;
            tee.byte_delivering = false;
        }
        let (status, object) = {
            let state = source.borrow();
            (state.status, state.value())
        };
        clear_waiters(vm, &object)?;
        if let Err(error) = distribution {
            let reason = super::exception(vm, error)?;
            if status == Status::Closed {
                host.finish_tee(vm, &object, &tee, Some(reason))?;
            } else {
                host.fail(vm, &source, reason)?;
            }
            return Ok(Value::Undefined);
        }
        match status {
            Status::Readable => host.advance(vm, &source)?,
            Status::Closed => host.finish_tee(vm, &object, &tee, None)?,
            Status::Errored => {}
        }
        Ok(Value::Undefined)
    });
    vm.set_host_slot(finish.clone(), OWNER, source.borrow().value())?;
    vm.enqueue_call(finish, Value::Undefined, &[view])
}

fn distribute_loan(
    host: &StreamHost,
    vm: &mut Vm,
    tee: &Rc<RefCell<Tee>>,
    loan: &Loan,
    value: Value,
) -> Result<(), JsError> {
    let view = vm.fixed_array_buffer_view(&value)?;
    let count = view.byte_range.len();
    let other = if count == 0 {
        None
    } else {
        tee.borrow()
            .branches
            .iter()
            .find(|branch| !Weak::ptr_eq(branch, &loan.branch))
            .and_then(Weak::upgrade)
            .filter(|branch| branch.borrow().status == Status::Readable)
            .map(|branch| clone_item(vm, &Item::Js(value)).map(|chunk| (branch, chunk)))
            .transpose()?
    };
    if let Some(branch) = loan.branch.upgrade()
        && head(&branch).is_some_and(|pull| pull.lent && pull.slot == loan.slot)
    {
        accept_response(host, vm, &branch, view.buffer, count)?;
    }
    if let Some((branch, chunk)) = other {
        host.deliver_branch(vm, &branch, chunk)?;
    }
    Ok(())
}
