//! User Timing entries and their per-window timeline.
use super::{NavigationTiming, clock};
use crate::script::{abort::dom_exception_error, arg, getter, webidl};
use rust_browser_javascript::{JsError, JsString, RootId, Value, Vm};
use std::{cell::RefCell, cmp::Ordering, collections::VecDeque, rc::Rc, time::Instant};

const ENTRY_LIMIT: usize = 1024;
const NAME: u32 = 0;
const DETAIL: u32 = 1;
const FIELDS: [&str; 4] = ["name", "entryType", "startTime", "duration"];
const LEGACY_TIMING_NAMES: [&str; 21] = [
    "navigationStart",
    "unloadEventStart",
    "unloadEventEnd",
    "redirectStart",
    "redirectEnd",
    "fetchStart",
    "domainLookupStart",
    "domainLookupEnd",
    "connectStart",
    "connectEnd",
    "secureConnectionStart",
    "requestStart",
    "responseStart",
    "responseEnd",
    "domLoading",
    "domInteractive",
    "domContentLoadedEventStart",
    "domContentLoadedEventEnd",
    "domComplete",
    "loadEventStart",
    "loadEventEnd",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mark,
    Measure,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Mark => "mark",
            Self::Measure => "measure",
        }
    }
}

struct Entry {
    kind: Kind,
    name: JsString,
    start: f64,
    duration: f64,
}

impl Entry {
    fn by_start(&self, other: &Self) -> Ordering {
        self.start
            .partial_cmp(&other.start)
            .expect("finite start time")
    }

    fn field(&self, name: &str) -> Value {
        match name {
            "name" => self.name.clone().into(),
            "entryType" => self.kind.name().into(),
            "startTime" => self.start.into(),
            "duration" => self.duration.into(),
            _ => unreachable!("registered PerformanceEntry field"),
        }
    }
}

struct BufferedEntry {
    data: Rc<Entry>,
    object: RootId,
}

struct Timeline {
    entries: RefCell<VecDeque<BufferedEntry>>,
    timing: Rc<NavigationTiming>,
    clone_detail: RootId,
    exception: RootId,
}

fn brand(vm: &Vm, receiver: &Value) -> Result<Rc<Entry>, JsError> {
    vm.host_data(receiver)
        .ok_or_else(|| JsError::type_error("Illegal PerformanceEntry receiver"))
}

fn matches_ascii(input: &JsString, expected: &str) -> bool {
    input.as_utf16().iter().copied().eq(expected.encode_utf16())
}

fn legacy_name(name: &JsString) -> Option<&'static str> {
    LEGACY_TIMING_NAMES
        .into_iter()
        .find(|legacy| matches_ascii(name, legacy))
}

fn string(vm: &mut Vm, value: &Value) -> Result<JsString, JsError> {
    let name = vm.to_string(value)?;
    vm.charge_work(name.len())?;
    Ok(name)
}

fn finite(vm: &mut Vm, value: &Value) -> Result<f64, JsError> {
    let value = vm.to_number(value)?;
    if !value.is_finite() {
        return Err(JsError::type_error("Expected a finite timestamp"));
    }
    Ok(value)
}

fn nonnegative(value: f64) -> Result<f64, JsError> {
    if value < 0. {
        return Err(JsError::type_error("Negative timestamp"));
    }
    Ok(value)
}

enum Offset {
    Name(JsString),
    Time(f64),
}

fn offset(vm: &mut Vm, value: Value) -> Result<Option<Offset>, JsError> {
    match value {
        Value::Undefined => Ok(None),
        Value::Number(_) => finite(vm, &value).map(|value| Some(Offset::Time(value))),
        _ => string(vm, &value).map(|name| Some(Offset::Name(name))),
    }
}

#[derive(Default)]
struct MeasureOptions {
    detail: Value,
    duration: Option<f64>,
    end: Option<Offset>,
    start: Option<Offset>,
}

impl MeasureOptions {
    fn from_object(vm: &mut Vm, options: Value) -> Result<Self, JsError> {
        let detail = vm.get_property(options.clone(), "detail")?;
        let duration = vm.get_property(options.clone(), "duration")?;
        let duration = (!duration.is_undefined())
            .then(|| finite(vm, &duration))
            .transpose()?;
        let end = vm.get_property(options.clone(), "end")?;
        let end = offset(vm, end)?;
        let start = vm.get_property(options, "start")?;
        let start = offset(vm, start)?;
        Ok(Self {
            detail,
            duration,
            end,
            start,
        })
    }

