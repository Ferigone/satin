use super::{Exceptions, Shared, context_node, illegal, matrix_finite, numbers};
use crate::script::{arg, webidl};
use crate::svg::SvgTransform;
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, rc::Rc};

const MEMBERS: [(&str, &str, f64); 6] = [
    ("a", "m11", 1.),
    ("b", "m12", 0.),
    ("c", "m21", 0.),
    ("d", "m22", 1.),
    ("e", "m41", 0.),
    ("f", "m42", 0.),
];

fn components(m: SvgTransform) -> [f64; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f]
}

fn from_components([a, b, c, d, e, f]: [f64; 6]) -> SvgTransform {
    SvgTransform { a, b, c, d, e, f }
}

pub(super) fn from_dict(vm: &mut Vm, value: Value) -> Result<SvgTransform, JsError> {
    if value.is_nullish() {
        return Ok(SvgTransform::IDENTITY);
    }
    if value.as_object().is_none() {
        return Err(JsError::type_error("Expected DOMMatrix2DInit dictionary"));
    }
    let member = |vm: &mut Vm, key| {
        let number = vm.get_property(value.clone(), key)?;
        if number.is_undefined() {
            Ok(None)
        } else {
            vm.to_number(&number).map(Some)
        }
    };
    let mut aliases = [None; 6];
    // The IDL conversion visits the six aliases first, then the mXY names.
    for (slot, (alias, _, _)) in aliases.iter_mut().zip(MEMBERS) {
        *slot = member(vm, alias)?;
    }
    let mut standard = [None; 6];
    for (slot, (_, name, _)) in standard.iter_mut().zip(MEMBERS) {
        *slot = member(vm, name)?;
    }
    let mut output = [0.; 6];
    for (i, (_, _, default)) in MEMBERS.into_iter().enumerate() {
        if let (Some(a), Some(b)) = (aliases[i], standard[i])
            && a != b
            && !(a.is_nan() && b.is_nan())
        {
            return Err(JsError::type_error(
                "Conflicting DOMMatrix2DInit components",
            ));
        }
        output[i] = standard[i].or(aliases[i]).unwrap_or(default);
    }
    Ok(from_components(output))
}

fn rotate(radians: f64) -> SvgTransform {
    let (sin, cos) = radians.sin_cos();
    SvgTransform {
        a: cos,
        b: sin,
        c: -sin,
        d: cos,
        ..SvgTransform::IDENTITY
    }
}

struct Matrix(Cell<SvgTransform>);

fn matrix_object(vm: &mut Vm, prototype: Value, matrix: SvgTransform) -> Result<Value, JsError> {
    let result = vm.new_object_with_prototype(prototype)?;
    vm.set_host_data(result.clone(), Rc::new(Matrix(Cell::new(matrix))))?;
    Ok(result)
}

fn parse_matrix(vm: &mut Vm, input: Value, errors: Exceptions) -> Result<SvgTransform, JsError> {
    if input.is_undefined() {
        return Ok(SvgTransform::IDENTITY);
    }
    if input.as_object().is_some() {
        let iterator = webidl::iterator_method(vm, &input)?;
        let values = webidl::sequence(vm, input, iterator, 32, |vm, value| vm.to_number(&value))?;
        let values: [f64; 6] = values
            .try_into()
            .map_err(|_| JsError::type_error("Expected six DOMMatrix components"))?;
        return Ok(from_components(values));
    }
    let string = crate::script::text(vm, input)?;
    let string = string.trim();
    if string.is_empty() || string == "none" {
        return Ok(SvgTransform::IDENTITY);
    }
    let parts = string
        .strip_prefix("matrix(")
        .and_then(|s| s.strip_suffix(')'));
    if let Some(parts) = parts {
        let mut output = [0.; 6];
        let mut parts = parts.split(',');
        for component in &mut output {
            let Some(number) = parts
                .next()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
            else {
                return Err(errors.error(vm, "SyntaxError", "Invalid DOMMatrix transform"));
            };
            *component = number;
        }
        if parts.next().is_none() {
            return Ok(from_components(output));
        }
    }
    Err(errors.error(vm, "SyntaxError", "Expected a 2D CSS matrix transform"))
}

