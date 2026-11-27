use crate::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn classic_v8_scripts_share_lexical_state_and_mutate_the_real_dom() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"<!doctype html><title>Before</title><p id="status">Start</p>
        <script>
          let runs = 1;
          const item = document.createElement('span');
          item.textContent = ': V8';
          document.getElementById('status').appendChild(item);
          throw new Error('V8 continued after this script');
        </script>
        <script>
          const status = document.getElementById('status');
          if (document === window.document && item === status.lastChild &&
              typeof WebAssembly.Module === 'function') {
            document.title = status.textContent + ' ' + (++runs);
          }
        </script>"#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.vm.enable_v8_document().unwrap();
    runtime.run_initial(&page);

    assert_eq!(
        runtime.eval("document.title").unwrap(),
        Value::from("Start: V8 2")
    );
    assert_eq!(
        runtime.eval("typeof runs").unwrap(),
        Value::from("undefined")
    );
    assert!(
        runtime
            .diagnostics()
            .iter()
            .any(|error| error.contains("V8 continued after this script"))
    );
}
