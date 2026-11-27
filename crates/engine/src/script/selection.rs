//! The selection associated with the top-level document.
use super::{
    DispatchMode, Shared, arg, diagnostic, dispatch_checked_with_mode, document_node,
    dom_api::Errors,
    getter, node,
    range::{self, Bounds, Point},
    scheduler, webidl, wrap,
};
use crate::dom::{Document, NodeId, NodeKind};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, cmp::Ordering, rc::Rc, time::Duration};

const DOCUMENT_SELECTION: u32 = 0x5345_4c00;
const SELECTED_RANGE: u32 = 0;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Direction {
    #[default]
    None,
    Forward,
    Backward,
}

#[derive(Default)]
struct Selection {
    direction: Cell<Direction>,
    change_pending: Cell<bool>,
}

fn selection(vm: &Vm, receiver: &Value) -> Result<Rc<Selection>, JsError> {
    vm.host_data(receiver)
        .ok_or_else(|| JsError::type_error("Illegal Selection receiver"))
}

fn document_selection(vm: &mut Vm, document: Value, prototype: &Value) -> Result<Value, JsError> {
    if let Some(current @ Value::Object(_)) = vm.host_slot(&document, DOCUMENT_SELECTION) {
        return Ok(current);
    }
    let result = vm.new_object_with_prototype(prototype.clone())?;
    vm.mark_platform_object(&result)?;
    vm.set_host_data(result.clone(), Rc::new(Selection::default()))?;
    vm.set_host_slot(document, DOCUMENT_SELECTION, result.clone())?;
    Ok(result)
}

pub(super) fn reset_document(vm: &mut Vm, document: &Value) -> Result<(), JsError> {
    if vm.host_slot(document, DOCUMENT_SELECTION).is_some() {
        vm.set_host_slot(document.clone(), DOCUMENT_SELECTION, Value::Undefined)?;
    }
    Ok(())
}

fn selected_range(vm: &Vm, receiver: &Value) -> Option<Value> {
    vm.host_slot(receiver, SELECTED_RANGE)
        .filter(|value| value.as_object().is_some())
}

fn root(vm: &mut Vm, doc: &Document, mut id: NodeId) -> Result<NodeId, JsError> {
    while let Some(parent) = doc.nodes[id.0].parent {
        vm.charge_work(1)?;
        id = parent;
    }
    Ok(id)
}

fn visible_bounds(vm: &mut Vm, host: &Shared, receiver: &Value) -> Result<Option<Bounds>, JsError> {
    let Some(range) = selected_range(vm, receiver) else {
        return Ok(None);
    };
    let bounds = range::bounds(vm, &range)?;
    let state = host.borrow();
    let doc = &state.document;
    let visible = root(vm, doc, bounds.start.node)? == doc.root
        && root(vm, doc, bounds.end.node)? == doc.root;
    Ok(visible.then_some(bounds))
}

fn queue_change(vm: &mut Vm, host: &Shared, receiver: &Value, data: &Selection) {
    if data.change_pending.get() {
        return;
    }
    let owner = host.clone();
    let task = vm.new_native_function("selectionchange", move |vm, _, args| {
        let current = arg(args, 0);
        selection(vm, &current)?.change_pending.set(false);
        let root = owner.borrow().document.root;
        let document = wrap(vm, &owner, Some(root))?;
        if vm.host_slot(&document, DOCUMENT_SELECTION).as_ref() == Some(&current) {
            dispatch_checked_with_mode(
                vm,
                &owner,
                Some(root),
                "selectionchange",
                false,
                false,
                &[],
                DispatchMode::Host,
            )?;
        }
        Ok(Value::Undefined)
    });
    match scheduler::schedule_timer(vm, host, task, vec![receiver.clone()], Duration::ZERO) {
        Ok(_) => data.change_pending.set(true),
        Err(error) => diagnostic(host, error.to_string()),
    }
}

pub(super) fn range_changed(vm: &mut Vm, host: &Shared, range: &Value) {
    let document = {
        let state = host.borrow();
        state.wrappers.get(&state.document.root).cloned()
    };
    if let Some(document) = document
        && let Some(current) = vm.host_slot(&document, DOCUMENT_SELECTION)
        && selected_range(vm, &current).as_ref() == Some(range)
        && let Ok(data) = selection(vm, &current)
    {
        queue_change(vm, host, &current, &data);
    }
}

