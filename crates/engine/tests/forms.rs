use rust_browser_engine::{
    LoadedPage,
    dom::NodeId,
    history::{NavigationMode, ScriptNavigation},
    script::{FormKey, ScriptRuntime},
};
use rust_browser_javascript::Value;

fn realm(source: &str) -> (LoadedPage, ScriptRuntime) {
    let page = LoadedPage::from_html("https://forms.test/path/page?old=1", source);
    let runtime = ScriptRuntime::new(&page).unwrap();
    (page, runtime)
}
fn js(runtime: &mut ScriptRuntime, source: &str, expected: impl Into<Value>) {
    assert_eq!(runtime.eval(source).unwrap(), expected.into(), "{source}");
}
fn id(page: &LoadedPage, name: &str) -> NodeId {
    page.document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some(name))
        .unwrap()
}

#[test]
fn dirty_input_textarea_defaults_and_reset_do_not_rewrite_content_attributes() {
    let (mut page, mut runtime) = realm(
        "<!doctype html><form id=f><input id=i value=original><textarea id=t>old\ntext</textarea></form>",
    );
    js(
        &mut runtime,
        "var i=document.getElementById('i');var t=document.getElementById('t');var f=document.getElementById('f');i.value='typed';i.setAttribute('value','new default');t.value='A\\r\\nB';t.defaultValue='reset text';i.value==='typed' && i.defaultValue==='new default' && t.value==='A\\nB'",
        true,
    );
    runtime.snapshot(&mut page);
    assert_eq!(
        page.document.attr(id(&page, "i"), "value"),
        Some("new default")
    );
    assert_eq!(
        page.form_controls.get(&id(&page, "i")).unwrap().value,
        "typed"
    );
    js(
        &mut runtime,
        "f.reset();i.value==='new default' && t.value==='reset text'",
        true,
    );
}

#[test]
fn checkbox_and_radio_activation_precedes_click_and_rolls_back_on_cancellation() {
    let (page, mut runtime) = realm(
        "<!doctype html><form><input id=c type=checkbox><input id=a type=radio name=r checked><input id=b type=radio name=r></form>",
    );
    js(
        &mut runtime,
        "var c=document.getElementById('c');var a=document.getElementById('a');var b=document.getElementById('b');var events='';c.addEventListener('click',function(e){events+=c.checked?'T':'F';e.preventDefault();});c.addEventListener('input',function(){events+='i';});",
        Value::Undefined,
    );
    assert!(!runtime.click(id(&page, "c")));
    js(&mut runtime, "events==='T' && c.checked===false", true);
    js(
        &mut runtime,
        "b.addEventListener('input',function(){events+='i';});b.addEventListener('change',function(){events+='c';});",
        Value::Undefined,
    );
    assert!(runtime.click(id(&page, "b")));
    js(
        &mut runtime,
        "b.checked && !a.checked && events==='Tic'",
        true,
    );
    js(
        &mut runtime,
        "a.setAttribute('checked','');a.checked && !b.checked",
        true,
    );
    js(
        &mut runtime,
        "b.defaultChecked=true;!b.checked && a.checked",
        true,
    );
    js(&mut runtime, "b.checked=true;b.checked && !a.checked", true);
}

#[test]
fn form_ownership_disabled_fieldset_legend_and_selected_options_define_successful_entries() {
    let (_, mut runtime) = realm(
        "<!doctype html><input form=f name=external value=one><form id=f action='/search?discard=yes'><fieldset disabled><legend><input name=legend value=yes></legend><input name=blocked value=no></fieldset><input name=off type=checkbox><input name=on type=checkbox checked><select name=choice><option value=a>A<option selected value=b>B<option value=c disabled>C</select><textarea name=lines>a\nb</textarea><button id=go name=button value=go>Go</button></form>",
    );
    js(
        &mut runtime,
        "var f=document.getElementById('f');document.getElementById('go').form===f && f.elements.length===9",
        true,
    );
    runtime
        .eval("f.requestSubmit(document.getElementById('go'));")
        .unwrap();
    let submissions = runtime.take_form_submissions();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].method, "GET");
    assert_eq!(
        submissions[0].url,
        "https://forms.test/search?external=one&legend=yes&on=on&choice=b&lines=a%0D%0Ab&button=go"
    );
    assert!(submissions[0].body.is_none());
}

