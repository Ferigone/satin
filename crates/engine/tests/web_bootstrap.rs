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
