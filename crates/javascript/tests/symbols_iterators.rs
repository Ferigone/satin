use rust_browser_javascript::{ErrorKind, JsString, PropertyKey, Value, Vm, VmLimits};

fn yes(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn symbols_have_identity_registry_description_and_primitive_boxing() {
    yes(
        "var a=Symbol('x'),b=Symbol('x');typeof a==='symbol' && a!==b && a.description==='x' && Symbol().description===undefined && Symbol('').description==='' && Symbol.for('x')===Symbol.for('x') && Symbol.keyFor(a)===undefined && Symbol.keyFor(Symbol.for('x'))==='x' && Object(a).valueOf()===a && Object(a)==a && String(a)==='Symbol(x)'",
    );
    for source in [
        "new Symbol()",
        "Number(Symbol())",
        "''+Symbol()",
        "String(Object(Symbol()))",
        "Symbol.keyFor(Object(Symbol()))",
    ] {
        let mut vm = Vm::new();
        assert_eq!(
            vm.eval(source).unwrap_err().kind,
            ErrorKind::TypeError,
            "{source}"
        );
    }
}

#[test]
fn symbol_keys_are_distinct_preserve_order_and_support_descriptors() {
    yes(
        "var a=Symbol('x'),b=Symbol('x'),o={x:3};o[a]=1;o[b]=2;var keys=Reflect.ownKeys(o);delete o[a];o[a]=4;var keys2=Reflect.ownKeys(o);Object.defineProperty(o,a,{writable:false,configurable:false});var threw=false;try{Object.defineProperty(o,a,{value:9});}catch(e){threw=e instanceof TypeError;}keys.length===3 && keys[0]==='x' && keys[1]===a && keys[2]===b && keys2[1]===b && keys2[2]===a && o[a]===4 && o[b]===2 && (a in o) && o.hasOwnProperty(a) && o.propertyIsEnumerable(b) && Object.keys(o).join(',')==='x' && threw",
    );
}

#[test]
fn symbol_accessors_use_receiver_and_assign_define_properties_and_freeze_include_symbols() {
    yes(
        "var s=Symbol(),v=0,p={};Object.defineProperty(p,s,{get:function(){return this.n;},set:function(x){this.n=x;},enumerable:true,configurable:true});var o=Object.create(p);o[s]=7;var d={};d[s]={value:9,enumerable:true};var t=Object.defineProperties({},d),u=Object.assign({},t);Object.freeze(u);o[s]===7 && o.n===7 && u[s]===9 && Object.getOwnPropertyDescriptor(u,s).writable===false && Object.getOwnPropertyDescriptor(u,s).configurable===false",
    );
}

#[test]
fn well_known_coercion_receives_real_hints_and_errors_in_order() {
    yes(
        "var hints=[],o={};o[Symbol.toPrimitive]=function(h){hints.push(h);return h==='string'?'key':3;};var t={};t[o]=7;var r=o+1;var n=+o;var eq=o==3;hints.join(',')==='string,default,number,default' && t.key===7 && r===4 && n===3 && eq",
    );
    yes(
        "var log=[],a={},b={};a[Symbol.toPrimitive]=function(){log.push('a');return Symbol();};b[Symbol.toPrimitive]=function(){log.push('b');return 1;};try{a-b;}catch(e){}log.join(',')==='a'",
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("var o={};o[Symbol.toPrimitive]=()=>({});String(o)")
            .unwrap_err()
            .kind,
        ErrorKind::TypeError
    );
}

#[test]
fn custom_has_instance_tags_and_symbol_throws_are_observable() {
    yes(
        "var o={};o[Symbol.hasInstance]=function(v){return v===42;};var x={};x[Symbol.toStringTag]='Custom';var s=Symbol(),caught;try{throw s;}catch(e){caught=e;}42 instanceof o && !(1 instanceof o) && Object.prototype.toString.call(x)==='[object Custom]' && Object.prototype.toString.call(Symbol())==='[object Symbol]' && caught===s",
    );
    yes(
        "JSON.stringify(Symbol())===undefined && JSON.stringify([Symbol()])==='[null]' && JSON.stringify({x:Symbol()})==='{}' && JSON.stringify(Object(Symbol()))==='{}'",
    );
}

#[test]
fn native_symbol_api_and_lone_surrogates_survive_gc() {
    let mut vm = Vm::new();
    let symbol = vm
        .new_symbol(Some(JsString::from_utf16(vec![0xd800])))
        .unwrap();
    let key = PropertyKey::Symbol(symbol.as_symbol().unwrap().clone());
    let object = vm.new_object();
    let r = vm.root(object.clone());
    vm.define_data_property_key(
        object.clone(),
        key.clone(),
        Value::from("kept"),
        true,
        true,
        true,
    )
    .unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.get_property_key(object, key).unwrap(),
        Value::from("kept")
    );
    assert_eq!(
        symbol
            .as_symbol()
            .unwrap()
            .description()
            .unwrap()
            .as_utf16(),
        [0xd800]
    );
    vm.unroot(r);
}

