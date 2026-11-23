use rust_browser_engine::{
    LoadedPage,
    script::ScriptRuntime,
    web_storage::{StoragePersistence, StorageProfile, TabStorage},
};
use rust_browser_javascript::Value;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

fn realm(url: &str, tab: &TabStorage) -> ScriptRuntime {
    ScriptRuntime::with_storage(
        &LoadedPage::from_html(url, "<!doctype html><title>Storage</title>"),
        tab,
    )
    .unwrap()
}

fn check(realm: &mut ScriptRuntime, source: &str) {
    assert_eq!(realm.eval(source).unwrap(), Value::Bool(true), "{source}");
    assert!(realm.diagnostics().is_empty(), "{:?}", realm.diagnostics());
}

#[test]
fn storage_is_a_live_named_webidl_object() {
    let mut runtime = realm("https://example.test/first", &TabStorage::default());
    check(
        &mut runtime,
        r#"
      const s = localStorage;
      const marker = Symbol('ordinary');
      s[marker] = 3;
      s.alpha = 7;
      Object.defineProperty(s, 'beta', {value:'value'});
      s.setItem = 'hidden';
      s.setItem('surrogate', '\ud800');
      const descriptor = Object.getOwnPropertyDescriptor(s, 'alpha');
      const keys = Object.keys(s);
      const before = s === window.localStorage && s !== sessionStorage && s instanceof Storage &&
        descriptor.value === '7' && descriptor.writable && descriptor.configurable && descriptor.enumerable &&
        keys.length === 3 && keys.indexOf('alpha') >= 0 && keys.indexOf('beta') >= 0 &&
        keys.indexOf('surrogate') >= 0 &&
        s.getItem('setItem') === 'hidden' &&
        s[marker] === 3 && s.getItem('surrogate').charCodeAt(0) === 55296 && s.length === 4;
      const written = Reflect.set(s, 'alpha', 8) && s.alpha === '8' && 'alpha' in s;
      const denied = !Reflect.defineProperty(s, 'gamma', {get:function(){return 0}}) &&
        !Reflect.preventExtensions(s) && !('gamma' in s);
      delete s.alpha;
      delete s.setItem;
      before && written && denied && s.getItem('alpha') === null && !('alpha' in s) &&
        s.getItem('setItem') === 'hidden' && s.length === 3
    "#,
    );
}

#[test]
fn origin_identity_and_session_area_follow_tabs() {
    let profile = StorageProfile::default();
    let tab = TabStorage::new(profile.clone());
    let next = TabStorage::new(profile);
    let mut source = realm("https://example.test:443/first", &tab);
    source
        .eval("localStorage.token='local'; sessionStorage.token='session';")
        .unwrap();
    drop(source);
    let mut same_tab = realm("https://example.test/second", &tab);
    let mut different_tab = realm("https://example.test/second", &next);
    let mut different_port = realm("https://example.test:444/first", &tab);
    let mut other_scheme = realm("http://example.test/first", &tab);
    let mut opaque = realm("data:text/html,opaque", &tab);
    check(
        &mut same_tab,
        "localStorage.token === 'local' && sessionStorage.token === 'session'",
    );
    check(
        &mut different_tab,
        "localStorage.token === 'local' && sessionStorage.getItem('token') === null",
    );
    check(
        &mut different_port,
        "localStorage.length === 0 && sessionStorage.length === 0",
    );
    check(
        &mut other_scheme,
        "localStorage.length === 0 && sessionStorage.length === 0",
    );
    check(
        &mut opaque,
        r#"
       var matched;
       try { void localStorage; }
       catch(error) { matched = error instanceof DOMException && error.name === 'SecurityError'; }
       matched && typeof Storage === 'function'
    "#,
    );
}

#[test]
fn storage_events_are_async_scoped_and_carry_the_live_wrapper() {
    let profile = StorageProfile::default();
    let tab = TabStorage::new(profile.clone());
    let mut a = realm("https://example.test/writer", &tab);
    let mut b = realm(
        "https://example.test/observer",
        &TabStorage::new(profile.clone()),
    );
    let mut c = realm("https://example.test/frame", &tab);
    let mut d = realm("https://other.test/observer", &TabStorage::new(profile));
    for context in [&mut a, &mut b, &mut c, &mut d] {
        context.eval(r#"
          var log = [], brand = true;
          onstorage = function(event) {
            brand = brand && event instanceof StorageEvent && event.isTrusted &&
              event.target === window && !event.bubbles && !event.cancelable;
            log.push([event.key, event.oldValue, event.newValue, event.url, event.storageArea === localStorage]);
            queueMicrotask(() => log.push('micro'));
          };
        "#).unwrap();
    }
    a.eval("localStorage.item='value'; localStorage.item='value'; localStorage.item='next'; localStorage.clear(); localStorage.clear(); sessionStorage.only='tab'").unwrap();
    check(&mut b, "log.length === 0");
    for context in [&mut a, &mut b, &mut c, &mut d] {
        context.pump();
    }
    check(&mut a, "log.length === 0");
    check(
        &mut b,
        "brand && log.length === 6 && log[0].join('/') === 'item//value/https://example.test/writer/true' && log[1] === 'micro' && log[2][1] === 'value' && log[2][2] === 'next' && log[4][0] === null",
    );
    check(
        &mut c,
        "brand && log.length === 8 && log[6][0] === 'only' && !log[6][4] && sessionStorage.only === 'tab'",
    );
    check(&mut d, "log.length === 0");
}

#[derive(Default)]
struct Disk {
    files: RefCell<BTreeMap<String, Vec<u8>>>,
    fail: Cell<bool>,
}

impl StoragePersistence for Disk {
    fn load(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.files.borrow().get(id).cloned())
    }

    fn save(&self, id: &str, data: &[u8]) -> Result<(), String> {
        if self.fail.get() {
            return Err("simulated write failure".into());
        }
        self.files.borrow_mut().insert(id.into(), data.to_vec());
        Ok(())
    }
}

#[test]
fn persistence_restores_utf16_and_retries_failed_origin_writes() {
    let disk = Rc::new(Disk::default());
    let profile = StorageProfile::with_persistence(disk.clone());
    let mut source = realm("https://example.test/", &TabStorage::new(profile.clone()));
    source
        .eval(r#"localStorage.setItem('\udfff','\ud800');sessionStorage.item='transient';"#)
        .unwrap();
    disk.fail.set(true);
    assert!(profile.flush().is_err());
    drop(source);
    assert!(disk.files.borrow().is_empty());
    disk.fail.set(false);
    profile.flush().unwrap();
    let tab = TabStorage::new(StorageProfile::with_persistence(disk.clone()));
    let mut restored = realm("https://example.test/again", &tab);
    check(
        &mut restored,
        r#"localStorage.key(0).charCodeAt(0) === 57343 &&
      localStorage.getItem('\udfff').charCodeAt(0) === 55296 &&
      sessionStorage.length === 0"#,
    );
    drop(restored);
    drop(tab);
    disk.files
        .borrow_mut()
        .values_mut()
        .next()
        .unwrap()
        .truncate(4);
    let tab = TabStorage::new(StorageProfile::with_persistence(disk.clone()));
    let mut corrupted = realm("https://example.test/again", &tab);
    check(
        &mut corrupted,
        "var blocked; try{localStorage.setItem('new', 'item')}catch(error){blocked=error.name}; blocked==='SecurityError'",
    );
    assert_eq!(disk.files.borrow().values().next().unwrap().len(), 4);
}
