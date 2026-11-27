//! Document ranges and contextual parsing over Satin's DOM arena.
use super::{
    Shared, arg, document_node,
    dom_api::{self, Errors},
    getter, node, text, webidl, wrap,
};
use crate::dom::{Document, NodeId, NodeKind};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, cmp::Ordering, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Point {
    node: NodeId,
    offset: usize,
}

#[derive(Clone, Copy)]
struct Bounds {
    start: Point,
    end: Point,
}

struct Range(Cell<Bounds>);

fn range(vm: &Vm, receiver: &Value) -> Result<Rc<Range>, JsError> {
    vm.host_data(receiver)
        .ok_or_else(|| JsError::type_error("Illegal Range receiver"))
}

fn new_range(vm: &mut Vm, object: Value, bounds: Bounds) -> Result<Value, JsError> {
    vm.mark_platform_object(&object)?;
    vm.set_host_data(object.clone(), Rc::new(Range(Cell::new(bounds))))?;
    Ok(object)
}

fn collapsed(node: NodeId) -> Bounds {
    let point = Point { node, offset: 0 };
    Bounds {
        start: point,
        end: point,
    }
}

fn ancestry(vm: &mut Vm, doc: &Document, mut node: NodeId) -> Result<Vec<NodeId>, JsError> {
    let mut result = vec![node];
    while let Some(parent) = doc.nodes[node.0].parent {
        vm.charge_work(1)?;
        result.push(parent);
        node = parent;
    }
    result.reverse();
    Ok(result)
}

fn child_offset(
    vm: &mut Vm,
    doc: &Document,
    parent: NodeId,
    child: NodeId,
) -> Result<usize, JsError> {
    let children = &doc.nodes[parent.0].children;
    vm.charge_buffer_work(children.len())?;
    Ok(children
        .iter()
        .position(|&node| node == child)
        .expect("DOM parent owns child"))
}