#[test]
fn submit_cancellation_submitter_override_and_direct_submit_have_distinct_behavior() {
    let (_, mut runtime) = realm(
        "<!doctype html><form id=f action='/default'><input name=q value='a b'><button id=go name=commit value=yes formaction='/post?keep=1' formmethod=post>Go</button></form>",
    );
    runtime.eval("var f=document.getElementById('f');var go=document.getElementById('go');var seen=false;var cancel=function(e){seen=e.submitter===go;e.preventDefault();};f.addEventListener('submit',cancel);f.requestSubmit(go);").unwrap();
    assert!(runtime.take_form_submissions().is_empty());
    js(&mut runtime, "seen", true);
    runtime
        .eval("f.removeEventListener('submit',cancel);f.requestSubmit(go);")
        .unwrap();
    let submissions = runtime.take_form_submissions();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].url, "https://forms.test/post?keep=1");
    assert_eq!(submissions[0].method, "POST");
    assert_eq!(
        submissions[0].content_type.as_deref(),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        submissions[0].body.as_deref(),
        Some(b"q=a+b&commit=yes".as_slice())
    );
    runtime.eval("f.addEventListener('submit',function(e){throw new Error('must not run');});f.submit();").unwrap();
    let direct = runtime.take_form_submissions();
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].url, "https://forms.test/default?q=a+b");
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn required_validation_reset_cancellation_and_reentrant_submit_are_bounded() {
    let (_, mut runtime) = realm(
        "<!doctype html><form id=f action='/send'><input id=i name=q required value=''><button id=b>Send</button></form>",
    );
    runtime.eval("var f=document.getElementById('f');var i=document.getElementById('i');var invalid=0;i.addEventListener('invalid',function(){invalid++;});f.requestSubmit();").unwrap();
    assert!(runtime.take_form_submissions().is_empty());
    js(&mut runtime, "invalid", 1);
    runtime
        .eval(
            "i.value='new';f.addEventListener('reset',function(e){e.preventDefault();});f.reset();",
        )
        .unwrap();
    js(&mut runtime, "i.value", "new");
    runtime
        .eval("f.addEventListener('submit',function(){f.requestSubmit();});f.requestSubmit();")
        .unwrap();
    assert_eq!(runtime.take_form_submissions().len(), 1);
    js(
        &mut runtime,
        "var other=document.createElement('button');var name='';try{f.requestSubmit(other);}catch(e){name=e.name;}name",
        "NotFoundError",
    );
}

#[test]
fn native_editor_selection_surrogate_backspace_beforeinput_and_blur_change_are_real() {
    let (mut page, mut runtime) =
        realm("<!doctype html><input id=i maxlength=8><textarea id=t></textarea>");
    let input = id(&page, "i");
    let textarea = id(&page, "t");
    runtime.eval("var i=document.getElementById('i');var events='';i.addEventListener('input',function(e){events+=e.inputType+';';});i.addEventListener('change',function(){events+='change;';});").unwrap();
    assert!(runtime.focus_control(Some(input)).unwrap());
    assert!(runtime.insert_form_text("A😀B").unwrap());
    runtime
        .form_keydown(FormKey::Backspace, false, false)
        .unwrap();
    runtime
        .form_keydown(FormKey::Backspace, false, false)
        .unwrap();
    js(
        &mut runtime,
        "i.value==='A' && i.selectionStart===1 && document.activeElement===i",
        true,
    );
    runtime.eval("i.addEventListener('beforeinput',function(e){if(e.data==='blocked')e.preventDefault();});").unwrap();
    runtime.insert_form_text("blocked").unwrap();
    js(&mut runtime, "i.value", "A");
    runtime.focus_control(Some(textarea)).unwrap();
    js(&mut runtime, "events.indexOf('change;')>=0", true);
    runtime.insert_form_text("line1").unwrap();
    runtime.form_keydown(FormKey::Enter, false, false).unwrap();
    runtime.insert_form_text("line2").unwrap();
    runtime.snapshot(&mut page);
    assert_eq!(
        page.form_controls.get(&textarea).unwrap().value,
        "line1\nline2"
    );
}