#[test]
fn array_iterators_are_live_generic_and_self_iterable() {
    yes(
        "var a=[1,,3],it=a.values(),r1=it.next();a[1]=2;a.push(4);var r2=it.next(),r3=it.next(),r4=it.next();var done=it.next();a.push(5);r1.value===1 && r2.value===2 && r3.value===3 && r4.value===4 && done.done && it.next().done && it[Symbol.iterator]()===it && Array.prototype.values===Array.prototype[Symbol.iterator] && Object.prototype.toString.call(it)==='[object Array Iterator]' && Array.prototype.values.call({0:'a',length:1}).next().value==='a'",
    );
    yes(
        "var a=[5],k=a.keys().next(),e=a.entries().next();k.value===0 && e.value[0]===0 && e.value[1]===5 && e.done===false",
    );
}

#[test]
fn string_iterators_yield_codepoints_and_preserve_lone_surrogates() {
    yes(
        "var it='A\\uD83D\\uDE00\\uD800B'[Symbol.iterator]();it.next().value==='A' && it.next().value==='\\uD83D\\uDE00' && it.next().value==='\\uD800' && it.next().value==='B' && it.next().done && Object.prototype.toString.call(it)==='[object String Iterator]'",
    );
    yes(
        "var a=[][Symbol.iterator](),s=''[Symbol.iterator]();a.next();s.next();var n=Object.getPrototypeOf(a).next,threw=false;try{n.call(s);}catch(e){threw=e instanceof TypeError;}threw",
    );
}

#[test]
fn array_from_uses_protocol_and_closes_only_when_body_fails() {
    yes(
        "var closed=0,i=0,iter={next:function(){return {value:++i,done:false};},return:function(){closed++;throw 'close';}},source={};source[Symbol.iterator]=function(){return iter;};var caught;try{Array.from(source,function(){throw 'map';});}catch(e){caught=e;}closed===1 && caught==='map'",
    );
    yes(
        "var closed=0,source={};source[Symbol.iterator]=function(){return {next:function(){throw 5;},return:function(){closed++;return {};}};};try{Array.from(source);}catch(e){}closed===0 && Array.from('\\uD83D\\uDE00x').length===2 && Array.from({0:'x',length:1})[0]==='x'",
    );
}

#[test]
fn symbol_growth_and_infinite_iteration_are_bounded() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 1024 * 1024,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("var a=[];for(var i=0;i<10000;i++)a.push(Symbol('unique'+i));")
            .unwrap_err()
            .kind,
        ErrorKind::HeapLimit
    );
    let mut vm = Vm::with_limits(VmLimits {
        max_instructions: 1000,
        ..VmLimits::default()
    });
    assert_eq!(vm.eval("var x={};x[Symbol.iterator]=function(){return {next:function(){return {value:1,done:false};}};};Array.from(x)").unwrap_err().kind,ErrorKind::ExecutionLimit);
}

#[test]
fn array_species_uses_custom_constructor_and_concat_spreadable_protocol() {
    yes(
        "var calls=[],a=[1,,3];function C(n){calls.push(n);this.length=n;}a.constructor={[Symbol.species]:C};var m=a.map(x=>x*2),f=a.filter(x=>x>1),s=a.slice(1);var extra={0:'x',1:'y',length:2,[Symbol.isConcatSpreadable]:true};var c=a.concat(extra);m instanceof C && m[0]===2 && !(1 in m) && m[2]===6 && m.length===3 && f instanceof C && f[0]===3 && f.length===0 && s instanceof C && s.length===2 && !(0 in s) && s[1]===3 && c instanceof C && c.length===5 && c[3]==='x' && c[4]==='y' && calls.join(',')==='3,0,2,0' && Array[Symbol.species]===Array",
    );
    yes(
        "var a=[1,2];a[Symbol.isConcatSpreadable]=false;var b=[].concat(a);b.length===1 && b[0]===a",
    );
}

#[test]
fn deleted_symbol_keys_release_descriptions_at_collection() {
    let mut vm = Vm::new();
    vm.eval("var o={},s=Symbol('x'.repeat(4096));o[s]=42;s=undefined;")
        .unwrap();
    vm.collect_garbage();
    let before = vm.estimated_heap_bytes();
    vm.eval("delete o[Object.getOwnPropertySymbols(o)[0]]")
        .unwrap();
    vm.collect_garbage();
    let after = vm.estimated_heap_bytes();
    assert!(before > after + 8000, "{before} -> {after}");
}

#[test]
fn deleted_string_keys_and_truncated_array_indices_do_not_retain_text() {
    let mut vm = Vm::new();
    vm.eval("var o={},s='x'.repeat(4096);o[s]=42;s=undefined;")
        .unwrap();
    vm.collect_garbage();
    let before = vm.estimated_heap_bytes();
    vm.eval("delete o[Object.keys(o)[0]]").unwrap();
    vm.collect_garbage();
    let after = vm.estimated_heap_bytes();
    assert!(before > after + 8000, "{before} -> {after}");
    vm.eval("var a=[];for(var i=0;i<100;i++)a.push(i);a.length=0;")
        .unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.eval("Reflect.ownKeys(a).join(',')").unwrap(),
        Value::from("length")
    );
}
