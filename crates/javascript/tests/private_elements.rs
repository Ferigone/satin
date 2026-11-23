use rust_browser_javascript::{ErrorKind, Value, Vm, VmLimits};

fn yes(source: &str) {
    assert_eq!(
        Vm::new().eval(source).unwrap(),
        Value::Bool(true),
        "{source}"
    );
}

#[test]
fn fields_methods_and_accessors_use_hidden_slots_on_the_receiver() {
    yes(r#"
        class Counter {
            #value = 1;
            get #current() { return this.#value; }
            set #current(value) { this.#value = value; }
            #advance(by) { return this.#current += by; }
            read() { return this.#current; }
            advance(by) { return this.#advance(by); }
            getMethod() { return this.#advance; }
        }
        const a = Object.freeze(new Counter());
        const b = new Counter();
        const method = a.getMethod();
        a.advance(3) === 4 && a.read() === 4 && b.read() === 1
          && method === b.getMethod() && method.name === '#advance'
          && method.call(b, 5) === 6 && Object.getOwnPropertyNames(a).length === 0
          && !Object.getOwnPropertyDescriptor(Counter.prototype, '#advance')
    "#);
}

#[test]
fn private_references_participate_in_assignments_calls_and_optional_chains() {
    yes(r#"
        class C {
            #x = 1;
            #unused;
            #m() { return this; }
            update() {
                let old = this.#x++;
                this.#x ||= 99;
                this.#x ??= 88;
                this.#unused ??= 7;
                [this.#x] = [this.#x + 1];
                ({y: this.#x} = {y: this.#x + 1});
                return old === 1 && this.#x === 4 && this.#unused === 7;
            }
            probe(obj) { return obj?.#m?.() === obj && (obj?.#x) === 4; }
            nil() { return (null?.#m().x) === undefined; }
        }
        const c = new C();
        c.update() && c.probe(c) && c.nil()
    "#);
    yes(r#"
        class C {
            #x = { value: 1 };
            run() { return delete this.#x.value && this.#x.value === undefined; }
            set(target, effects) { target.#x = (effects.push('rhs'), 3); }
        }
        const c = new C(), effects = [];
        let threw;
        try { c.set(null, effects); } catch (error) { threw = error instanceof TypeError; }
        c.run() && threw && effects.join() === 'rhs'
    "#);
}

#[test]
fn static_and_instance_methods_exist_before_fields_and_blocks() {
    yes(r#"
        class Base {
            #first = [#first in this, #second in this, #get in this, this.#method()];
            #second = 17;
            get #get() { return this.#second; }
            #method() { return #second in this ? this.#get : undefined; }
            static { this.early = [#later in this, #m in this, this.#m()]; }
            static #later = 6;
            static #m() { return 7; }
            static has(o) { return #later in o; }
            read() { return [this.#first, this.#method()]; }
            has(o) { return #first in o && #second in o; }
        }
        class Derived extends Base {
            #second = 33;
            own() { return this.#second; }
        }
        const d = new Derived();
        d.read().join() === 'false,false,true,,17' && d.own() === 33
          && d.has(d) && !d.has(Object.create(d))
          && Base.early.join() === 'false,true,7' && Base.has(Base) && !Base.has(Derived)
    "#);
}

#[test]
fn missing_brands_and_missing_accessor_halves_throw_type_error() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        class C {
            #x;
            #m() {}
            get #readOnly() { return 1; }
            set #writeOnly(x) { this.#x = x; }
            read(target) { return target.#x; }
            has(target) { return #x in target; }
            writeMethod() { this.#m = 0; }
            writeGetter() { this.#readOnly = 0; }
            readSetter() { return this.#writeOnly; }
        }
        var c = new C();
    "#,
    )
    .unwrap();
    for source in [
        "c.read({ '#x': 1 })",
        "c.read(Object.create(c))",
        "c.read(1)",
        "c.has(null)",
        "c.has('x')",
        "c.writeMethod()",
        "c.writeGetter()",
        "c.readSetter()",
    ] {
        assert_eq!(
            vm.eval(source).unwrap_err().kind,
            ErrorKind::TypeError,
            "{source}"
        );
    }
}

#[test]
fn class_reevaluation_and_shadowing_keep_distinct_private_names() {
    yes(r#"
        function make(value) {
            return class {
                read(o) { return o.#x; }
                has(o) { return #x in o; }
                #x = value;
            };
        }
        const A = make(1), B = make(2);
        const a = new A(), b = new B();
        a.read(a) === 1 && b.read(b) === 2 && !a.has(b) && !b.has(a)
    "#);
    yes(r#"
        class Outer {
            make() {
                const outer = this;
                let read;
                const Inner = class extends (read = () => outer.#x, class {}) {
                    #x = 29;
                    own() { return this.#x; }
                    outerBrand() { return #other in outer; }
                };
                return [new Inner(), read];
            }
            #x = 7;
            #other;
        }
        const [inner, read] = new Outer().make();
        inner.own() === 29 && inner.outerBrand() && read() === 7
    "#);
}

#[test]
fn direct_eval_has_lexical_names_and_indirect_eval_does_not() {
    yes(r#"
        class C {
            #x = 5;
            check() { return eval('this.#x') === 5 && eval('#x in this'); }
            closure() { return eval('() => this.#x'); }
            direct() { return eval('class Nested { read(o) { return o.#x; } }; new Nested()'); }
            indirect() { return (0, eval)('this.#x'); }
            dynamic() { return Function('target', 'return target.#x'); }
        }
        const c = new C();
        let indirect, dynamic;
        try { c.indirect(); } catch (e) { indirect = e instanceof SyntaxError; }
        try { c.dynamic(); } catch (e) { dynamic = e instanceof SyntaxError; }
        c.check() && c.closure()() === 5 && c.direct().read(c) === 5 && indirect && dynamic
    "#);
}

#[test]
fn unicode_names_and_field_initializers_preserve_function_names() {
    yes(r#"
        class C {
            #\u0061 = function(){};
            #π = class { static inferred = this.name; };
            #\u{10400} = () => 1;
            #null() {}
            result() { return [this.#a.name, this.#π.inferred, this.#𐐀.name, this.#null.name].join(); }
        }
        new C().result() === '#a,#π,#𐐀,#null'
    "#);
    yes(r#"
        class C {
            st\u0061tic
            #first = 17;
            g\u0065t
            #second = 18;
            \u2118() { return this.#first; }
            m() { return this.#second; }
        }
        const obj = new C();
        obj.℘() === 17 && obj.\u2118() === 17 && obj.m() === 18
            && Object.hasOwn(obj, 'static') && Object.hasOwn(obj, 'get')
    "#);
}

#[test]
fn private_syntax_errors_are_rejected_before_execution() {
    let mut vm = Vm::new();
    for source in [
        "({ #x: 1 })",
        "obj.#x",
        "obj?.#x",
        "#x in obj",
        "class C { #constructor; }",
        r"class C { #\u0063onstructor(){} }",
        r"class C { #\u{D800}; }",
        r"class C { #\u0031; }",
        r"class C { #a; #\u0061; }",
        r"class C { #a; m(){ return \u0074his.#a; } }",
        r"class C { #a; m(){ return #a \u0069n this; } }",
        r"class C { g\u0065t #a(){} }",
        "class C { # x; }",
        "class C { #1; }",
        "class C { #x; static #x; }",
        "class C { get #x(){} static set #x(v){} }",
        "class C { get #x(){} get #x(){} }",
        "class C { #x(){} set #x(v){} }",
        "class C { set #x(v, b){} }",
        "class C { #x; m(){ return super.#x; } }",
        "class C { #x; m(){ delete (this.#x); } }",
        "class C { #x; m(){ delete this?.#x; } }",
        "class C { #x; m(){ return #x; } }",
        "class C { #x; m(){ return 1 + #x in this; } }",
        "class C { #x; m(){ #x in this = 0; } }",
        "class C extends (o.#x) { #x; }",
        "class C { #x; } class D extends C { m(){ return this.#x; } }",
        "class C { m(){ throw 0; return this.#undeclared; } }",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}

#[test]
fn derived_constructors_stamp_valid_receivers_and_reject_duplicate_elements() {
    yes(r#"
        const recipient = Object.preventExtensions({});
        let entered = 0, duplicate;
        class Target { constructor() { return recipient; } }
        class Field extends Target {
            #x = ++entered;
            static read(o) { return o.#x; }
        }
        const result = new Field();
        try { new Field(); } catch (e) { duplicate = e instanceof TypeError; }
        result === recipient && Field.read(recipient) === 1 && entered === 2 && duplicate
          && Object.getOwnPropertyNames(recipient).length === 0
    "#);
    yes(r#"
        const recipient = {};
        let entered = 0, duplicate;
        class Target { constructor() { return recipient; } }
        class Method extends Target {
            #x() { return 2; }
            y = (++entered, this.#x());
        }
        const result = new Method();
        try { new Method(); } catch (e) { duplicate = e instanceof TypeError; }
        result.y === 2 && entered === 1 && duplicate
    "#);
}

#[test]
fn failed_field_initializers_keep_only_previously_added_elements() {
    yes(r#"
        const recipient = {};
        class Base { constructor() { return recipient; } }
        class Child extends Base {
            #earlier = 7;
            #failing = (() => { throw 8; })();
            #later;
            #m() { return this.#earlier; }
            static inspect(o) {
                return [#earlier in o, #failing in o, #later in o, #m in o, o.#m()];
            }
        }
        let thrown;
        try { new Child(); } catch (error) { thrown = error; }
        thrown === 8 && Child.inspect(recipient).join() === 'true,false,false,true,7'
    "#);
}

#[test]
fn private_slots_methods_and_lexical_references_survive_collection_and_suspension() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var C = class {
            #object = { marker: 17 };
            #method() { return this.#object.marker; }
            get #read() { return this.#method(); }
            *#change() { this.#object = yield this.#read; return this.#read; }
            start() { return this.#change(); }
        };
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    assert_eq!(
        vm.eval("var it = new C().start(); C = null; it.next().value")
            .unwrap(),
        Value::from(17)
    );
    vm.collect_garbage();
    assert_eq!(
        vm.eval("it.next({marker:23}).value").unwrap(),
        Value::from(23)
    );
}

#[test]
fn private_fields_and_calls_respect_allocation_and_one_mib_stack_limits() {
    let mut vm = Vm::with_limits(VmLimits {
        max_heap_bytes: 2 * 1024 * 1024,
        ..VmLimits::default()
    });
    assert_eq!(
        vm.eval("class C{#a={}; #b=()=>this.#a;} var many=[]; for(let i=0;i<10000;i++)many.push(new C());")
          .unwrap_err()
          .kind,
        ErrorKind::HeapLimit
    );
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut vm = Vm::new();
            let source = format!("class C{{#x; m(){{return this{};}}}}", ".#x".repeat(500));
            assert_eq!(vm.check_syntax(&source).unwrap_err().kind, ErrorKind::ExecutionLimit);
            assert_eq!(
                vm.eval("class C{#recurse(){return this.#recurse();} run(){return this.#recurse();}}new C().run()")
                    .unwrap_err()
                    .kind,
                ErrorKind::ExecutionLimit
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
