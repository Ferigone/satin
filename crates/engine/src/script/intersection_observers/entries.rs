use crate::{
    dom::NodeId,
    layout::Rect,
    script::{Shared, arg, getter, webidl, wrap},
};
use rust_browser_javascript::{JsError, Value, Vm};
use std::rc::Rc;

pub(super) struct Entry {
    pub target: NodeId,
    pub time: f64,
    pub target_rect: Rect,
    pub root_rect: Rect,
    pub intersection_rect: Rect,
    pub ratio: f64,
    pub intersecting: bool,
}

type RectGetter = fn(&Entry) -> Rect;

#[derive(Clone)]
pub(super) struct EntryFactory(Value);

impl EntryFactory {
    pub fn install(vm: &mut Vm, state: &Shared) -> Result<Self, JsError> {
        let rect_prototype = install_rect(vm)?;
        let prototype = vm.new_object();
        webidl::interface(
            vm,
            prototype.clone(),
            "IntersectionObserverEntry",
            0,
            |_, _, _, _| {
                Err(JsError::type_error(
                    "Illegal IntersectionObserverEntry constructor",
                ))
            },
        )?;
        vm.root(prototype.clone());
        vm.root(rect_prototype.clone());
        let state = state.clone();
        getter(vm, prototype.clone(), "target", move |vm, this, _| {
            wrap(vm, &state, Some(entry(vm, &this)?.target))
        })?;
        getter(vm, prototype.clone(), "time", |vm, this, _| {
            Ok(entry(vm, &this)?.time.into())
        })?;
        getter(vm, prototype.clone(), "intersectionRatio", |vm, this, _| {
            Ok(entry(vm, &this)?.ratio.into())
        })?;
        getter(vm, prototype.clone(), "isIntersecting", |vm, this, _| {
            Ok(Value::Bool(entry(vm, &this)?.intersecting))
        })?;
        let rects: [(&str, RectGetter); 3] = [
            ("boundingClientRect", |e| e.target_rect),
            ("rootBounds", |e| e.root_rect),
            ("intersectionRect", |e| e.intersection_rect),
        ];
        for (name, read) in rects {
            let rect_prototype = rect_prototype.clone();
            getter(vm, prototype.clone(), name, move |vm, this, _| {
                let entry = entry(vm, &this)?;
                let Rect {
                    x,
                    y,
                    width,
                    height,
                } = read(&entry);
                let rect = vm.new_object_with_prototype(rect_prototype.clone())?;
                set_rect(vm, &rect, [x.into(), y.into(), width.into(), height.into()])?;
                Ok(rect)
            })?;
        }
        Ok(Self(prototype))
    }

    pub fn array(&self, vm: &mut Vm, entries: Vec<Entry>) -> Result<Value, JsError> {
        vm.charge_work(entries.len())?;
        let mut values = Vec::with_capacity(entries.len());
        for entry in entries {
            let object = vm.new_object_with_prototype(self.0.clone())?;
            vm.mark_platform_object(&object)?;
            vm.set_host_data(object.clone(), Rc::new(entry))?;
            values.push(object);
        }
        vm.try_new_array(&values)
    }
}

fn entry(vm: &Vm, this: &Value) -> Result<Rc<Entry>, JsError> {
    vm.host_data(this)
        .ok_or_else(|| JsError::type_error("Expected an IntersectionObserverEntry receiver"))
}

#[derive(Clone, Copy)]
struct ReadOnlyRect([f64; 4]);

const RECT_KEYS: [&str; 8] = [
    "x", "y", "width", "height", "top", "right", "bottom", "left",
];

impl ReadOnlyRect {
    fn value(self, index: usize) -> Value {
        let [x, y, width, height] = self.0;
        match index {
            4 if height < 0. => y + height,
            4 => y,
            5 if width < 0. => x,
            5 => x + width,
            6 if height < 0. => y,
            6 => y + height,
            7 if width < 0. => x + width,
            7 => x,
            _ => self.0[index],
        }
        .into()
    }
}

fn set_rect(vm: &mut Vm, object: &Value, coordinates: [f64; 4]) -> Result<(), JsError> {
    vm.mark_platform_object(object)?;
    vm.set_host_data(object.clone(), Rc::new(ReadOnlyRect(coordinates)))
}

fn rect(vm: &Vm, this: &Value) -> Result<Rc<ReadOnlyRect>, JsError> {
    vm.host_data(this)
        .ok_or_else(|| JsError::type_error("Expected a DOMRectReadOnly receiver"))
}

fn install_rect(vm: &mut Vm) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    let intrinsic = prototype.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "DOMRectReadOnly",
        0,
        move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("DOMRectReadOnly requires new"))?;
            let mut coordinates = [0.; 4];
            for (coordinate, value) in coordinates.iter_mut().zip(args) {
                if !value.is_undefined() {
                    *coordinate = vm.to_number(value)?;
                }
            }
            let object = webidl::instance(vm, target, &intrinsic)?;
            set_rect(vm, &object, coordinates)?;
            Ok(object)
        },
    )?;
    let intrinsic = prototype.clone();
    webidl::operation(vm, constructor, "fromRect", 0, move |vm, _, args| {
        let value = arg(args, 0);
        let mut coordinates = [0.; 4];
        if !value.is_nullish() {
            if value.as_object().is_none() {
                return Err(JsError::type_error("Expected DOMRectInit options"));
            }
            for index in [3, 2, 0, 1] {
                let member = vm.get_property(value.clone(), RECT_KEYS[index])?;
                if !member.is_undefined() {
                    coordinates[index] = vm.to_number(&member)?;
                }
            }
        }
        let object = vm.new_object_with_prototype(intrinsic.clone())?;
        set_rect(vm, &object, coordinates)?;
        Ok(object)
    })?;
    for (index, name) in RECT_KEYS.into_iter().enumerate() {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            Ok(rect(vm, &this)?.value(index))
        })?;
    }
    webidl::operation(vm, prototype.clone(), "toJSON", 0, |vm, this, _| {
        let rect = rect(vm, &this)?;
        let result = vm.new_object();
        for (index, name) in RECT_KEYS.into_iter().enumerate() {
            vm.define_data_property(result.clone(), name, rect.value(index), true, true, true)?;
        }
        Ok(result)
    })?;
    Ok(prototype)
}