    fn validate(&self, has_end_mark: bool) -> Result<(), JsError> {
        let has_boundary = self.start.is_some() || self.end.is_some();
        let has_options = has_boundary || self.duration.is_some() || !self.detail.is_undefined();
        let all_boundaries = self.start.is_some() && self.end.is_some() && self.duration.is_some();
        if has_options && (has_end_mark || !has_boundary || all_boundaries) {
            return Err(JsError::type_error("Invalid PerformanceMeasureOptions"));
        }
        Ok(())
    }
}

impl Timeline {
    fn error(&self, vm: &mut Vm, name: &str, message: &str) -> JsError {
        let class = vm.root_value(self.exception).expect("rooted DOMException");
        dom_exception_error(vm, class, name, message).unwrap_or_else(|error| error)
    }

    fn attach(
        &self,
        vm: &mut Vm,
        object: &Value,
        entry: &Rc<Entry>,
        detail: Value,
    ) -> Result<(), JsError> {
        if !detail.is_nullish() {
            let clone = vm
                .root_value(self.clone_detail)
                .expect("rooted structuredClone");
            let detail = vm.call(clone, Value::Undefined, &[detail])?;
            vm.set_host_slot(object.clone(), DETAIL, detail)?;
        }
        // The VM accounts the entry's name as a retained JavaScript string.
        vm.set_host_slot(object.clone(), NAME, entry.name.clone().into())?;
        vm.set_host_data(object.clone(), entry.clone())
    }

    fn buffer(&self, vm: &mut Vm, object: Value, data: Rc<Entry>) -> Value {
        let mut entries = self.entries.borrow_mut();
        if entries.len() == ENTRY_LIMIT {
            let oldest = entries.pop_front().expect("full timing timeline");
            vm.unroot(oldest.object);
        }
        entries.push_back(BufferedEntry {
            data,
            object: vm.root(object.clone()),
        });
        object
    }

    fn mark(
        &self,
        vm: &mut Vm,
        intrinsic: Value,
        target: Option<Value>,
        args: &[Value],
    ) -> Result<(Value, Rc<Entry>), JsError> {
        webidl::required(args, 1, "PerformanceMark")?;
        let name = string(vm, &args[0])?;
        let options = arg(args, 1);
        let (detail, start) = if options.is_nullish() {
            (Value::Null, None)
        } else {
            if options.as_object().is_none() {
                return Err(JsError::type_error("Expected PerformanceMarkOptions"));
            }
            let detail = vm.get_property(options.clone(), "detail")?;
            let start = vm.get_property(options, "startTime")?;
            let start = (!start.is_undefined())
                .then(|| finite(vm, &start))
                .transpose()?;
            (detail, start)
        };
        if legacy_name(&name).is_some() {
            return Err(self.error(
                vm,
                "SyntaxError",
                "Mark name is reserved for navigation timing",
            ));
        }
        let start = match start {
            Some(value) => nonnegative(value)?,
            None => self.timing.clock.now(Instant::now()),
        };
        let object = match target {
            Some(target) => webidl::instance(vm, target, &intrinsic)?,
            None => {
                let object = vm.new_object_with_prototype(intrinsic)?;
                vm.mark_platform_object(&object)?;
                object
            }
        };
        let entry = Rc::new(Entry {
            kind: Kind::Mark,
            name,
            start,
            duration: 0.,
        });
        self.attach(vm, &object, &entry, detail)?;
        Ok((object, entry))
    }

    fn timestamp(&self, vm: &mut Vm, offset: Offset) -> Result<f64, JsError> {
        let name = match offset {
            Offset::Time(time) => return nonnegative(time),
            Offset::Name(name) => name,
        };
        if let Some(legacy) = legacy_name(&name) {
            if legacy == "navigationStart" {
                return Ok(0.);
            }
            let time = self.timing.timestamp(legacy);
            if time == 0. {
                return Err(self.error(
                    vm,
                    "InvalidAccessError",
                    "Navigation timing value is unavailable",
                ));
            }
            return Ok(time - self.timing.timestamp("navigationStart"));
        }
        let timestamp = {
            let entries = self.entries.borrow();
            vm.charge_work(entries.len().saturating_mul(name.len().max(1)))?;
            entries
                .iter()
                .map(|entry| &entry.data)
                .filter(|entry| entry.kind == Kind::Mark && entry.name == name)
                .max_by(|a, b| a.by_start(b))
                .map(|entry| entry.start)
        };
        timestamp.ok_or_else(|| self.error(vm, "SyntaxError", "Performance mark was not found"))
    }