#[test]
fn select_value_index_defaults_and_dirty_clone_keep_separate_current_state() {
    let (_, mut runtime) = realm(
        "<!doctype html><form id=f><select id=s><option id=a value=a>A<option id=b value=b selected>B</select><input id=i value=default></form>",
    );
    js(
        &mut runtime,
        "var s=document.getElementById('s');var a=document.getElementById('a');var b=document.getElementById('b');s.value==='b' && s.selectedIndex===1 && s.options.item(0)===a",
        true,
    );
    js(
        &mut runtime,
        "s.value='missing';s.selectedIndex===-1 && s.value===''",
        true,
    );
    js(
        &mut runtime,
        "a.selected=true;s.value==='a' && !b.selected",
        true,
    );
    js(
        &mut runtime,
        "var i=document.getElementById('i');i.value='dirty';var c=i.cloneNode();c.value==='dirty' && c.defaultValue==='default'",
        true,
    );
    runtime.eval("c.value='clone';").unwrap();
    js(&mut runtime, "i.value", "dirty");
    js(
        &mut runtime,
        "document.getElementById('f').reset();s.value==='b' && i.value==='default'",
        true,
    );
}

#[test]
fn implicit_enter_and_keyboard_focus_order_use_controls_and_submitter() {
    let (page, mut runtime) = realm(
        "<!doctype html><form action='/query'><input id=last name=q tabindex=2><input id=first tabindex=1><input id=skip disabled tabindex=0><input type=hidden><button id=send name=go value=one>Send</button></form>",
    );
    assert!(runtime.focus_control(Some(id(&page, "last"))).unwrap());
    runtime.insert_form_text("hello").unwrap();
    runtime.form_keydown(FormKey::Enter, false, false).unwrap();
    let submissions = runtime.take_form_submissions();
    assert_eq!(
        submissions[0].url,
        "https://forms.test/query?q=hello&go=one"
    );
    runtime.focus_control(None).unwrap();
    runtime.focus_next_control(false).unwrap();
    assert_eq!(runtime.focused_control(), Some(id(&page, "first")));
    runtime.focus_next_control(false).unwrap();
    assert_eq!(runtime.focused_control(), Some(id(&page, "last")));
    runtime.focus_next_control(true).unwrap();
    assert_eq!(runtime.focused_control(), Some(id(&page, "first")));
}

#[test]
fn pointer_options_single_multiple_toggle_range_and_cancellation_events() {
    let (page, mut runtime) = realm(
        "<!doctype html><select id=s><option id=a value=a selected disabled>A<option id=b value=b>B</select><select id=m multiple><option id=x>X<option id=y>Y<option id=z>Z</select>",
    );
    runtime.eval("var changes=0;document.getElementById('s').addEventListener('change',function(){changes++;});").unwrap();
    runtime
        .choose_form_option(id(&page, "s"), id(&page, "b"), false, false)
        .unwrap();
    js(
        &mut runtime,
        "document.getElementById('s').value==='b' && !document.getElementById('a').selected && changes===1",
        true,
    );
    runtime
        .choose_form_option(id(&page, "m"), id(&page, "x"), false, false)
        .unwrap();
    runtime
        .choose_form_option(id(&page, "m"), id(&page, "z"), true, false)
        .unwrap();
    js(
        &mut runtime,
        "document.getElementById('m').selectedOptions.length",
        2,
    );
    runtime
        .choose_form_option(id(&page, "m"), id(&page, "y"), false, true)
        .unwrap();
    js(
        &mut runtime,
        "!document.getElementById('x').selected && document.getElementById('y').selected && document.getElementById('z').selected",
        true,
    );
}

