use rust_browser_javascript::{ErrorKind, Value, Vm};

fn check(source: &str) {
    let mut vm = Vm::new();
    assert_eq!(vm.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn dates_have_an_internal_brand_and_are_callable_as_functions() {
    check(
        r#"
        let invoked = 0;
        let text = Date({valueOf() { invoked++; return 7; }});
        let first = new Date(12345);
        first[Symbol.toPrimitive] = undefined;
        first.valueOf = () => { throw 9; };
        let second = new Date(first);
        class Stamp extends Date {}
        let derived = new Stamp(23456);
        let invalidReceiver = false;
        try { Date.prototype.getTime(); }
        catch (error) { invalidReceiver = error instanceof TypeError; }
        typeof text === 'string' && invoked === 0 &&
            second.getTime() === 12345 && second instanceof Date &&
            derived instanceof Stamp && derived instanceof Date && derived.getTime() === 23456 &&
            Object.prototype.toString.call(derived) === '[object Date]' &&
            Object.prototype.toString.call(Date.prototype) === '[object Object]' &&
            invalidReceiver && typeof Date.now() === 'number' && Date.length === 7;
        "#,
    );
}

#[test]
fn utc_calendar_fields_carry_across_leap_years_and_clip_boundaries() {
    check(
        r#"
        let negative = new Date(-1);
        let narrow = new Date(Date.UTC(2024, 1, 29, 25, -1, 0, -1));
        let minimum = new Date(-8640000000000000);
        let maximum = new Date(8640000000000000);
        let outside = new Date(8640000000000001);
        let negativeZero = new Date(-0.9);
        negative.getUTCFullYear() === 1969 && negative.getUTCMonth() === 11 &&
            negative.getUTCDate() === 31 && negative.getUTCDay() === 3 &&
            negative.getUTCHours() === 23 && negative.getUTCMinutes() === 59 &&
            negative.getUTCSeconds() === 59 && negative.getUTCMilliseconds() === 999 &&
            narrow.toISOString() === '2024-03-01T00:58:59.999Z' &&
            minimum.toISOString() === '-271821-04-20T00:00:00.000Z' &&
            maximum.toISOString() === '+275760-09-13T00:00:00.000Z' &&
            Number.isNaN(outside.getTime()) && Object.is(negativeZero.getTime(), 0) &&
            Date.UTC(99, 12, 0, 24, 0, 0, 0) === 946684800000;
        "#,
    );
}

#[test]
fn iso_and_serialized_dates_round_trip_through_the_native_parser() {
    check(
        r#"
        let normalized = new Date('2020-02-29T24:00:00.000Z');
        let offset = new Date('2023-07-02T08:11:15.902+05:45');
        let local = new Date(2024, 0, 15, 3, 40, 20, 81);
        let dateOnly = Date.parse('2024-01-15');
        let fromFields = Date.UTC(2024, 0, 15, 3, 40, 20, 81);
        let fromLocal = Date.parse('2024-01-15T03:40:20.081');
        normalized.toISOString() === '2020-03-01T00:00:00.000Z' &&
            offset.toISOString() === '2023-07-02T02:26:15.902Z' &&
            Date.parse('0000') === -62167219200000 &&
            new Date('+000000-01-01').getUTCFullYear() === 0 &&
            Number.isNaN(Date.parse('-000000-01-01')) &&
            Number.isNaN(Date.parse('2024-13-01T00:00:00Z')) &&
            Number.isNaN(Date.parse('2024-01-01T24:01:00Z')) &&
            Number.isNaN(Date.parse('+275760-09-13T00:00:00.001Z')) &&
            dateOnly === Date.UTC(2024, 0, 15) &&
            fromLocal === local.valueOf() && fromLocal === fromFields + local.getTimezoneOffset() * 60000 &&
            Date.parse(normalized.toUTCString()) === normalized.valueOf() &&
            Date.parse(normalized.toString()) === normalized.valueOf() &&
            Date.parse('01 Jan 1970 00:00:00 GMT') === 0;
        "#,
    );
}

#[test]
fn setters_snapshot_the_receiver_and_recover_only_where_required() {
    check(
        r#"
        let stamp = new Date('2000-02-29T00:00:00Z');
        let sequence = '';
        const marker = (digit, result) => ({valueOf() { sequence += digit; return result; }});
        const changed = stamp.setUTCFullYear(marker('a', 2001), marker('b', 1), marker('c', 29));
        let repaired = new Date(NaN);
        repaired.setUTCFullYear(9);
        let keep = new Date(NaN);
        const result = keep.setUTCMonth({valueOf() { keep.setTime(123); return 2; }});
        let reentry = new Date(Date.UTC(2020, 0, 2));
        reentry.setUTCDate({valueOf() { reentry.setTime(0); return 4; }});
        let year = new Date(2020, 0, 3);
        year.setYear(99);
        sequence === 'abc' && changed === Date.UTC(2001, 2, 1) &&
            stamp.toISOString() === '2001-03-01T00:00:00.000Z' &&
            repaired.toISOString() === '0009-01-01T00:00:00.000Z' &&
            Number.isNaN(result) && keep.getTime() === 123 &&
            reentry.toISOString() === '2020-01-04T00:00:00.000Z' &&
            year.getFullYear() === 1999 && year.getYear() === 99 &&
            Number.isNaN(stamp.setUTCHours(1, undefined));
        "#,
    );
}

#[test]
fn primitive_conversion_and_json_preserve_overrides() {
    check(
        r#"
        let value = new Date(0);
        value.toString = () => 'formatted';
        let finite = {valueOf() { return 42; }, toISOString() { return 'custom'; }};
        let infinite = {valueOf() { return Infinity; }, toISOString() { throw 1; }};
        let untouched = new Date('2020-01-01T04:00:00Z');
        let duck = {toString() { return 's'; }, valueOf() { return 7; }};
        let convert = Date.prototype[Symbol.toPrimitive];
        String(value) === 'formatted' && value + '' === 'formatted' && +value === 0 &&
            convert.call(duck, 'default') === 's' && convert.call(duck, 'number') === 7 &&
            Date.prototype.toJSON.call(finite) === 'custom' &&
            Date.prototype.toJSON.call(infinite) === null &&
            JSON.stringify({date: untouched, missing: new Date(NaN)}) ===
                '{"date":"2020-01-01T04:00:00.000Z","missing":null}' &&
            Date.prototype.toGMTString === Date.prototype.toUTCString;
        "#,
    );
    let mut vm = Vm::new();
    assert_eq!(
        vm.eval("new Date(NaN).toISOString()").unwrap_err().kind,
        ErrorKind::RangeError
    );
}
