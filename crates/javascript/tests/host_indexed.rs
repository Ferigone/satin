use rust_browser_javascript::{ErrorKind, HostIndexedProperties, JsError, Value, Vm, VmLimits};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

struct Indexed {
    values: RefCell<Vec<i32>>,
    reads: Cell<usize>,
    collection_deferred: Cell<bool>,
}
impl Indexed {
    fn new(values: Vec<i32>) -> Rc<Self> {
        Rc::new(Self {
            values: RefCell::new(values),
            reads: Cell::new(0),
            collection_deferred: Cell::new(false),
        })
    }
}
impl HostIndexedProperties for Indexed {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.values.borrow().len())
    }

    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        self.reads.set(self.reads.get() + 1);
        self.collection_deferred
            .set(vm.collect_garbage().skipped_active_execution);
        Ok(self
            .values
            .borrow()
            .get(index as usize)
            .copied()
            .map(Value::from))
    }
}

#[test]
fn indexed_lookup_and_own_keys_observe_current_host_values() {
    let mut vm = Vm::new();
    let live = vm.new_object();
    let provider = Indexed::new(vec![5, 8]);
    vm.set_host_indexed_properties(live.clone(), provider.clone())
        .unwrap();
    vm.set_global("live", live.clone()).unwrap();
    vm.eval("var key=Symbol('extra');live.extra=1;live['01']=2;live[key]=3;")
        .unwrap();

    assert_eq!(
        vm.eval(
            r#"
            var child=Object.create(live), seen=[];
            for (var name in child) seen.push(name);
            var keys=Reflect.ownKeys(live);
            [0 in child, 2 in live, Object.hasOwn(live, 0), child.hasOwnProperty(0),
             live.propertyIsEnumerable(0), Object.keys(live).join(','),
             seen.join(','), keys.length, keys[4]===key].join('|');
        "#
        )
        .unwrap(),
        Value::from("true|false|true|false|true|0,1,extra,01|0,1,extra,01|5|true")
    );
    assert_eq!(provider.reads.get(), 0);
    assert_eq!(vm.get_property(live.clone(), "0").unwrap(), Value::from(5));
    assert!(provider.collection_deferred.get());
    assert_eq!(
        vm.eval(
            r#"
            var desc=Object.getOwnPropertyDescriptor(live,'1');
            [desc.value,desc.writable,desc.enumerable,desc.configurable,
             Reflect.getOwnPropertyDescriptor(live,'0').value,Reflect.get(child,'1')].join(',');
        "#
        )
        .unwrap(),
        Value::from("8,false,true,true,5,8")
    );
    assert_eq!(provider.reads.get(), 4);
    provider.values.replace(vec![13]);
    assert_eq!(
        vm.eval(
            r#"
            var copy={...live}, assigned=Object.assign({},live);
            [live[0],child[0],live[1],1 in live,Object.keys(live).join(','),
             copy[0],assigned[0],copy[key],Object.entries(live)[0].join(':')].join('|');
        "#
        )
        .unwrap(),
        Value::from("13|13||false|0,extra,01|13|13|3|0:13")
    );
}

#[test]
fn mutation_cannot_shadow_live_indices_or_make_the_wrapper_nonextensible() {
    let mut vm = Vm::new();
    let live = vm.new_object();
    let provider = Indexed::new(vec![5]);
    vm.set_host_indexed_properties(live.clone(), provider.clone())
        .unwrap();
    vm.set_global("live", live).unwrap();

    assert_eq!(
        vm.eval(
            r#"
            live[0]=1;live[7]=2;
            var child=Object.create(live);
            [delete live[0],delete live[7],Reflect.deleteProperty(live,'0'),
             Reflect.defineProperty(live,'7',{value:7}),Reflect.set(live,'0',9),
             Reflect.set(live,'0',9,child),Reflect.set(live,'7',9),
             Reflect.preventExtensions(live),Object.isExtensible(live),
             Object.hasOwn(live,'7')].join(',');
        "#
        )
        .unwrap(),
        Value::from("false,true,false,false,false,false,false,false,true,false")
    );
    assert_eq!(provider.reads.get(), 0);
    for source in [
        "'use strict'; live[0]=1;",
        "'use strict'; live[7]=1;",
        "'use strict'; delete live[0];",
        "Object.defineProperty(live,'0',{value:5,writable:false,enumerable:true,configurable:true});",
        "Object.preventExtensions(live);",
        "Object.seal(live);",
        "Object.freeze(live);",
    ] {
        assert_eq!(
            vm.eval(source).unwrap_err().kind,
            ErrorKind::TypeError,
            "{source}"
        );
    }
    assert_eq!(
        vm.eval(
            r#"
            var hits=0,p=Object.create(Object.prototype,{7:{get(){return 70;},set(v){hits=v;}}});
            Object.setPrototypeOf(live,p);
            live[7]=19;live['01']='ordinary';
            [live[0],live[7],hits,live['01'],Object.isExtensible(live)].join(',');
        "#
        )
        .unwrap(),
        Value::from("5,70,19,ordinary,true")
    );
    provider.values.replace(vec![2, 3]);
    assert_eq!(vm.eval("live[1];").unwrap(), Value::from(3));
}

#[test]
fn host_indices_use_key_budgets_and_the_provider_dies_with_the_wrapper() {
    let mut vm = Vm::with_limits(VmLimits {
        max_array_length: 4,
        ..VmLimits::default()
    });
    let live = vm.new_object();
    let provider = Indexed::new(vec![0; 5]);
    let weak = Rc::downgrade(&provider);
    vm.set_host_indexed_properties(live.clone(), provider.clone())
        .unwrap();
    vm.set_global("live", live).unwrap();
    assert_eq!(
        vm.eval("Object.keys(live);").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(
        vm.eval("'4' in live;").unwrap_err().kind,
        ErrorKind::ExecutionLimit
    );
    assert_eq!(provider.reads.get(), 0);
    provider.values.replace(vec![0; 4]);
    assert_eq!(
        vm.eval("Object.keys(live).length;").unwrap(),
        Value::from(4)
    );
    drop(provider);
    vm.collect_garbage();
    assert!(weak.upgrade().is_some());
    vm.eval("live=undefined;").unwrap();
    vm.collect_garbage();
    assert!(weak.upgrade().is_none());
}
