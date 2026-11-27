use rust_browser_engine::{LoadedPage, script::ScriptRuntime};

#[test]
fn navigator_and_images_use_the_window_realm_and_dom() {
    let page = LoadedPage::from_html("https://example.test/path/", "<img id='parsed'>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime.eval(r#"
        const image = new Image(16, 8);
        image.alt = 'preview';
        image.src = 'fixture.png';
        image.crossOrigin = 'USE-CREDENTIALS';
        const credentials = image.crossOrigin === 'use-credentials';
        image.crossOrigin = null;
        let illegalConstructor = false;
        try { new HTMLImageElement(); }
        catch (error) { illegalConstructor = error.name === 'TypeError'; }
        let callWithoutNew = false;
        try { Image(); }
        catch (error) { callWithoutNew = error.name === 'TypeError'; }
        let illegalGetter = false;
        try { Object.getOwnPropertyDescriptor(Navigator.prototype, 'userAgent').get.call({}); }
        catch (error) { illegalGetter = error.name === 'TypeError'; }
        illegalConstructor && callWithoutNew && illegalGetter && navigator === window.navigator &&
        navigator === clientInformation && navigator instanceof Navigator &&
        navigator.userAgent.includes('Satin/') && navigator.cookieEnabled && navigator.onLine === true &&
        'Mozilla/' + navigator.appVersion === navigator.userAgent &&
        navigator.language === navigator.languages[0] && Object.isFrozen(navigator.languages) &&
        navigator.hardwareConcurrency >= 1 &&
        image instanceof HTMLImageElement && image instanceof Image &&
        image.tagName === 'IMG' && image.ownerDocument === document && image.parentNode === null &&
        image.width === 16 && image.height === 8 && image.alt === 'preview' &&
        document.getElementById('parsed') instanceof HTMLImageElement &&
        image.getAttribute('src') === 'fixture.png' && image.src === 'https://example.test/path/fixture.png' &&
        credentials && image.crossOrigin === null && !image.hasAttribute('crossorigin');
    "#).unwrap();
    assert_eq!(result.as_bool(), Some(true), "{result:?}");
}

#[test]
fn web_crypto_respects_view_bounds_and_secure_context() {
    let page = LoadedPage::from_html("https://example.test/", "");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime.eval(r#"
        const storage = new Uint8Array(34);
        storage[0] = storage[33] = 99;
        const body = storage.subarray(1, 33);
        const sameView = crypto.getRandomValues(body) === body;
        const errors = [];
        for (const target of [new Float32Array(1), new DataView(new ArrayBuffer(8)),
                              new Uint8Array(65537), {}]) {
            try { crypto.getRandomValues(target); }
            catch (error) { errors.push(error.name); }
        }
        const uuid = crypto.randomUUID();
        isSecureContext && crypto === window.crypto && crypto instanceof Crypto &&
            crypto.subtle === crypto.subtle && crypto.subtle instanceof SubtleCrypto &&
            sameView && body.some(value => value !== 0) &&
            storage[0] === 99 && storage[33] === 99 &&
            errors.join(',') === 'TypeMismatchError,TypeMismatchError,QuotaExceededError,TypeError' &&
            /^[\da-f]{8}-[\da-f]{4}-4[\da-f]{3}-[89ab][\da-f]{3}-[\da-f]{12}$/.test(uuid);
    "#).unwrap();
    assert_eq!(result.as_bool(), Some(true), "{result:?}");

    runtime.eval(r#"
        let result = '', refusal = '';
        const bytes = new Uint8Array([0, 97, 98, 99, 0]);
        crypto.subtle.digest({get name() { bytes.fill(0); return 'sha-256'; }}, bytes.subarray(1, 4))
            .then(out => { result = new Uint8Array(out).toHex(); });
        crypto.subtle.digest('unknown', new Uint8Array(1)).catch(error => { refusal = error.name; });
    "#).unwrap();
    assert_eq!(runtime.eval("result === ''").unwrap().as_bool(), Some(true));
    runtime.pump();
    assert_eq!(runtime.eval(r#"
        result === 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad' &&
            refusal === 'NotSupportedError'
    "#).unwrap().as_bool(), Some(true));

    let page = LoadedPage::from_html("http://example.test/", "");
    let mut insecure = ScriptRuntime::new(&page).unwrap();
    assert_eq!(insecure.eval(r#"
        !isSecureContext && typeof crypto.getRandomValues === 'function' &&
            !('randomUUID' in crypto) && !('subtle' in crypto)
    "#).unwrap().as_bool(), Some(true));
}
