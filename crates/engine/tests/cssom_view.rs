use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use rust_browser_javascript::{ErrorKind, Value};
use std::sync::Arc;

fn runtime(source: &str) -> ScriptRuntime {
    let page = LoadedPage::from_html("https://example.test/", source);
    let runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 800.,
        height: 200.,
        ..Default::default()
    });
    runtime
}
fn check(runtime: &mut ScriptRuntime, source: &str) {
    assert_eq!(
        runtime
            .eval(source)
            .unwrap_or_else(|e| panic!("{source}: {e}")),
        Value::Bool(true),
        "{source}"
    );
}

fn runner_controls() -> ScriptRuntime {
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        include_str!("fixtures/cssom_runner_controls.html"),
    );
    ScriptRuntime::for_page_with_viewport(
        &mut page,
        Viewport {
            width: 800.,
            height: 200.,
            ..Default::default()
        },
    )
    .unwrap()
    .unwrap()
}

#[test]
fn owner_document_view_exposes_computed_style_for_runner_controls() {
    let mut r = runner_controls();
    check(
        &mut r,
        "showRunPanel(); var fill=document.getElementById('fill');
         fill.style.width==='25%' && fill.getBoundingClientRect().width===40",
    );
    check(
        &mut r,
        "document.defaultView===window && document.defaultView.document===document &&
         document.createElement('div').ownerDocument.defaultView===window",
    );
}

#[test]
fn repeated_runner_progress_width_does_not_mutate_or_invalidate_layout() {
    let mut r = runner_controls();
    check(
        &mut r,
        "document.getElementById('run-panel').style.display='block';
         var fill=document.getElementById('fill');
         var progressObserver=new MutationObserver(function(){});
         progressObserver.observe(fill,{attributes:true,attributeFilter:['style']});
         setProgress(1,16436);
         progressObserver.takeRecords().length===1 && fill.offsetWidth===0",
    );
    let first = r.layout_for_viewport(r.viewport()).unwrap();
    check(
        &mut r,
        "setProgress(2,16436);
         progressObserver.takeRecords().length===0 && fill.style.width==='0%'",
    );
    assert!(r.layout_is_current(&first));
    check(
        &mut r,
        "setProgress(500,16436);
         progressObserver.takeRecords().length===1 && fill.style.width==='3%'",
    );
    assert!(r.layout_needs_refresh());
    check(&mut r, "fill.offsetWidth===5");
    check(
        &mut r,
        "fill.style.setProperty('width','3%','important');
         progressObserver.takeRecords().length===1 &&
         fill.style.getPropertyPriority('width')==='important'",
    );
}

#[test]
fn unchanged_cssom_declaration_leaves_authored_style_attribute_untouched() {
    let mut r = runtime(
        "<!doctype html><div style='width:160px'>
         <div id=fill style='height: 10px; WIDTH : 25% '></div></div>",
    );
    check(
        &mut r,
        "var fill=document.getElementById('fill');
         var original=fill.getAttribute('style');
         var observer=new MutationObserver(function(){});
         observer.observe(fill,{attributes:true,attributeFilter:['style'],attributeOldValue:true});
         fill.style.width='25%';
         observer.takeRecords().length===0 && fill.getAttribute('style')===original &&
         fill.offsetWidth===40",
    );
    check(
        &mut r,
        "fill.setAttribute('style',original);
         var records=observer.takeRecords();
         records.length===1 && records[0].oldValue===original",
    );
}

#[test]
fn reapplying_width_can_change_its_precedence() {
    let mut r = runtime(
        "<!doctype html><div style='width:160px'>
         <div id=alias style='width:25%;inline-size:80px'></div>
         <div id=duplicate style='width:80px!important;width:25%'></div></div>",
    );
    check(
        &mut r,
        "var alias=document.getElementById('alias'), duplicate=document.getElementById('duplicate');
         alias.offsetWidth===80 && duplicate.offsetWidth===80",
    );
    check(
        &mut r,
        "alias.style.width='25%'; duplicate.style.width='25%';
         alias.offsetWidth===40 && duplicate.offsetWidth===40",
    );
}