    fn measure(&self, vm: &mut Vm, prototype: Value, args: &[Value]) -> Result<Value, JsError> {
        webidl::required(args, 1, "Performance.measure")?;
        let name = string(vm, &args[0])?;
        let options = arg(args, 1);
        let (options, legacy_start) = if options.as_object().is_some() {
            (MeasureOptions::from_object(vm, options)?, None)
        } else if options.is_nullish() {
            (MeasureOptions::default(), None)
        } else {
            let start = Offset::Name(string(vm, &options)?);
            (MeasureOptions::default(), Some(start))
        };
        let legacy_end = arg(args, 2);
        let legacy_end = (!legacy_end.is_undefined())
            .then(|| string(vm, &legacy_end).map(Offset::Name))
            .transpose()?;
        options.validate(legacy_end.is_some())?;
        let start = options.start.or(legacy_start);
        let end = options.end.or(legacy_end);
        let (start, end) = match (start, end, options.duration) {
            (Some(start), None, Some(duration)) => {
                let start = self.timestamp(vm, start)?;
                (start, start + nonnegative(duration)?)
            }
            (None, Some(end), Some(duration)) => {
                let end = self.timestamp(vm, end)?;
                (end - nonnegative(duration)?, end)
            }
            (start, end, None) => {
                let end = match end {
                    Some(end) => self.timestamp(vm, end)?,
                    None => self.timing.clock.now(Instant::now()),
                };
                let start = start
                    .map(|start| self.timestamp(vm, start))
                    .transpose()?
                    .unwrap_or(0.);
                (start, end)
            }
            _ => unreachable!("measure options already validated"),
        };
        let entry = Rc::new(Entry {
            kind: Kind::Measure,
            name,
            start,
            duration: end - start,
        });
        let object = vm.new_object_with_prototype(prototype)?;
        vm.mark_platform_object(&object)?;
        self.attach(vm, &object, &entry, options.detail)?;
        Ok(self.buffer(vm, object, entry))
    }

    fn clear(&self, vm: &mut Vm, kind: Kind, name: Option<JsString>) -> Result<Value, JsError> {
        let mut entries = self.entries.borrow_mut();
        vm.charge_work(
            entries
                .len()
                .saturating_mul(name.as_ref().map_or(1, |name| name.len().max(1))),
        )?;
        entries.retain(|entry| {
            let remove = entry.data.kind == kind
                && name.as_ref().is_none_or(|name| &entry.data.name == name);
            if remove {
                vm.unroot(entry.object);
            }
            !remove
        });
        Ok(Value::Undefined)
    }

    fn query(
        &self,
        vm: &mut Vm,
        name: Option<JsString>,
        kind: Option<JsString>,
    ) -> Result<Value, JsError> {
        let entries = self.entries.borrow();
        let per_entry = 12 + name.as_ref().map_or(0, JsString::len);
        vm.charge_work(entries.len().saturating_mul(per_entry))?;
        let mut selected: Vec<_> = entries
            .iter()
            .filter(|entry| {
                name.as_ref().is_none_or(|name| &entry.data.name == name)
                    && kind
                        .as_ref()
                        .is_none_or(|kind| matches_ascii(kind, entry.data.kind.name()))
            })
            .collect();
        selected.sort_by(|a, b| a.data.by_start(&b.data));
        let objects: Vec<_> = selected
            .into_iter()
            .map(|entry| {
                vm.root_value(entry.object)
                    .expect("buffered timing entry is rooted")
            })
            .collect();
        vm.try_new_array(&objects)
    }
}