#[test]
fn submit_from_cancelled_submit_handler_bypasses_events_and_reentrant_getters_are_safe() {
    let (_, mut runtime) =
        realm("<!doctype html><form id=f action='/manual'><input id=i name=x value=one></form>");
    js(
        &mut runtime,
        "var f=document.getElementById('f');var i=document.getElementById('i');i.value={toString:function(){i.defaultValue='default';return 'two';}};f.addEventListener('submit',function(e){e.preventDefault();f.submit();});f.requestSubmit();i.value==='two' && i.defaultValue==='default'",
        true,
    );
    let submissions = runtime.take_form_submissions();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].url, "https://forms.test/manual?x=two");
    js(&mut runtime, "i.value=undefined;i.value", "undefined");
    js(&mut runtime, "i.value=null;i.value", "");
}

#[test]
fn required_radio_constraint_is_shared_and_default_attribute_does_not_erase_dirty_value() {
    let (_, mut runtime) = realm(
        "<!doctype html><input id=a type=radio name=g required><input id=b type=radio name=g><select id=s><option id=x selected>X<option id=y>Y</select>",
    );
    js(
        &mut runtime,
        "var a=document.getElementById('a');var b=document.getElementById('b');!a.checkValidity() && !b.checkValidity()",
        true,
    );
    js(
        &mut runtime,
        "b.checked=true;a.checkValidity() && b.checkValidity()",
        true,
    );
    js(
        &mut runtime,
        "var y=document.getElementById('y');y.selected=true;y.defaultSelected=false;y.selected",
        true,
    );
}

#[test]
fn large_static_radio_snapshot_is_indexed_and_preserves_single_group_selection() {
    let mut source = String::from("<!doctype html><form id=f>");
    for index in 0..9000 {
        source.push_str(&format!(
            "<input type=radio name=shared value={index}{}>",
            if index == 8999 { " checked" } else { "" }
        ));
    }
    source.push_str("</form>");
    let start = std::time::Instant::now();
    let page = LoadedPage::from_html("https://forms.test/", &source);
    assert_eq!(page.form_controls.len(), 9000);
    assert_eq!(
        page.form_controls
            .values()
            .filter(|control| control.checked)
            .count(),
        1
    );
    assert_eq!(
        page.form_controls
            .values()
            .find(|control| control.checked)
            .unwrap()
            .value,
        "8999"
    );
    eprintln!("Indexed forms snapshot9000 radios: {:?}", start.elapsed());
}

#[test]
fn disabled_by_click_handler_suppresses_submit_and_resource_failure_suppresses_reset() {
    let (_, mut runtime) = realm(
        "<!doctype html><form id=f action='/send'><input id=i name=q value=default><button id=b>Send</button></form>",
    );
    runtime.eval("var f=document.getElementById('f');var i=document.getElementById('i');var b=document.getElementById('b');b.onclick=function(){b.disabled=true;};b.click();").unwrap();
    assert!(runtime.take_form_submissions().is_empty());
    let error=runtime.eval("i.value='dirty';var fail=function(){'x'.repeat(10000000);};f.addEventListener('reset',fail);f.reset();").unwrap_err();
    assert_eq!(
        error.kind,
        rust_browser_javascript::ErrorKind::ExecutionLimit
    );
    js(&mut runtime, "i.value", "dirty");
    runtime
        .eval("f.removeEventListener('reset',fail);f.reset();")
        .unwrap();
    js(&mut runtime, "i.value", "default");
}

