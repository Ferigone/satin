use rust_browser_engine::{
    LoadedPage,
    dom::NodeId,
    script::{ScriptRuntime, SelectedFile},
};
use rust_browser_javascript::{ErrorKind, Value};

const PAGE: &str = r#"<!doctype html>
<form id="form" enctype="multipart/form-data">
  <label id="label" for="file">Choose files</label>
  <input id="file" name="upload" type="file" accept="image/*,.txt" multiple required>
  <input id="other" type="file">
  <button id="open" type="button">Open file picker</button>
</form>"#;

fn setup() -> (ScriptRuntime, NodeId, NodeId, NodeId) {
    let page = LoadedPage::from_html("https://files.test/", PAGE);
    let node = |id: &str| {
        page.document
            .connected_nodes()
            .find(|&node| page.document.attr(node, "id") == Some(id))
            .unwrap()
    };
    (
        ScriptRuntime::new(&page).unwrap(),
        node("file"),
        node("label"),
        node("open"),
    )
}

fn file(path: &str, name: &str, mime: &str, bytes: &[u8]) -> SelectedFile {
    SelectedFile {
        path: path.into(),
        name: name.into(),
        mime: mime.into(),
        last_modified: 123_456,
        bytes: bytes.to_vec(),
    }
}

fn select(runtime: &mut ScriptRuntime, trigger: NodeId, files: Option<Vec<SelectedFile>>) {
    assert!(runtime.click(trigger));
    let pending = runtime.take_file_picker_request().unwrap();
    assert!(runtime.complete_file_picker(&pending, files).unwrap());
}

