use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn check(source: &str) {
    let mut runtime = ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html>",
    ))
    .unwrap();
    assert_eq!(runtime.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn encoder_replaces_unpaired_surrogates_and_writes_whole_code_points() {
    check(
        r#"
        let encoder = new TextEncoder();
        let encoded = encoder.encode('Pą😀\ud800');
        let storage = new Uint8Array(8).fill(90);
        let offset = new Uint8Array(storage.buffer, 2, 5);
        let result = encoder.encodeInto('A😀Z', offset);
        let incomplete = encoder.encodeInto('😀', new Uint8Array(storage.buffer, 0, 3));
        encoder.encoding === 'utf-8' && encoded instanceof Uint8Array &&
            encoded.join(',') === '80,196,133,240,159,152,128,239,191,189' &&
            result.read === 3 && result.written === 5 &&
            storage.join(',') === '90,90,65,240,159,152,128,90' &&
            incomplete.read === 0 && incomplete.written === 0 &&
            encoder.encode().length === 0;
        "#,
    );
}

#[test]
fn decoder_tracks_a_split_bom_and_scalar_until_flush() {
    check(
        r#"
        let decoder = new TextDecoder(' UTF-8 \t');
        let first = decoder.decode(new Uint8Array([0xef, 0xbb]), {stream:true});
        let second = decoder.decode(new Uint8Array([0xbf, 0xf0, 0x9f]), {stream:true});
        let third = decoder.decode(new Uint8Array([0x98, 0x80]));
        let next = decoder.decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x41]));
        let bom = new TextDecoder('utf-8', {ignoreBOM:true});
        let withBom = bom.decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x41]));
        let legacy = new TextDecoder('iso-8859-1');
        let source = new Uint8Array([0x21, 0x80, 0x21]);
        let middle = new DataView(source.buffer, 1, 1);
        first === '' && second === '' && third === '😀' && next === 'A' &&
            withBom === '\ufeffA' && bom.ignoreBOM && !decoder.ignoreBOM &&
            decoder.encoding === 'utf-8' && legacy.encoding === 'windows-1252' &&
            legacy.decode(middle) === '€';
        "#,
    );
}

#[test]
fn fatal_decode_can_restart_and_option_getters_can_reenter() {
    check(
        r#"
        let decoder = new TextDecoder('utf-8', {fatal:true});
        let incomplete = decoder.decode(new Uint8Array([0xc2]), {stream:true});
        let fatal = false;
        try { decoder.decode(); } catch (error) { fatal = error instanceof TypeError; }
        let source = new Uint8Array([65]);
        let nested = '';
        let options = Object.defineProperty({}, 'stream', {
            get: function () {
                nested = decoder.decode(new Uint8Array([97]));
                source.buffer.transfer();
                return true;
            }
        });
        let empty = decoder.decode(source, options);
        let resumed = decoder.decode(new Uint8Array([66]));
        incomplete === '' && decoder.fatal && fatal &&
            nested === 'a' && empty === '' && resumed === 'B';
        "#,
    );
}

#[test]
fn encoding_interfaces_enforce_brands_and_webidl_options() {
    check(
        r#"
        let checks = 0;
        let encoder = new TextEncoder();
        try { encoder.encodeInto('x', new Uint8ClampedArray(4)); }
        catch (error) { if (error instanceof TypeError) checks++; }
        try { encoder.encodeInto('x', new Uint8Array(new ArrayBuffer(1, {maxByteLength:2}))); }
        catch (error) { if (error instanceof TypeError) checks++; }
        try { (new TextDecoder()).decode(null); }
        catch (error) { if (error instanceof TypeError) checks++; }
        try { new TextDecoder('replacement'); }
        catch (error) { if (error instanceof RangeError) checks++; }
        let getter = Object.getOwnPropertyDescriptor(TextEncoder.prototype, 'encoding').get;
        try { getter.call({}); }
        catch (error) { if (error instanceof TypeError) checks++; }
        let detached = new Uint8Array(2);
        detached.buffer.transfer();
        checks === 5 && encoder.encodeInto('x', detached).written === 0;
        "#,
    );
}
