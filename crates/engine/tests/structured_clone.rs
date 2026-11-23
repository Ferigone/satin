use rust_browser_engine::{LoadedPage, script::ScriptRuntime};

fn yes(source: &str) {
    let mut runtime = ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/",
        "<button id='target'>Target</button>",
    ))
    .unwrap();
    assert_eq!(
        runtime.eval(source).unwrap().as_bool(),
        Some(true),
        "{source}"
    );
}

#[test]
fn document_global_clones_and_transfers_through_the_normal_binding() {
    yes(r#"
        const source = new ArrayBuffer(5), forgotten = new ArrayBuffer(1);
        new Uint8Array(source).set([10,11,12,13,14]);
        const input = {view: new Uint8Array(source, 1, 2), data: new DataView(source)};
        input.self = input;
        const order = [], options = Object.create({});
        Object.defineProperty(options, 'transfer', {get: () => {
            order.push('options');
            return (function* () { order.push('iteration'); yield source; yield forgotten; })();
        }});
        Object.defineProperty(input, 'added', {enumerable: true, get: () => {
            order.push('value');
            return 17;
        }});
        const copy = window.structuredClone(input, options);
        window.structuredClone.length === 1 && copy.self === copy && copy.added === 17
            && source.detached && forgotten.detached && copy.view.buffer === copy.data.buffer
            && copy.view.byteOffset === 1 && copy.view.join() === '11,12'
            && order.join() === 'options,iteration,value'
    "#);
}

#[test]
fn noncloneable_platform_objects_use_the_captured_dom_exception_class() {
    yes(r#"
        const ctor = DOMException, clone = structuredClone;
        DOMException = () => { throw 1; };
        const errors = [];
        for (const source of [
            window, document, document.location, document.getElementById('target'),
            document.getElementById('target').style, history, performance,
            new Event('x'), new AbortController(), new ReadableStream(),
            new WeakMap(), Symbol(), () => {},
        ]) {
            try { clone(source); }
            catch (error) {
                errors.push(error instanceof ctor && error.name === 'DataCloneError' && error.code === 25);
            }
        }
        const ordinary = Object.create(Object.getPrototypeOf(document));
        ordinary.own = 2;
        errors.length === 13 && errors.every(Boolean) && clone(ordinary).own === 2
    "#);
}

#[test]
fn dictionary_and_sequence_conversion_keep_original_webidl_errors() {
    yes(r#"
        const thrown = {}, results = [], buffer = new ArrayBuffer(2);
        let closed = 0;
        const inputs = [
            () => structuredClone(),
            () => structuredClone.call({}, 1),
            () => structuredClone(1, false),
            () => structuredClone(1, {transfer: null}),
            () => structuredClone(1, {transfer: {}}),
            () => structuredClone(1, {transfer: (function* () {
                try { yield buffer; yield 3; }
                finally { closed++; }
            })()}),
            () => structuredClone(1, {transfer: [buffer, buffer]}),
            () => structuredClone(1, {transfer: [new Uint8Array(buffer)]}),
            () => {
                const options = {};
                Object.defineProperty(options, 'transfer', {get: () => { throw thrown; }});
                structuredClone(1, options);
            },
        ];
        for (const run of inputs) {
            try { run(); }
            catch (error) { results.push(error === thrown ? 'original' : error.name); }
        }
        results.join() === 'TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,DataCloneError,DataCloneError,original'
            && closed === 1 && !buffer.detached && structuredClone(null, null) === null
    "#);
}

#[test]
fn clone_captures_platform_intrinsics_and_immutable_payload_metadata() {
    yes(r#"
        const BlobType = Blob, FileType = File, Exception = DOMException;
        const bytes = new Blob(['abcd']).slice(1, 3, 'TEXT/PLAIN');
        const file = new File([bytes], 'żółć.txt', {type: 'TEXT/PLAIN', lastModified: 42});
        const exception = new DOMException('lone \uD800', 'AbortError');
        Object.defineProperty(bytes, 'size', {get: () => { throw 'author getter'; }});
        Object.defineProperty(file, 'name', {get: () => { throw 'author getter'; }});
        Object.defineProperty(exception, 'name', {get: () => { throw 'author getter'; }});
        globalThis.Blob = globalThis.File = globalThis.DOMException = () => { throw 'author constructor'; };
        const result = structuredClone({file, bytes, exception, again: file, set: new Set([exception])});
        result.file instanceof FileType && result.bytes instanceof BlobType
            && result.bytes !== bytes && result.bytes.type === 'text/plain' && result.bytes.size === 2
            && result.file.name === 'żółć.txt' && result.file.lastModified === 42
            && result.file.type === 'text/plain' && result.again === result.file
            && result.exception instanceof Exception && result.exception !== exception
            && result.exception.name === 'AbortError' && result.exception.code === 20
            && result.exception.message.charCodeAt(5) === 0xD800 && result.set.has(result.exception)
    "#);
}

#[test]
fn image_data_subserialization_shares_the_cloned_view_and_transfer_graph() {
    yes(r#"
        const buffer = new ArrayBuffer(12);
        const pixels = new Uint8ClampedArray(buffer, 4, 4);
        pixels.set([8, 13, 21, 255]);
        const image = new ImageData(pixels, 1, 1);
        const saved = ImageData;
        globalThis.ImageData = () => { throw 'author constructor'; };
        Object.defineProperty(image, 'data', {get: () => { throw 'author getter'; }});
        let failure;
        try { structuredClone({image, node: document.body}, {transfer: [buffer]}); }
        catch (error) { failure = error.name; }
        const retained = !buffer.detached && pixels[0] === 8;
        const copy = structuredClone({image, pixels, buffer});
        copy.pixels[0] = 144;
        const copied = copy.image instanceof saved && copy.image.data === copy.pixels
            && copy.image.data.buffer === copy.buffer && copy.image.data.byteOffset === 4
            && copy.image.data[0] === 144 && pixels[0] === 8;
        const moved = structuredClone({buffer, pixels, image}, {transfer: [buffer]});
        failure === 'DataCloneError' && retained && copied && buffer.detached
            && moved.image.data === moved.pixels && moved.image.data.buffer === moved.buffer
            && moved.image.data.join() === '8,13,21,255'
            && moved.image.width === 1 && moved.image.height === 1
            && moved.image.colorSpace === 'srgb' && moved.image.pixelFormat === 'rgba-unorm8'
    "#);
}