#[test]
fn resets_clear_pending_user_change_and_invalid_enum_defaults_are_url_encoded() {
    let (page, mut runtime) = realm(
        "<!doctype html><form id=f action='/post' method=post enctype=unknown><input id=i name=q value=old><select id=s><option>A<option>B</select></form>",
    );
    runtime.eval("var f=document.getElementById('f');var i=document.getElementById('i');var changes=0;i.onchange=function(){changes++;};").unwrap();
    runtime.focus_control(Some(id(&page, "i"))).unwrap();
    runtime.insert_form_text("new").unwrap();
    runtime.eval("f.reset();").unwrap();
    runtime.focus_control(None).unwrap();
    js(&mut runtime, "changes", 0);
    js(
        &mut runtime,
        "var s=document.getElementById('s');s.selectedIndex=100;s.selectedIndex===-1 && s.value===''",
        true,
    );
    runtime.eval("f.submit();").unwrap();
    let submissions = runtime.take_form_submissions();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].body.as_deref(), Some(b"q=old".as_slice()));
}

#[test]
fn required_select_placeholder_rules_do_not_reject_valid_empty_values() {
    let (_, mut runtime) = realm(
        "<!doctype html><select id=m required multiple><option selected value=''>empty<option selected value=x>X</select><select id=list required size=2><option selected value=''>empty</select><select id=single required><option value=''>placeholder<option id=later value=''>empty later</select><select id=group required><optgroup><option selected value=''>group empty</optgroup></select>",
    );
    js(
        &mut runtime,
        "document.getElementById('m').checkValidity() && document.getElementById('list').checkValidity() && document.getElementById('group').checkValidity()",
        true,
    );
    js(
        &mut runtime,
        "var single=document.getElementById('single');!single.checkValidity()",
        true,
    );
    js(
        &mut runtime,
        "document.getElementById('later').selected=true;single.checkValidity()",
        true,
    );
}

#[test]
fn detached_form_ownership_reset_and_collections_work_before_connection() {
    let (_, mut runtime) = realm("<!doctype html><body></body>");
    js(
        &mut runtime,
        "var f=document.createElement('form');var i=document.createElement('input');i.defaultValue='default';i.setAttribute('form','unresolved');f.appendChild(i);i.value='dirty';i.form===f && f.elements.length===1 && f.length===1",
        true,
    );
    js(&mut runtime, "f.reset();i.value", "default");
    runtime.eval("document.body.appendChild(f);").unwrap();
    js(&mut runtime, "i.form===null && document.forms[0]===f", true);
}

#[test]
fn tab_keydown_can_cancel_native_focus_traversal_and_shift_reverses_it() {
    let (page, mut runtime) = realm("<!doctype html><input id=a><input id=b>");
    runtime.focus_control(Some(id(&page, "a"))).unwrap();
    runtime.eval("var a=document.getElementById('a');var b=document.getElementById('b');var keys='';a.onkeydown=function(e){keys+=e.key;e.preventDefault();};").unwrap();
    assert!(runtime.form_keydown(FormKey::Tab, false, false).unwrap());
    assert_eq!(runtime.focused_control(), Some(id(&page, "a")));
    js(&mut runtime, "keys", "Tab");
    runtime
        .eval("a.onkeydown=null;b.onkeydown=function(e){keys+=e.shiftKey?'reverse':'forward';};")
        .unwrap();
    assert!(runtime.form_keydown(FormKey::Tab, false, false).unwrap());
    assert_eq!(runtime.focused_control(), Some(id(&page, "b")));
    assert!(runtime.form_keydown(FormKey::Tab, true, false).unwrap());
    assert_eq!(runtime.focused_control(), Some(id(&page, "a")));
    js(&mut runtime, "keys", "Tabreverse");
}