pub(super) fn install(
    vm: &mut Vm,
    performance: &Value,
    timing: Rc<NavigationTiming>,
) -> Result<(), JsError> {
    let clone = vm.get_global("structuredClone")?;
    let exception = vm.get_global("DOMException")?;
    let timeline = Rc::new(Timeline {
        timing,
        clone_detail: vm.root(clone),
        exception: vm.root(exception),
        entries: RefCell::default(),
    });
    let base = vm.new_object();
    let base_class = webidl::interface(vm, base.clone(), "PerformanceEntry", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal PerformanceEntry constructor"))
    })?;
    for name in FIELDS {
        getter(vm, base.clone(), name, move |vm, this, _| {
            Ok(brand(vm, &this)?.field(name))
        })?;
    }
    webidl::operation(vm, base.clone(), "toJSON", 0, |vm, this, _| {
        let entry = brand(vm, &this)?;
        let out = vm.new_object();
        for name in FIELDS {
            vm.define_data_property(out.clone(), name, entry.field(name), true, true, true)?;
        }
        Ok(out)
    })?;

    let mark = vm.new_object_with_prototype(base.clone())?;
    let mark_root = vm.root(mark.clone());
    let constructor_timeline = timeline.clone();
    let mark_class = webidl::interface(
        vm,
        mark.clone(),
        "PerformanceMark",
        1,
        move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error("PerformanceMark requires new"))?;
            let prototype = vm.root_value(mark_root).expect("rooted mark prototype");
            constructor_timeline
                .mark(vm, prototype, Some(target), args)
                .map(|(mark, _)| mark)
        },
    )?;
    let measure = vm.new_object_with_prototype(base)?;
    let measure_root = vm.root(measure.clone());
    let measure_class = webidl::interface(
        vm,
        measure.clone(),
        "PerformanceMeasure",
        0,
        |_, _, _, _| {
            Err(JsError::type_error(
                "Illegal PerformanceMeasure constructor",
            ))
        },
    )?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    for (kind, prototype, class) in [
        (Kind::Mark, mark, mark_class),
        (Kind::Measure, measure, measure_class),
    ] {
        vm.call(
            set_prototype.clone(),
            Value::Undefined,
            &[class, base_class.clone()],
        )?;
        getter(vm, prototype, "detail", move |vm, this, _| {
            if brand(vm, &this)?.kind != kind {
                return Err(JsError::type_error("Illegal user timing receiver"));
            }
            Ok(vm.host_slot(&this, DETAIL).unwrap_or(Value::Null))
        })?;
    }

    let marks = timeline.clone();
    webidl::operation(vm, performance.clone(), "mark", 1, move |vm, this, args| {
        clock(vm, &this)?;
        let prototype = vm.root_value(mark_root).expect("rooted mark prototype");
        let (object, entry) = marks.mark(vm, prototype, None, args)?;
        Ok(marks.buffer(vm, object, entry))
    })?;
    let measures = timeline.clone();
    webidl::operation(
        vm,
        performance.clone(),
        "measure",
        1,
        move |vm, this, args| {
            clock(vm, &this)?;
            let prototype = vm
                .root_value(measure_root)
                .expect("rooted measure prototype");
            measures.measure(vm, prototype, args)
        },
    )?;
    for (operation, kind) in [("clearMarks", Kind::Mark), ("clearMeasures", Kind::Measure)] {
        let timeline = timeline.clone();
        webidl::operation(
            vm,
            performance.clone(),
            operation,
            0,
            move |vm, this, args| {
                clock(vm, &this)?;
                let name = arg(args, 0);
                let name = (!name.is_undefined())
                    .then(|| string(vm, &name))
                    .transpose()?;
                timeline.clear(vm, kind, name)
            },
        )?;
    }

    for (operation, arity) in [
        ("getEntries", 0),
        ("getEntriesByName", 1),
        ("getEntriesByType", 1),
    ] {
        let timeline = timeline.clone();
        webidl::operation(
            vm,
            performance.clone(),
            operation,
            arity,
            move |vm, this, args| {
                clock(vm, &this)?;
                webidl::required(args, arity as usize, operation)?;
                let name = if operation == "getEntriesByName" {
                    Some(string(vm, &args[0])?)
                } else {
                    None
                };
                let kind = match operation {
                    "getEntriesByType" => Some(string(vm, &args[0])?),
                    "getEntriesByName" => args
                        .get(1)
                        .filter(|kind| !kind.is_undefined())
                        .map(|kind| string(vm, kind))
                        .transpose()?,
                    _ => None,
                };
                timeline.query(vm, name, kind)
            },
        )?;
    }
    Ok(())
}