fn replace_range(
    vm: &mut Vm,
    host: &Shared,
    receiver: Value,
    data: &Selection,
    range: Option<Value>,
    direction: Direction,
) -> Result<Value, JsError> {
    if selected_range(vm, &receiver) != range {
        vm.set_host_slot(
            receiver.clone(),
            SELECTED_RANGE,
            range.unwrap_or(Value::Undefined),
        )?;
        data.direction.set(direction);
        queue_change(vm, host, &receiver, data);
    }
    Ok(Value::Undefined)
}

fn offset(vm: &mut Vm, value: &Value) -> Result<usize, JsError> {
    Ok(vm.to_number(value)?.trunc().rem_euclid(4_294_967_296.0) as usize)
}

fn check_offset(vm: &mut Vm, doc: &Document, point: Point, errors: &Errors) -> Result<(), JsError> {
    if point.offset > range::node_length(vm, doc, point.node)? {
        Err(errors.exception(
            vm,
            "IndexSizeError",
            "Selection offset exceeds the node length",
        ))
    } else {
        Ok(())
    }
}

fn check_node(vm: &mut Vm, doc: &Document, node: NodeId, errors: &Errors) -> Result<(), JsError> {
    if matches!(doc.nodes[node.0].kind, NodeKind::Doctype { .. }) {
        Err(errors.exception(
            vm,
            "InvalidNodeTypeError",
            "A doctype cannot be a selection boundary",
        ))
    } else {
        Ok(())
    }
}

fn point_range(vm: &mut Vm, prototype: &Value, point: Point) -> Result<Value, JsError> {
    range::create(
        vm,
        prototype.clone(),
        Bounds {
            start: point,
            end: point,
        },
    )
}