#[test]
fn compat_mode_selects_the_viewport_height_for_runner_overlays() {
    let mut r = runner_controls();
    check(
        &mut r,
        "resizeOverlay();
         document.getElementById('overlay').getBoundingClientRect().height===180 &&
         document.body.clientHeight===60 && document.compatMode==='CSS1Compat'",
    );
}

#[test]
fn compat_mode_distinguishes_quirks_from_limited_quirks() {
    let mut r = runtime("<body><p>No doctype</p>");
    check(
        &mut r,
        "document.compatMode==='BackCompat' && document.body.clientHeight===innerHeight",
    );
    let mut r = runtime(
        "<!DOCTYPE html PUBLIC '-//W3C//DTD XHTML 1.0 Transitional//EN' 'http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd'><p>Limited quirks</p>",
    );
    check(
        &mut r,
        "document.compatMode==='CSS1Compat' && document.documentElement.clientHeight===innerHeight",
    );
}

#[test]
fn geometry_charges_displayed_text_and_allows_large_inert_data_blocks() {
    let html = format!(
        "<!doctype html><body><script id='data' type='application/json'>\"{}\"</script><p>Page</p>",
        "x".repeat(1_100_000),
    );
    let mut r = runtime(&html);
    check(&mut r, "document.body.clientWidth > 0");
    let error = r
        .eval("document.getElementById('data').style.display='block'; document.body.clientWidth")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ExecutionLimit);
}