fn install_dom_matrix(vm: &mut Vm, errors: Exceptions) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    let intrinsic = prototype.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "DOMMatrix",
        0,
        move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("DOMMatrix constructor requires new"))?;
            let matrix = parse_matrix(vm, arg(args, 0), errors)?;
            let object = webidl::instance(vm, target, &intrinsic)?;
            vm.set_host_data(object.clone(), Rc::new(Matrix(Cell::new(matrix))))?;
            Ok(object)
        },
    )?;
    let intrinsic = prototype.clone();
    webidl::operation(vm, constructor, "fromMatrix", 0, move |vm, _, args| {
        let matrix = from_dict(vm, arg(args, 0))?;
        matrix_object(vm, intrinsic.clone(), matrix)
    })?;
    for (index, (alias, name, _)) in MEMBERS.into_iter().enumerate() {
        for prop in [alias, name] {
            let get = vm.new_native_function(&format!("get {prop}"), move |vm, this, _| {
                let matrix = vm
                    .host_data::<Matrix>(&this)
                    .ok_or_else(|| illegal("DOMMatrix"))?;
                Ok(components(matrix.0.get())[index].into())
            });
            let set = vm.new_native_function(&format!("set {prop}"), move |vm, this, args| {
                let matrix = vm
                    .host_data::<Matrix>(&this)
                    .ok_or_else(|| illegal("DOMMatrix"))?;
                let value = vm.to_number(&arg(args, 0))?;
                let mut parts = components(matrix.0.get());
                parts[index] = value;
                matrix.0.set(from_components(parts));
                Ok(Value::Undefined)
            });
            vm.define_accessor(prototype.clone(), prop, Some(get), Some(set))?;
        }
    }
    let three_d_members = [
        "m13", "m14", "m23", "m24", "m31", "m32", "m33", "m34", "m43", "m44",
    ];
    for name in three_d_members {
        crate::script::getter(vm, prototype.clone(), name, move |vm, this, _| {
            vm.host_data::<Matrix>(&this)
                .ok_or_else(|| illegal("DOMMatrix"))?;
            Ok(if matches!(name, "m33" | "m44") {
                1.into()
            } else {
                0.into()
            })
        })?;
    }
    for name in ["is2D", "isIdentity"] {
        crate::script::getter(vm, prototype.clone(), name, move |vm, this, _| {
            let matrix = vm
                .host_data::<Matrix>(&this)
                .ok_or_else(|| illegal("DOMMatrix"))?;
            Ok((name == "is2D" || matrix.0.get() == SvgTransform::IDENTITY).into())
        })?;
    }
    webidl::operation(vm, prototype.clone(), "toString", 0, move |vm, this, _| {
        let matrix = vm
            .host_data::<Matrix>(&this)
            .ok_or_else(|| illegal("DOMMatrix"))?
            .0
            .get();
        if !matrix_finite(matrix) {
            return Err(errors.error(
                vm,
                "InvalidStateError",
                "Cannot serialize a non-finite matrix",
            ));
        }
        let components = components(matrix).map(|n| n.to_string()).join(", ");
        Ok(format!("matrix({components})").into())
    })?;
    for (name, operation) in [
        ("translate", MatrixOp::Translate),
        ("scale", MatrixOp::Scale),
        ("rotate", MatrixOp::Rotate),
        ("multiply", MatrixOp::Multiply),
        ("inverse", MatrixOp::Inverse),
    ] {
        for mutable in [false, true] {
            let method = if mutable {
                format!("{name}Self")
            } else {
                name.to_owned()
            };
            let intrinsic = prototype.clone();
            webidl::operation(vm, prototype.clone(), &method, 0, move |vm, this, args| {
                let matrix = vm
                    .host_data::<Matrix>(&this)
                    .ok_or_else(|| illegal("DOMMatrix"))?;
                let base = matrix.0.get();
                let next = operation.convert(vm, base, args)?;
                if mutable {
                    matrix.0.set(next);
                    Ok(this)
                } else {
                    matrix_object(vm, intrinsic.clone(), next)
                }
            })?;
        }
    }
    Ok(prototype)
}