fn yes(runtime: &mut ScriptRuntime, source: &str) {
    assert_eq!(runtime.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn file_input_exposes_readonly_indexed_files_and_submits_the_selected_objects() {
    let (mut runtime, input, label, _) = setup();
    yes(
        &mut runtime,
        r#"
        const form = document.getElementById('form');
        const input = document.getElementById('file');
        const empty = input.files;
        const events = [];
        for (const name of ['input', 'change', 'cancel']) {
          form.addEventListener(name, event => events.push([event.type, event.isTrusted, event.composed, event.target === input]));
        }
        let illegal;
        try { new FileList(); } catch (error) { illegal = error.name; }
        input instanceof HTMLInputElement && input.multiple && input.accept === 'image/*,.txt' &&
          empty instanceof FileList && input.files === empty && empty.length === 0 &&
          Object.prototype.toString.call(empty) === '[object FileList]' &&
          FileList.prototype[Symbol.iterator] === Array.prototype.values &&
          document.createElement('input').files === null && document.createElement('div').files === undefined &&
          !form.checkValidity() && illegal === 'TypeError'
    "#,
    );
    assert!(runtime.click(label));
    let request = runtime.take_file_picker_request().unwrap();
    assert_eq!(request.input, input);
    assert!(request.multiple);
    assert_eq!(request.accept, "image/*,.txt");
    assert!(runtime.take_file_picker_request().is_none());
    assert!(
        runtime
            .complete_file_picker(
                &request,
                Some(vec![
                    file("C:/chosen/first.txt", "first.txt", "text/plain", b"hello"),
                    file("C:/chosen/second.bin", "second.bin", "", &[0, 1, 255]),
                ])
            )
            .unwrap()
    );
    yes(
        &mut runtime,
        r#"
        const list = input.files;
        const descriptor = Object.getOwnPropertyDescriptor(list, '0');
        const entries = new FormData(form).getAll('upload');
        const snapshot = Array.from(list);
        list !== empty && list === input.files && empty.length === 0 &&
          list.length === 2 && snapshot[0] === list.item(0) && snapshot[1] === list[1] &&
          list[0] instanceof File && list[0].name === 'first.txt' &&
          list[0].type === 'text/plain' && list[0].size === 5 && list[0].lastModified === 123456 &&
          descriptor.value === list[0] && descriptor.enumerable && descriptor.configurable &&
          !descriptor.writable && !Reflect.deleteProperty(list, '0') &&
          list[2] === undefined && list.item(-1) === null &&
          input.value.split(String.fromCharCode(92)).join('/') === 'C:/fakepath/first.txt' && form.checkValidity() &&
          entries[0] === list[0] && entries[1] === list[1] &&
          JSON.stringify(events) === '[["input",true,true,true],["change",true,false,true]]'
    "#,
    );
    runtime
        .eval("let text; list[0].text().then(result => text = result);")
        .unwrap();
    runtime.pump();
    yes(&mut runtime, "text === 'hello'");
}

#[test]
fn pickers_require_activation_and_identical_reselection_fires_only_cancel() {
    let (mut runtime, _, label, button) = setup();
    yes(
        &mut runtime,
        r#"
        const input = document.getElementById('file');
        let blocked;
        try { input.showPicker(); } catch (error) { blocked = error.name; }
        input.disabled = true;
        let disabled;
        try { input.showPicker(); } catch (error) { disabled = error.name; }
        input.disabled = false;
        input.click();
        const events = [];
        for (let name of ['input', 'change', 'cancel']) {
          input.addEventListener(name, event => events.push(name + ':' + event.isTrusted));
        }
        document.getElementById('open').onclick = () => input.showPicker();
        blocked === 'NotAllowedError' && disabled === 'InvalidStateError'
    "#,
    );
    assert!(runtime.take_file_picker_request().is_none());
    let selection = || {
        Some(vec![file(
            "C:/chosen/one.txt",
            "one.txt",
            "text/plain",
            b"data",
        )])
    };
    select(&mut runtime, label, selection());
    runtime.eval("const retained = input.files;").unwrap();
    select(&mut runtime, button, selection());
    select(&mut runtime, label, None);
    yes(
        &mut runtime,
        r#"
        input.files === retained && retained.length === 1 &&
          events.join(',') === 'input:true,change:true,cancel:true,cancel:true'
    "#,
    );
}

#[test]
fn filelists_clone_with_file_aliases_and_clear_without_exposing_picker_paths() {
    let (mut runtime, input, _, _) = setup();
    select(
        &mut runtime,
        input,
        Some(vec![file(
            "C:/private/location/file.txt",
            "file.txt",
            "text/plain",
            b"immutable",
        )]),
    );
    yes(
        &mut runtime,
        r#"
        const input = document.getElementById('file');
        const other = document.getElementById('other');
        const form = document.getElementById('form');
        const original = input.files;
        const originalFile = original[0];
        const clone = input.cloneNode();
        const copied = structuredClone({files: original, file: originalFile});
        other.files = original;
        other.files = null;
        let rejectedType, rejectedVal;
        try { other.files = []; } catch (error) { rejectedType = error.name; }
        try { other.value = 'X:/private/fake'; } catch (error) { rejectedVal = error.name; }
        const assigned = other.files === original && other.value === input.value;
        input.value = '';
        const cleared = input.files === original && original.length === 0 && other.files.length === 0;
        input.files = clone.files;
        const preserved = input.files === clone.files && clone.files !== original &&
          clone.files[0] === originalFile && input.value.split(String.fromCharCode(92)).join('/') === 'C:/fakepath/file.txt';
        form.reset();
        assigned && cleared && preserved && clone.files.length === 0 &&
          copied.files instanceof FileList && copied.files !== original &&
          copied.files[0] === copied.file && copied.file !== originalFile &&
          copied.file.name === 'file.txt' && copied.file.size === 9 &&
          !('path' in copied.file) && copied.file.webkitRelativePath !== 'C:/private/location/' &&
          rejectedType === 'TypeError' && rejectedVal === 'InvalidStateError'
    "#,
    );
}

#[test]
fn stale_and_failed_picker_results_preserve_the_current_selection() {
    let (mut runtime, input, _, _) = setup();
    assert!(runtime.click(input));
    let stale = runtime.take_file_picker_request().unwrap();
    yes(
        &mut runtime,
        r#"
        const input = document.getElementById('file');
        input.type = 'text';
        const detached = input.files === null;
        input.type = 'file';
        detached && input.files.length === 0
    "#,
    );
    assert!(
        !runtime
            .complete_file_picker(
                &stale,
                Some(vec![file("C:/stale.txt", "stale.txt", "", b"ignored",)])
            )
            .unwrap()
    );
    select(
        &mut runtime,
        input,
        Some(vec![file("C:/kept.txt", "kept.txt", "", b"kept")]),
    );
    runtime
        .eval("const prior = input.files; input.multiple = false;")
        .unwrap();
    assert!(runtime.click(input));
    let pending = runtime.take_file_picker_request().unwrap();
    assert!(!pending.multiple);
    assert_eq!(
        runtime
            .complete_file_picker(
                &pending,
                Some(vec![
                    file("C:/a.txt", "a.txt", "", b"a"),
                    file("C:/b.txt", "b.txt", "", b"b"),
                ])
            )
            .unwrap_err()
            .kind,
        ErrorKind::ExecutionLimit,
    );
    yes(
        &mut runtime,
        "input.files === prior && input.value.split(String.fromCharCode(92)).join('/') === 'C:/fakepath/kept.txt'",
    );
}