#[test]
fn focused_html_link_enter_honors_cancellation_and_uses_the_live_href() {
    let (page, mut runtime) = realm(
        "<!doctype html><base href='/base/'><button id=before>Before</button>\
        <a href=skip tabindex=-1>Skip</a><a id=link href=original>Link</a>\
        <button id=after>After</button>",
    );
    runtime
        .eval(
            "var link=document.getElementById('link');var clicks=0;\
            link.onkeydown=function(e){if(e.key==='Enter')e.preventDefault();};\
            link.onclick=function(e){\
                if(++clicks===1)e.preventDefault();\
                else link.setAttribute('href','replacement#target');\
            };",
        )
        .unwrap();
    for (shift, target) in [
        (false, "before"),
        (false, "link"),
        (false, "after"),
        (true, "link"),
    ] {
        assert!(runtime.form_keydown(FormKey::Tab, shift, false).unwrap());
        assert_eq!(runtime.focused_control(), Some(id(&page, target)));
    }
    js(&mut runtime, "document.activeElement===link", true);
    assert!(!runtime.form_keydown(FormKey::Space, false, false).unwrap());
    assert!(runtime.form_keydown(FormKey::Enter, false, false).unwrap());
    assert!(runtime.take_navigation_request().is_none());
    js(&mut runtime, "clicks", 0.0);

    runtime.eval("link.onkeydown=null;").unwrap();
    assert!(runtime.form_keydown(FormKey::Enter, false, false).unwrap());
    assert!(runtime.take_navigation_request().is_none());
    js(&mut runtime, "clicks", 1.0);

    assert!(runtime.form_keydown(FormKey::Enter, false, false).unwrap());
    assert!(matches!(
        runtime.take_navigation_request(),
        Some(ScriptNavigation::Navigate { url, mode: NavigationMode::Push })
            if url.as_str() == "https://forms.test/base/replacement#target"
    ));
    assert!(runtime.take_navigation_request().is_none());
    js(&mut runtime, "clicks", 2.0);
}

#[test]
fn placeholder_shown_tracks_default_dirty_and_user_edited_values() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"<!doctype html>
        <style>#action {display:none} #search:not(:placeholder-shown) ~ #action {display:flex}</style>
        <form><input id=search type=search placeholder=""><button id=action>X</button>
        <textarea id=area placeholder="type">seed</textarea>
        <input id=without><input id=num type=number value=invalid placeholder="0">
        <input id=check type=checkbox placeholder=x></form>"#,
    );
    let search = page.document.html_elements("input").next().unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval("var search=document.getElementById('search'), area=document.getElementById('area'), action=document.getElementById('action'); function clearVisible(){return getComputedStyle(action).display==='flex'}").unwrap();
    let check =
        |r: &mut ScriptRuntime, s: &str| assert_eq!(r.eval(s).unwrap(), Value::Bool(true), "{s}");
    check(
        &mut runtime,
        "search.matches(':placeholder-shown') && !clearVisible() && document.querySelector(':placeholder-shown')===search && !area.matches(':placeholder-shown')",
    );
    check(
        &mut runtime,
        "!document.getElementById('without').matches(':placeholder-shown') && document.getElementById('num').matches(':placeholder-shown') && !document.getElementById('check').matches(':placeholder-shown')",
    );
    runtime.focus_control(Some(search)).unwrap();
    runtime.insert_form_text("q").unwrap();
    check(
        &mut runtime,
        "clearVisible() && !search.matches(':placeholder-shown')",
    );
    runtime
        .form_keydown(FormKey::Backspace, false, false)
        .unwrap();
    check(
        &mut runtime,
        "search.matches(':placeholder-shown') && !clearVisible()",
    );
    check(
        &mut runtime,
        "search.value='test'; area.value=''; area.matches(':placeholder-shown') && clearVisible()",
    );
    check(
        &mut runtime,
        "search.setAttribute('value',''); clearVisible() && search.getAttribute('value')===''",
    );
    check(
        &mut runtime,
        "search.form.reset(); !clearVisible() && !area.matches(':placeholder-shown')",
    );
    check(
        &mut runtime,
        "area.textContent=''; area.matches(':placeholder-shown')",
    );
    check(
        &mut runtime,
        "area.removeAttribute('placeholder'); !area.matches(':placeholder-shown')",
    );
}