#[derive(Clone, Copy)]
enum MatrixOp {
    Translate,
    Scale,
    Rotate,
    Multiply,
    Inverse,
}

fn optional_number(vm: &mut Vm, args: &[Value], i: usize, default: f64) -> Result<f64, JsError> {
    let value = arg(args, i);
    if value.is_undefined() {
        Ok(default)
    } else {
        vm.to_number(&value)
    }
}

impl MatrixOp {
    fn convert(
        self,
        vm: &mut Vm,
        base: SvgTransform,
        args: &[Value],
    ) -> Result<SvgTransform, JsError> {
        let matrix = match self {
            Self::Translate => SvgTransform::translate(
                optional_number(vm, args, 0, 0.)?,
                optional_number(vm, args, 1, 0.)?,
            ),
            Self::Scale => {
                let x = optional_number(vm, args, 0, 1.)?;
                SvgTransform::scale(x, optional_number(vm, args, 1, x)?)
            }
            Self::Rotate => rotate(optional_number(vm, args, 0, 0.)?.to_radians()),
            Self::Multiply => from_dict(vm, arg(args, 0))?,
            Self::Inverse => {
                return Ok(super::inverse(base).unwrap_or(from_components([f64::NAN; 6])));
            }
        };
        Ok(base.multiply(matrix))
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Scale,
    Rotate,
    Translate,
    Multiply,
    Set,
    Reset,
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    prototype: &Value,
    errors: Exceptions,
) -> Result<(), JsError> {
    let matrix_prototype = install_dom_matrix(vm, errors)?;
    vm.root(matrix_prototype.clone());
    let host = state.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "getTransform",
        0,
        move |vm, this, _| {
            let id = context_node(&host, &this)?;
            let matrix = host.borrow().canvases.entries[&id].drawing.transform();
            matrix_object(vm, matrix_prototype.clone(), matrix)
        },
    )?;
    for (name, arity, op) in [
        ("scale", 2, Operation::Scale),
        ("rotate", 1, Operation::Rotate),
        ("translate", 2, Operation::Translate),
        ("transform", 6, Operation::Multiply),
        ("setTransform", 0, Operation::Set),
        ("resetTransform", 0, Operation::Reset),
    ] {
        let host = state.clone();
        webidl::operation(vm, prototype.clone(), name, arity, move |vm, this, args| {
            let id = context_node(&host, &this)?;
            webidl::required(args, arity as usize, name)?;
            let matrix = match op {
                Operation::Scale => {
                    let [x, y] = numbers::<2>(vm, args, 0)?;
                    SvgTransform::scale(x, y)
                }
                Operation::Translate => {
                    let [x, y] = numbers::<2>(vm, args, 0)?;
                    SvgTransform::translate(x, y)
                }
                Operation::Rotate => rotate(numbers::<1>(vm, args, 0)?[0]),
                Operation::Multiply => from_components(numbers::<6>(vm, args, 0)?),
                Operation::Set if args.len() >= 6 => from_components(numbers::<6>(vm, args, 0)?),
                Operation::Set if args.len() <= 1 => from_dict(vm, arg(args, 0))?,
                Operation::Set => {
                    return Err(JsError::type_error(
                        "setTransform expects a matrix or six numbers",
                    ));
                }
                Operation::Reset => SvgTransform::IDENTITY,
            };
            if matrix_finite(matrix) {
                let mut state = host.borrow_mut();
                let current = &mut state
                    .canvases
                    .entries
                    .get_mut(&id)
                    .unwrap()
                    .drawing
                    .state
                    .matrix;
                let result = if matches!(op, Operation::Set | Operation::Reset) {
                    matrix
                } else {
                    current.multiply(matrix)
                };
                if matrix_finite(result) {
                    *current = result;
                }
            }
            Ok(Value::Undefined)
        })?;
    }
    Ok(())
}
