//! Branded FileList objects with VM-traced, immutable arrays of File references.
use super::{blobs::Blob, getter, webidl};
use rust_browser_javascript::{
    ErrorKind, HostIndexedProperties, JsError, JsString, ObjectId, PlatformClone, RootId, Value, Vm,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(super) const MAX_SELECTED_FILES: usize = 256;
const FILES_SLOT: u32 = 0;

pub(super) struct FileList {
    owner: ObjectId,
    length: Cell<usize>,
    first_name: RefCell<Option<JsString>>,
}

impl FileList {
    pub(super) fn get(vm: &Vm, object: &Value) -> Option<Rc<Self>> {
        vm.host_data(object)
    }

    pub(super) fn len(&self) -> usize {
        self.length.get()
    }

    pub(super) fn first_name(&self) -> Option<JsString> {
        self.first_name.borrow().clone()
    }

    fn storage(&self, vm: &Vm) -> Option<Value> {
        vm.host_slot(&Value::Object(self.owner), FILES_SLOT)
            .filter(|value| !value.is_undefined())
    }

    pub(super) fn at(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        if index as usize >= self.len() {
            return Ok(None);
        }
        let storage = self.storage(vm).expect("nonempty FileList storage");
        vm.get_property(storage, &index.to_string()).map(Some)
    }

    pub(super) fn clear(&self, vm: &mut Vm) -> Result<(), JsError> {
        if self.len() != 0 {
            vm.set_host_slot(Value::Object(self.owner), FILES_SLOT, Value::Undefined)?;
            self.length.set(0);
            self.first_name.borrow_mut().take();
        }
        Ok(())
    }
}

impl HostIndexedProperties for FileList {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.len())
    }

    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        self.at(vm, index)
    }
}

#[derive(Clone)]
pub(super) struct FileListHost {
    prototype: RootId,
}

impl FileListHost {
    pub(super) fn install(vm: &mut Vm) -> Result<Self, JsError> {
        let prototype = vm.new_object();
        webidl::interface(vm, prototype.clone(), "FileList", 0, |_, _, _, _| {
            Err(JsError::type_error("Illegal FileList constructor"))
        })?;
        getter(vm, prototype.clone(), "length", |vm, this, _| {
            Ok((receiver(vm, &this)?.len() as u32).into())
        })?;
        webidl::operation(vm, prototype.clone(), "item", 1, |vm, this, args| {
            let files = receiver(vm, &this)?;
            webidl::required(args, 1, "FileList.item")?;
            let index = vm.to_number(&args[0])?.trunc().rem_euclid(4_294_967_296.) as u32;
            Ok(files.at(vm, index)?.unwrap_or(Value::Null))
        })?;
        let array = vm.get_global("Array")?;
        let array_prototype = vm.get_property(array, "prototype")?;
        let values = vm.get_property(array_prototype, "values")?;
        vm.define_data_property_key(
            prototype.clone(),
            webidl::well_known_key(vm, "iterator"),
            values,
            true,
            false,
            true,
        )?;
        Ok(Self {
            prototype: vm.root(prototype),
        })
    }

    fn allocate(
        &self,
        vm: &mut Vm,
        length: usize,
        first_name: Option<JsString>,
    ) -> Result<Value, JsError> {
        let prototype = vm
            .root_value(self.prototype)
            .expect("rooted FileList prototype");
        let object = vm.new_object_with_prototype(prototype)?;
        let files = Rc::new(FileList {
            owner: object.as_object().unwrap(),
            length: Cell::new(length),
            first_name: RefCell::new(first_name),
        });
        vm.mark_platform_object(&object)?;
        vm.set_host_data(object.clone(), files.clone())?;
        vm.set_host_indexed_properties(object.clone(), files)?;
        Ok(object)
    }

    pub(super) fn create(&self, vm: &mut Vm, files: &[Value]) -> Result<Value, JsError> {
        if files.len() > MAX_SELECTED_FILES {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "FileList item limit exceeded",
            ));
        }
        vm.charge_work(files.len())?;
        let mut first_name = None;
        for value in files {
            let blob = vm.host_data::<Blob>(value).ok_or_else(not_file)?;
            let file = blob.file.as_ref().ok_or_else(not_file)?;
            if first_name.is_none() {
                vm.charge_work(file.name.len())?;
                first_name = Some(JsString::from(file.name.as_str()));
            }
        }
        let object = self.allocate(vm, files.len(), first_name)?;
        if !files.is_empty() {
            let storage = vm.try_new_array(files)?;
            vm.set_host_slot(object.clone(), FILES_SLOT, storage)?;
        }
        Ok(object)
    }

    pub(super) fn duplicate(&self, vm: &mut Vm, source: &FileList) -> Result<Value, JsError> {
        let object = self.allocate(vm, source.len(), source.first_name())?;
        if let Some(storage) = source.storage(vm) {
            vm.set_host_slot(object.clone(), FILES_SLOT, storage)?;
        }
        Ok(object)
    }

    pub(super) fn clone_file_list(
        &self,
        vm: &mut Vm,
        source: &Value,
    ) -> Result<Option<PlatformClone>, JsError> {
        let Some(source) = FileList::get(vm, source) else {
            return Ok(None);
        };
        let object = self.allocate(vm, source.len(), source.first_name())?;
        let slots = source
            .storage(vm)
            .map_or_else(Vec::new, |files| vec![(FILES_SLOT, files)]);
        Ok(Some(PlatformClone { object, slots }))
    }
}

fn receiver(vm: &Vm, value: &Value) -> Result<Rc<FileList>, JsError> {
    FileList::get(vm, value).ok_or_else(|| JsError::type_error("Illegal FileList receiver"))
}

fn not_file() -> JsError {
    JsError::type_error("FileList entries must be Files")
}
