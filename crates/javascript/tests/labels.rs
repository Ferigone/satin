use rust_browser_javascript::{ErrorKind, Value, Vm};

fn checkpoint(vm: &mut Vm) {
    vm.with_task(|vm| {
        while vm.has_pending_jobs() {
            vm.drain_jobs(1)?;
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn adjacent_labels_target_one_iteration_and_break_can_cross_switch() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var log=[];
            exit: {
                again: loop: for (var i=0; i<4; i++) {
                    switch (i) {
                    case 0: log.push('next'); continue again;
                    case 1: log.push('exit'); break exit;
                    }
                    log.push('unreachable');
                }
                log.push('unreachable');
            }
            var count=0;
            repeat: do {
                count++;
                if (count<3) continue repeat;
                break repeat;
            } while (true);
            var word='';
            keys: for (const key in {a: 1, b: 2}) {
                if (key==='a') continue keys;
                word+=key;
            }
            log.join(',')+':'+count+':'+word;
            "#,
        )
        .unwrap(),
        Value::from("next,exit:3:b")
    );
}

#[test]
fn labels_use_decoded_names_and_honor_line_terminators() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            va\u006Cue: { break value; throw 'unreachable'; }
            trail: while (true) { break /*
            */ undefinedLabel; }
            for(var i=0;i<1;i++) { continue
            undefinedLabel; }
            var first=answer();
            declaration: function answer() { return 17; }
            first;
            "#,
        )
        .unwrap(),
        Value::from(17)
    );
}

#[test]
fn invalid_label_targets_and_declarations_are_early_errors() {
    let mut vm = Vm::new();
    for source in [
        "break absent;",
        "here: { while (true) continue here; }",
        r#"a: \u0061: while (true) break a;"#,
        "outside: { function f(){break outside;} }",
        "outside: while (false) { const f = () => { continue outside; }; }",
        "outside: { class X { static { break outside; } } }",
        "tag: let binding;",
        "tag: const binding=1;",
        "tag: class C {}",
        "tag: function* generator() {}",
        "if (false) tag: function invalid() {}",
        "tag: async function invalid() {}",
        "'use strict'; yield: ;",
        "'use strict'; tag: function invalid() {}",
    ] {
        assert_eq!(
            vm.check_syntax(source).unwrap_err().kind,
            ErrorKind::SyntaxError,
            "{source}"
        );
    }
}

#[test]
fn eval_keeps_statement_values_through_label_and_finally() {
    let mut vm = Vm::new();
    for (source, value) in [
        ("done: { 37; break done; }", Value::from(37)),
        ("2; done: { break done; }", Value::from(2)),
        ("L: { try { 9; break L; } finally { 4; } }", Value::from(9)),
        (
            "L: { try { 9; break L; } finally { 4; break L; } }",
            Value::from(4),
        ),
        ("L: switch(1) { case 1: 8; break L; }", Value::from(8)),
        ("L: while(true) { 6; break L; }", Value::from(6)),
        ("7; if(false) {}", Value::Undefined),
        ("7; try{}finally{}", Value::Undefined),
        ("7; while(false) {}", Value::Undefined),
    ] {
        assert_eq!(vm.eval(source).unwrap(), value, "{source}");
    }
}

#[test]
fn labelled_continue_closes_crossed_iterators_in_order() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            var log=[];
            function items(name, count, error) {
                var i=0;
                return {[Symbol.iterator](){return {
                    next(){return {value:i++,done:i>count};},
                    return(){log.push(name);if(error)throw 23;return {};}
                };}};
            }
            outer: for (const x of items('outer',2,false)) {
                for (const y of items('inner',1,false)) {
                    log.push(x);
                    if (x===0) continue outer;
                    break outer;
                }
            }
            try {
                retry: for (const x of items('retry',1,false))
                    for (const y of items('fails',1,true)) continue retry;
            } catch(e) { log.push(e); }
            log.join(',');
            "#,
        )
        .unwrap(),
        Value::from("0,inner,1,inner,outer,fails,retry,23")
    );
}

#[test]
fn labelled_async_iterator_close_completes_before_outer_progress() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var log=[], gate=Promise.withResolvers();
        function items(name,count,wait) {
            var i=0;
            return {[Symbol.asyncIterator](){return {
                next(){log.push(name+' next');return Promise.resolve({value:i++,done:i>count});},
                return(){
                    log.push(name+' close');
                    if (wait) return gate.promise.then(()=>{log.push(name+' closed');return {};});
                    return Promise.resolve({});
                }
            };}};
        }
        async function run() {
            outer: for await (const a of items('outer',2,false)) {
                for await (const b of items('inner',1,true)) {
                    log.push(a);
                    if (a===0) continue outer;
                    break outer;
                }
            }
            log.push('done');
        }
        run().catch(error=>log.push(error.message));
        "#,
    )
    .unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from("outer next,inner next,0,inner close")
    );
    vm.collect_garbage();
    vm.eval("gate.resolve()").unwrap();
    checkpoint(&mut vm);
    assert_eq!(
        vm.eval("log.join(',')").unwrap(),
        Value::from(
            "outer next,inner next,0,inner close,inner closed,outer next,inner next,1,inner close,inner closed,outer close,done"
        )
    );
}

#[test]
fn generator_finally_can_replace_a_suspended_labelled_break() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
            function* g(){
                again: for(let i=0;i<2;i++){
                    try { yield i; break again; }
                    finally { yield 'cleanup'; if(i===0) continue again; }
                }
                return 'done';
            }
            var it=g(), log=[];
            for(var i=0;i<5;i++){var record=it.next();log.push(record.value,record.done);}
            log.join(',');
            "#,
        )
        .unwrap(),
        Value::from("0,false,cleanup,false,1,false,cleanup,false,done,true")
    );
}