fn compare(
    vm: &mut Vm,
    doc: &Document,
    left: Point,
    right: Point,
) -> Result<Option<Ordering>, JsError> {
    if left.node == right.node {
        return Ok(Some(left.offset.cmp(&right.offset)));
    }
    let a = ancestry(vm, doc, left.node)?;
    let b = ancestry(vm, doc, right.node)?;
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let result = match (a.get(common), b.get(common)) {
        (None, Some(child)) => {
            if left.offset <= child_offset(vm, doc, left.node, *child)? {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(child), None) => {
            if right.offset <= child_offset(vm, doc, right.node, *child)? {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(a), Some(b)) if common > 0 => {
            let parent = doc.nodes[a.0].parent.expect("Common parent");
            child_offset(vm, doc, parent, *a)?.cmp(&child_offset(vm, doc, parent, *b)?)
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

fn node_length(vm: &mut Vm, doc: &Document, node: NodeId) -> Result<usize, JsError> {
    let node = &doc.nodes[node.0];
    if let Some(text) = node.kind.character_data() {
        vm.charge_buffer_work(text.len())?;
        return Ok(text.encode_utf16().count());
    }
    Ok(node.children.len())
}

fn set_boundary(
    vm: &mut Vm,
    doc: &Document,
    bounds: &Range,
    point: Point,
    start: bool,
) -> Result<(), JsError> {
    let mut value = bounds.0.get();
    if start {
        if compare(vm, doc, point, value.end)?.is_none_or(|order| order.is_gt()) {
            value.end = point;
        }
        value.start = point;
    } else {
        if compare(vm, doc, point, value.start)?.is_none_or(|order| order.is_lt()) {
            value.start = point;
        }
        value.end = point;
    }
    bounds.0.set(value);
    Ok(())
}

pub(super) fn install(vm: &mut Vm, host: &Shared, errors: &Errors) -> Result<(), JsError> {
    let prototype = vm.new_object();
    let intrinsic = prototype.clone();
    let owner = host.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "Range",
        0,
        move |vm, _, _, target| {
            let target = target.ok_or_else(|| JsError::type_error("Range requires new"))?;
            let object = webidl::instance(vm, target, &intrinsic)?;
            new_range(vm, object, collapsed(owner.borrow().document.root))
        },
    )?;
    let owner = host.clone();
    let intrinsic = prototype.clone();
    let document_proto = host.borrow().document_prototype.clone();
    webidl::operation(
        vm,
        document_proto,
        "createRange",
        0,
        move |vm, document, _| {
            let id = document_node(&owner, &document)?;
            let object = vm.new_object_with_prototype(intrinsic.clone())?;
            new_range(vm, object, collapsed(id))
        },
    )?;
    for (name, value) in [
        ("START_TO_START", 0),
        ("START_TO_END", 1),
        ("END_TO_END", 2),
        ("END_TO_START", 3),
    ] {
        for target in [&constructor, &prototype] {
            vm.define_data_property(target.clone(), name, value.into(), false, true, false)?;
        }
    }
    for start in [true, false] {
        let owner = host.clone();
        let name = if start {
            "startContainer"
        } else {
            "endContainer"
        };
        getter(vm, prototype.clone(), name, move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            wrap(
                vm,
                &owner,
                Some(if start {
                    bounds.start.node
                } else {
                    bounds.end.node
                }),
            )
        })?;
        let name = if start { "startOffset" } else { "endOffset" };
        getter(vm, prototype.clone(), name, move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            Ok((if start {
                bounds.start.offset
            } else {
                bounds.end.offset
            })
            .into())
        })?;
        let owner = host.clone();
        let errors = errors.clone();
        let name = if start { "setStart" } else { "setEnd" };
        webidl::operation(vm, prototype.clone(), name, 2, move |vm, receiver, args| {
            let range = range(vm, &receiver)?;
            webidl::required(args, 2, name)?;
            let node = node(&owner, &args[0])?;
            let offset = vm.to_number(&args[1])?.trunc().rem_euclid(4_294_967_296.0) as usize;
            let state = owner.borrow();
            let doc = &state.document;
            if matches!(doc.nodes[node.0].kind, NodeKind::Doctype { .. }) {
                return Err(errors.exception(
                    vm,
                    "InvalidNodeTypeError",
                    "A doctype cannot be a boundary container",
                ));
            }
            if offset > node_length(vm, doc, node)? {
                return Err(errors.exception(
                    vm,
                    "IndexSizeError",
                    "Range offset exceeds the node length",
                ));
            }
            set_boundary(vm, doc, &range, Point { node, offset }, start)?;
            Ok(Value::Undefined)
        })?;
    }
    getter(vm, prototype.clone(), "collapsed", |vm, receiver, _| {
        let bounds = range(vm, &receiver)?.0.get();
        Ok((bounds.start == bounds.end).into())
    })?;
    let owner = host.clone();
    getter(
        vm,
        prototype.clone(),
        "commonAncestorContainer",
        move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            let common = {
                let state = owner.borrow();
                let a = ancestry(vm, &state.document, bounds.start.node)?;
                let b = ancestry(vm, &state.document, bounds.end.node)?;
                a.into_iter()
                    .zip(b)
                    .take_while(|(x, y)| x == y)
                    .last()
                    .map(|(node, _)| node)
            };
            wrap(vm, &owner, common)
        },
    )?;
    webidl::operation(
        vm,
        prototype.clone(),
        "collapse",
        0,
        |vm, receiver, args| {
            let range = range(vm, &receiver)?;
            let bounds = range.0.get();
            let point = if arg(args, 0).is_truthy() {
                bounds.start
            } else {
                bounds.end
            };
            range.0.set(Bounds {
                start: point,
                end: point,
            });
            Ok(Value::Undefined)
        },
    )?;
    let intrinsic = prototype.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "cloneRange",
        0,
        move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            let object = vm.new_object_with_prototype(intrinsic.clone())?;
            new_range(vm, object, bounds)
        },
    )?;
    for contents in [false, true] {
        let owner = host.clone();
        let errors = errors.clone();
        let name = if contents {
            "selectNodeContents"
        } else {
            "selectNode"
        };
        webidl::operation(vm, prototype.clone(), name, 1, move |vm, receiver, args| {
            let range = range(vm, &receiver)?;
            webidl::required(args, 1, name)?;
            let node = node(&owner, &args[0])?;
            let state = owner.borrow();
            let doc = &state.document;
            let (container, start, end) = if contents {
                if matches!(doc.nodes[node.0].kind, NodeKind::Doctype { .. }) {
                    return Err(errors.exception(
                        vm,
                        "InvalidNodeTypeError",
                        "A doctype cannot contain a range",
                    ));
                }
                (node, 0, node_length(vm, doc, node)?)
            } else {
                let parent = doc.nodes[node.0].parent.ok_or_else(|| {
                    errors.exception(vm, "InvalidNodeTypeError", "Range node has no parent")
                })?;
                let index = child_offset(vm, doc, parent, node)?;
                (parent, index, index + 1)
            };
            range.0.set(Bounds {
                start: Point {
                    node: container,
                    offset: start,
                },
                end: Point {
                    node: container,
                    offset: end,
                },
            });
            Ok(Value::Undefined)
        })?;
    }
    let owner = host.clone();
    let compare_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "compareBoundaryPoints",
        2,
        move |vm, receiver, args| {
            let bounds = range(vm, &receiver)?.0.get();
            webidl::required(args, 2, "Range.compareBoundaryPoints")?;
            let how = vm.to_number(&args[0])?.trunc().rem_euclid(65536.0) as u16;
            let source = range(vm, &args[1])?.0.get();
            let (a, b) = match how {
                0 => (bounds.start, source.start),
                1 => (bounds.end, source.start),
                2 => (bounds.end, source.end),
                3 => (bounds.start, source.end),
                _ => {
                    return Err(compare_errors.exception(
                        vm,
                        "NotSupportedError",
                        "Unknown range comparison mode",
                    ));
                }
            };
            let doc = &owner.borrow().document;
            match compare(vm, doc, a, b)? {
                Some(Ordering::Less) => Ok((-1).into()),
                Some(Ordering::Equal) => Ok(0.into()),
                Some(Ordering::Greater) => Ok(1.into()),
                None => Err(compare_errors.exception(
                    vm,
                    "WrongDocumentError",
                    "Range boundaries have different roots",
                )),
            }
        },
    )?;
    let owner = host.clone();
    let fragment_errors = errors.clone();
    webidl::operation(
        vm,
        prototype,
        "createContextualFragment",
        1,
        move |vm, receiver, args| {
            let start = range(vm, &receiver)?.0.get().start.node;
            webidl::required(args, 1, "Range.createContextualFragment")?;
            let source = text(vm, args[0].clone())?;
            let context = {
                let state = owner.borrow();
                match &state.document.nodes[start.0].kind {
                    NodeKind::Element { .. } => start,
                    _ => state.document.nodes[start.0].parent.unwrap_or(start),
                }
            };
            dom_api::contextual_fragment(vm, &owner, context, &source, &fragment_errors)
        },
    )?;
    Ok(())
}
