use rust_browser_javascript::{Value, Vm};

#[test]
fn lazy_helpers_and_disposal_keep_their_inputs_alive() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var log = [], cleanup = new DisposableStack();
        var iter = Iterator.zip([[3,4], [8,9]]).map(([a,b]) => a+b);
        cleanup.use({ [Symbol.dispose]() { log.push("used"); }});
        cleanup.adopt(iter, it => log.push(it.next().value));
        cleanup.defer(() => log.push("first"));
        var moved = cleanup.move();
    "#,
    )
    .unwrap();
    vm.collect_garbage();
    assert_eq!(vm.eval(r#"
        moved.dispose();
        cleanup.disposed && moved.disposed && log.join() === "first,11,used" && iter.next().value === 13
    "#).unwrap(),Value::Bool(true));
}

#[test]
fn async_stacks_suppress_errors_and_ignore_sync_fallback_results() {
    let mut vm = Vm::new();
    vm.eval(
        r#"
        var stack = new AsyncDisposableStack(), seen="";
        stack.defer(() => { throw "outer" });
        stack.use({[Symbol.dispose]() {return {then() {seen += "wrong"}}}});
        stack.adopt(17, async n => { throw n });
        stack.disposeAsync().catch(e => {
            if (e instanceof SuppressedError && stack.disposed) seen += e.error + e.suppressed;
        });
    "#,
    )
    .unwrap();
    vm.drain_jobs(100).unwrap();
    assert_eq!(vm.eval("seen").unwrap(), Value::from("outer17"));
}

#[test]
fn byte_decoding_commits_only_complete_valid_chunks() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        var a = new Uint8Array(5), failed = false;
        try { a.setFromBase64("QUJDRB===", {lastChunkHandling:"strict"}) }
        catch (e) {failed = e instanceof SyntaxError}
        var r = a.subarray(3).setFromHex("fefe00");
        failed && r.read === 4 && r.written === 2 && a.toHex() === "414243fefe"
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn precise_sums_keep_cancellation_bits_and_round_only_at_the_end() {
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval(
            r#"
        var max = Number.MAX_VALUE, tiny = Number.MIN_VALUE;
        Math.sumPrecise([max, max, -max]) === max &&
        Math.sumPrecise([max, tiny, -max]) === tiny &&
        Object.is(Math.sumPrecise([-0, -0]), -0) &&
        2 ** -2 === 0.25 && (-2) ** 2 === 4
    "#
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert!(vm.eval("-2 ** 2").is_err());
}