pub(super) fn install(
    vm: &mut Vm,
    host: &Shared,
    errors: &Errors,
    range_prototype: Value,
) -> Result<(), JsError> {
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "Selection", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Selection constructor"))
    })?;
    let owner = host.clone();
    let selection_prototype = prototype.clone();
    let document_prototype = host.borrow().document_prototype.clone();
    webidl::operation(
        vm,
        document_prototype,
        "getSelection",
        0,
        move |vm, document, _| {
            let id = document_node(&owner, &document)?;
            if id != owner.borrow().document.root {
                return Ok(Value::Null);
            }
            document_selection(vm, document, &selection_prototype)
        },
    )?;
    let owner = host.clone();
    let selection_prototype = prototype.clone();
    let window = host.borrow().window.clone();
    webidl::operation(
        vm,
        window.clone(),
        "getSelection",
        0,
        move |vm, receiver, _| {
            if receiver != window && !matches!(receiver, Value::Null | Value::Undefined) {
                return Err(JsError::type_error("Window receiver required"));
            }
            let root = owner.borrow().document.root;
            let document = wrap(vm, &owner, Some(root))?;
            document_selection(vm, document, &selection_prototype)
        },
    )?;

    for (name, anchor, node_property) in [
        ("anchorNode", true, true),
        ("anchorOffset", true, false),
        ("focusNode", false, true),
        ("focusOffset", false, false),
    ] {
        let owner = host.clone();
        getter(vm, prototype.clone(), name, move |vm, receiver, _| {
            let data = selection(vm, &receiver)?;
            let endpoint = visible_bounds(vm, &owner, &receiver)?.map(|bounds| {
                if anchor != (data.direction.get() == Direction::Backward) {
                    bounds.start
                } else {
                    bounds.end
                }
            });
            if node_property {
                wrap(vm, &owner, endpoint.map(|point| point.node))
            } else {
                Ok(endpoint.map_or(0, |point| point.offset).into())
            }
        })?;
    }
    for name in ["rangeCount", "type", "isCollapsed", "direction"] {
        let owner = host.clone();
        getter(vm, prototype.clone(), name, move |vm, receiver, _| {
            let data = selection(vm, &receiver)?;
            let bounds = visible_bounds(vm, &owner, &receiver)?;
            let collapsed = bounds.is_none_or(|bounds| bounds.start == bounds.end);
            Ok(match name {
                "rangeCount" => u32::from(bounds.is_some()).into(),
                "type" if bounds.is_none() => "None".into(),
                "type" if collapsed => "Caret".into(),
                "type" => "Range".into(),
                "isCollapsed" => collapsed.into(),
                "direction" => match selected_range(vm, &receiver).map(|_| data.direction.get()) {
                    Some(Direction::Forward) => "forward".into(),
                    Some(Direction::Backward) => "backward".into(),
                    _ => "none".into(),
                },
                _ => unreachable!(),
            })
        })?;
    }

    let owner = host.clone();
    let range_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "getRangeAt",
        1,
        move |vm, receiver, args| {
            selection(vm, &receiver)?;
            webidl::required(args, 1, "getRangeAt")?;
            if offset(vm, &args[0])? == 0 && visible_bounds(vm, &owner, &receiver)?.is_some() {
                return Ok(selected_range(vm, &receiver).expect("visible selection has a range"));
            }
            Err(range_errors.exception(
                vm,
                "IndexSizeError",
                "Selection has no range at that index",
            ))
        },
    )?;
    let owner = host.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "addRange",
        1,
        move |vm, receiver, args| {
            let data = selection(vm, &receiver)?;
            webidl::required(args, 1, "addRange")?;
            let range = args[0].clone();
            let bounds = range::bounds(vm, &range)?;
            let in_document = {
                let state = owner.borrow();
                let doc = &state.document;
                root(vm, doc, bounds.start.node)? == doc.root
                    && root(vm, doc, bounds.end.node)? == doc.root
            };
            if in_document && visible_bounds(vm, &owner, &receiver)?.is_none() {
                return replace_range(vm, &owner, receiver, &data, Some(range), Direction::Forward);
            }
            Ok(Value::Undefined)
        },
    )?;
    let owner = host.clone();
    let range_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "removeRange",
        1,
        move |vm, receiver, args| {
            let data = selection(vm, &receiver)?;
            webidl::required(args, 1, "removeRange")?;
            range::bounds(vm, &args[0])?;
            if selected_range(vm, &receiver).as_ref() != Some(&args[0]) {
                return Err(range_errors.exception(
                    vm,
                    "NotFoundError",
                    "The range is not selected",
                ));
            }
            replace_range(vm, &owner, receiver, &data, None, Direction::None)
        },
    )?;
    let owner = host.clone();
    let clear = webidl::operation(
        vm,
        prototype.clone(),
        "removeAllRanges",
        0,
        move |vm, receiver, _| {
            let data = selection(vm, &receiver)?;
            replace_range(vm, &owner, receiver, &data, None, Direction::None)
        },
    )?;
    vm.define_data_property(prototype.clone(), "empty", clear, true, true, true)?;

    let owner = host.clone();
    let boundary_errors = errors.clone();
    let range_proto = range_prototype.clone();
    let collapse = webidl::operation(
        vm,
        prototype.clone(),
        "collapse",
        1,
        move |vm, receiver, args| {
            let data = selection(vm, &receiver)?;
            webidl::required(args, 1, "collapse")?;
            let container = if matches!(args[0], Value::Null | Value::Undefined) {
                None
            } else {
                Some(node(&owner, &args[0])?)
            };
            let offset = offset(vm, &arg(args, 1))?;
            let Some(node) = container else {
                return replace_range(vm, &owner, receiver, &data, None, Direction::None);
            };
            let point = Point { node, offset };
            {
                let state = owner.borrow();
                let doc = &state.document;
                check_node(vm, doc, point.node, &boundary_errors)?;
                check_offset(vm, doc, point, &boundary_errors)?;
                if !doc.is_connected(point.node) {
                    return Ok(Value::Undefined);
                }
            }
            let range = point_range(vm, &range_proto, point)?;
            replace_range(
                vm,
                &owner,
                receiver,
                &data,
                Some(range),
                data.direction.get(),
            )
        },
    )?;
    vm.define_data_property(prototype.clone(), "setPosition", collapse, true, true, true)?;
    for (name, from_start) in [("collapseToStart", true), ("collapseToEnd", false)] {
        let owner = host.clone();
        let boundary_errors = errors.clone();
        let range_proto = range_prototype.clone();
        webidl::operation(vm, prototype.clone(), name, 0, move |vm, receiver, _| {
            let data = selection(vm, &receiver)?;
            let previous = selected_range(vm, &receiver).ok_or_else(|| {
                boundary_errors.exception(vm, "InvalidStateError", "The selection is empty")
            })?;
            let bounds = range::bounds(vm, &previous)?;
            let point = if from_start { bounds.start } else { bounds.end };
            let range = point_range(vm, &range_proto, point)?;
            replace_range(
                vm,
                &owner,
                receiver,
                &data,
                Some(range),
                data.direction.get(),
            )
        })?;
    }

    let owner = host.clone();
    let errors = errors.clone();
    webidl::operation(
        vm,
        prototype,
        "setBaseAndExtent",
        4,
        move |vm, receiver, args| {
            let data = selection(vm, &receiver)?;
            webidl::required(args, 4, "setBaseAndExtent")?;
            let anchor = Point {
                node: node(&owner, &args[0])?,
                offset: offset(vm, &args[1])?,
            };
            let focus = Point {
                node: node(&owner, &args[2])?,
                offset: offset(vm, &args[3])?,
            };
            let (bounds, direction) = {
                let state = owner.borrow();
                let doc = &state.document;
                check_offset(vm, doc, anchor, &errors)?;
                check_offset(vm, doc, focus, &errors)?;
                if !doc.is_connected(anchor.node) || !doc.is_connected(focus.node) {
                    return Ok(Value::Undefined);
                }
                check_node(vm, doc, anchor.node, &errors)?;
                check_node(vm, doc, focus.node, &errors)?;
                match range::compare(vm, doc, anchor, focus)? {
                    Some(Ordering::Greater) => (
                        Bounds {
                            start: focus,
                            end: anchor,
                        },
                        Direction::Backward,
                    ),
                    Some(_) => (
                        Bounds {
                            start: anchor,
                            end: focus,
                        },
                        Direction::Forward,
                    ),
                    None => (
                        Bounds {
                            start: anchor,
                            end: anchor,
                        },
                        Direction::Forward,
                    ),
                }
            };
            let range = range::create(vm, range_prototype.clone(), bounds)?;
            replace_range(vm, &owner, receiver, &data, Some(range), direction)
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{LoadedPage, script::ScriptRuntime};
    use rust_browser_javascript::Value;

    #[test]
    fn document_selection_shares_real_ranges_and_tracks_direction() {
        let page = LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><div contenteditable><span id='a'>A😀B</span><span id='b'>XY</span></div>",
        );
        let mut runtime = ScriptRuntime::new(&page).unwrap();
        let initial = runtime.eval(r#"
            const selected = document.getSelection();
            const start = document.getElementById('a').firstChild;
            const end = document.getElementById('b').firstChild;
            let changes = 0;
            document.addEventListener('selectionchange', () => changes++);
            function throws(action, name) {
                try { action(); } catch (error) { return error.name === name; }
                return false;
            }
            const empty = selected === window.getSelection() && selected === getSelection() &&
                selected instanceof Selection && selected.rangeCount === 0 && selected.type === 'None' &&
                selected.isCollapsed && selected.anchorNode === null && selected.focusOffset === 0 &&
                document.implementation.createHTMLDocument('').getSelection() === null &&
                throws(() => selected.getRangeAt(0), 'IndexSizeError') &&
                throws(() => selected.collapseToEnd(), 'InvalidStateError');
            let range = document.createRange();
            range.setStart(start, 1);
            range.setEnd(start, 3);
            selected.addRange(range);
            const byReference = selected.getRangeAt(0) === range;
            range.setEnd(end, 1);
            const updated = selected.focusNode === end && selected.focusOffset === 1 &&
                selected.anchorNode === start && selected.anchorOffset === 1;
            range = null;
            empty && byReference && updated && changes === 0
        "#).unwrap();
        assert_eq!(initial, Value::Bool(true));
        runtime.vm.collect_garbage();
        runtime.pump();
        let after_change = runtime
            .eval(
                r#"
            const live = selected.getRangeAt(0);
            const retained = live.startContainer === start && live.endContainer === end &&
                selected.rangeCount === 1 && selected.type === 'Range' && changes === 1;
            const errors = throws(() => selected.getRangeAt(1), 'IndexSizeError') &&
                throws(() => selected.removeRange(live.cloneRange()), 'NotFoundError') &&
                throws(() => selected.setBaseAndExtent(start, 5, end, 1), 'IndexSizeError') &&
                throws(() => selected.collapse(document.doctype), 'InvalidNodeTypeError');
            selected.setBaseAndExtent(end, 1, start, 1);
            const backward = selected.anchorNode === end && selected.focusNode === start &&
                selected.direction === 'backward';
            const backwardRange = selected.getRangeAt(0);
            selected.collapseToEnd();
            const collapsed = selected.anchorNode === end && selected.focusOffset === 1 &&
                selected.type === 'Caret' && selected.isCollapsed &&
                selected.getRangeAt(0) !== backwardRange && !backwardRange.collapsed;
            selected.removeRange(selected.getRangeAt(0));
            retained && errors && backward && collapsed && selected.rangeCount === 0 &&
                selected.focusNode === null && changes === 1
        "#,
            )
            .unwrap();
        assert_eq!(after_change, Value::Bool(true));
        runtime.pump();
        assert_eq!(runtime.eval("changes").unwrap(), Value::Number(2.));
    }
}