#[test]
fn geometry_uses_border_boxes_but_offsets_ignore_transforms() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}#box{width:100px;height:50px;padding:10px;border:2px solid red;margin:5px;transform:scale(2);transform-origin:0 0}</style><div id=box></div>",
    );
    check(
        &mut r,
        "var box=document.getElementById('box');var rect=box.getBoundingClientRect();rect.x===5&&rect.y===5&&rect.width===248&&rect.height===148",
    );
    check(
        &mut r,
        "box.offsetWidth===124&&box.offsetHeight===74&&box.clientWidth===120&&box.clientHeight===70&&box.clientLeft===2&&box.clientTop===2",
    );
    check(
        &mut r,
        "var style=getComputedStyle(box);style.width==='100px'&&style.height==='50px'&&style.paddingTop==='10px'&&style.borderLeftWidth==='2px'&&style.transform==='matrix(2, 0, 0, 2, 0, 0)'",
    );
    check(
        &mut r,
        "var list=box.getClientRects();list.length===1&&list.item(0).width===248&&list.item(99)===null&&Array.from(list)[0].height===148",
    );
    check(
        &mut r,
        "rect.x=20;rect.width=-5;rect.left===15&&rect.right===20&&rect.toJSON().left===15",
    );
}
#[test]
fn layout_cache_is_shared_with_render_and_computed_styles_remain_live() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}#box{width:100px;height:20px;--ink:green;color:var(--ink)}</style><div id=box></div>",
    );
    let first = r.layout_for_viewport(r.viewport()).unwrap();
    check(
        &mut r,
        "var box=document.getElementById('box');var before=box.getBoundingClientRect();var style=getComputedStyle(box);style.getPropertyValue('--ink')==='green'&&style.color==='rgb(0, 128, 0)'",
    );
    let second = r.layout_for_viewport(r.viewport()).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    check(
        &mut r,
        "box.style.width='140px';before.width===100&&box.getBoundingClientRect().width===140&&style.width==='140px'",
    );
    let third = r.layout_for_viewport(r.viewport()).unwrap();
    assert!(!Arc::ptr_eq(&first, &third));
    let mut page = LoadedPage::from_html("https://example.test/", "");
    assert!(r.snapshot(&mut page));
    assert!(!r.snapshot(&mut page));
    assert!(Arc::ptr_eq(
        &third,
        &r.layout_for_viewport(r.viewport()).unwrap()
    ));
    check(
        &mut r,
        "var readonly=false;try{style.setProperty('width','999px')}catch(e){readonly=e.name==='NoModificationAllowedError'}readonly&&style.width==='140px'",
    );
}
#[test]
fn hidden_detached_and_empty_elements_have_no_client_boxes() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}</style><div id=hidden style='display:none;width:100px'></div><div id=zero style='height:0;width:0'></div>",
    );
    check(
        &mut r,
        "var h=document.getElementById('hidden');h.getBoundingClientRect().width===0&&h.getBoundingClientRect().x===0&&h.getClientRects().length===0&&h.offsetWidth===0",
    );
    check(
        &mut r,
        "var detached=document.createElement('div');detached.style.width='20px';detached.getBoundingClientRect().height===0&&detached.offsetParent===null&&getComputedStyle(detached).width===''",
    );
    check(
        &mut r,
        "var zero=document.getElementById('zero');zero.getBoundingClientRect().width===0&&zero.getBoundingClientRect().height===0",
    );
}
#[test]
fn nested_and_root_scroll_update_the_same_geometry_and_fixed_boxes() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}#scroller{width:100px;height:40px;overflow:auto}#child{height:100px}#spacer{height:1200px}#fixed{position:fixed;left:0;top:10px;width:20px;height:20px}</style><div id=scroller><div id=child></div></div><div id=spacer></div><div id=fixed></div>",
    );
    check(
        &mut r,
        "var scroller=document.getElementById('scroller');var child=document.getElementById('child');scroller.clientHeight===40&&scroller.scrollHeight===100&&scroller.scrollTop===0",
    );
    check(
        &mut r,
        "scroller.scrollTop=30;scroller.scrollTop===30&&child.getBoundingClientRect().top===-30",
    );
    check(
        &mut r,
        "scroller.scrollBy({top:999});scroller.scrollTop===60&&child.getBoundingClientRect().top===-60",
    );
    check(
        &mut r,
        "scrollTo(0,100);scrollY===100&&pageYOffset===100&&document.documentElement.scrollTop===100&&document.getElementById('fixed').getBoundingClientRect().top===10",
    );
    check(
        &mut r,
        "scrollBy({top:50});scrollY===150&&innerWidth===800&&innerHeight===200&&document.documentElement.clientHeight===200",
    );
    assert_eq!(r.take_scroll_request().unwrap().y, 150.);
}
#[test]
fn scroll_events_are_coalesced_and_run_after_the_current_task() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}#scroller{height:40px;overflow:auto}#child{height:100px}#spacer{height:1200px}</style><div id=scroller><div id=child></div></div><div id=spacer></div>",
    );
    check(
        &mut r,
        "var events=[];var scroller=document.getElementById('scroller');window.addEventListener('scroll',function(e){events.push(e.target===document?'document':'bubble')});scroller.addEventListener('scroll',function(e){events.push('element')});scrollTo(0,10);scrollTo(0,20);scroller.scrollTop=10;scroller.scrollTop=20;events.length===0",
    );
    r.pump();
    check(&mut r, "events.join(',')==='document,element'");
}
#[test]
fn relative_offsets_and_transformed_fixed_containing_blocks() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:8px}#parent{position:relative;left:20px;top:10px;width:100px;height:80px;transform:scale(2);transform-origin:0 0}#relative{position:relative;left:5px;top:3px;width:20px;height:20px}#fixed{position:fixed;left:7px;top:9px;width:10px;height:10px}</style><div id=parent><div id=relative></div><div id=fixed></div></div>",
    );
    check(
        &mut r,
        "var p=document.getElementById('parent');var c=document.getElementById('relative');var f=document.getElementById('fixed');c.offsetParent===p&&c.offsetLeft===5&&c.offsetTop===3&&c.offsetWidth===20&&f.offsetParent===p&&f.offsetLeft===7&&f.offsetTop===9&&document.body.offsetTop===0",
    );
}
#[test]
fn native_layout_work_is_charged_to_the_current_javascript_task() {
    let mut r = runtime("<!doctype html><div id=box>Budget</div>");
    // Every write invalidates layout; repeating the same declaration is a no-op.
    let error = r
        .eval(
            "var box=document.getElementById('box'), toggle=false;\
             while(true){try{toggle=!toggle;box.style.width=toggle?'20px':'21px';\
             box.getBoundingClientRect()}catch(e){}}",
        )
        .unwrap_err();
    assert_eq!(
        error.kind,
        rust_browser_javascript::ErrorKind::ExecutionLimit
    );
}
#[test]
fn window_resize_and_scroll_handlers_use_current_viewport_and_are_deferred() {
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><style>body{height:1200px}</style><script>var events=[];window.onresize=function(){events.push('resize:'+innerWidth)};window.onscroll=function(){events.push('scroll:'+scrollY)};</script>",
    );
    let mut r = ScriptRuntime::for_page_with_viewport(
        &mut page,
        Viewport {
            width: 800.,
            height: 200.,
            ..Default::default()
        },
    )
    .unwrap()
    .unwrap();
    r.set_viewport(Viewport {
        width: 900.,
        height: 200.,
        scroll_y: 20.,
        ..Default::default()
    });
    check(&mut r, "events.length===0");
    r.pump();
    check(&mut r, "events.join(',')==='resize:900,scroll:20'");
}
#[test]
fn scroll_clamps_are_committed_when_content_shrinks_and_grows() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}#panel{height:50px;overflow:auto}#child{height:1000px}#spacer{height:1200px}</style><div id=panel><div id=child></div></div><div id=spacer></div>",
    );
    check(
        &mut r,
        "var p=document.getElementById('panel');var c=document.getElementById('child');p.scrollTop=500;c.style.height='100px';p.scrollTop===50",
    );
    check(&mut r, "c.style.height='1000px';p.scrollTop===50");
    check(&mut r, "scrollTo(0,300);scrollY===300");
    check(
        &mut r,
        "document.getElementById('spacer').style.height='10px';scrollY===0",
    );
    check(
        &mut r,
        "document.getElementById('spacer').style.height='1200px';scrollY===0",
    );
}
#[test]
fn computed_percentage_insets_use_the_padding_containing_block() {
    let mut r = runtime(
        "<!doctype html><style>body{margin:0}#parent{position:relative;width:200px;height:100px;padding:10px;border:2px solid black}#child{position:absolute;left:50%;top:50%;width:10px;height:10px}</style><div id=parent><div id=child></div></div>",
    );
    check(
        &mut r,
        "var c=document.getElementById('child');var s=getComputedStyle(c);s.top==='60px'&&s.left==='110px'&&c.offsetTop===60&&c.offsetLeft===110",
    );
}
#[test]
fn linked_stylesheet_source_is_included_in_native_layout_fuel() {
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><link rel=stylesheet href='/style.css'><div id=box></div>",
    );
    let sheet: Arc<str> = format!("/*{}*/#box{{height:20px}}", "x".repeat(262144)).into();
    let link = page.document.html_elements("link").next().unwrap();
    page.linked_stylesheets.insert(link, sheet.clone());
    page.stylesheets = Arc::new(vec![sheet]);
    let mut r = ScriptRuntime::new(&page).unwrap();
    let error=r.eval("var box=document.getElementById('box');for(var i=0;i<20;i++){box.style.width=i+'px';box.getBoundingClientRect()}").unwrap_err();
    assert_eq!(
        error.kind,
        rust_browser_javascript::ErrorKind::ExecutionLimit
    );
}
