use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::time::{Duration, Instant};

fn runtime(html: &str) -> (LoadedPage, ScriptRuntime) {
    let page = LoadedPage::from_html("https://example.test/", html);
    let runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 200.,
        height: 160.,
        ..Viewport::default()
    });
    (page, runtime)
}

fn check(runtime: &mut ScriptRuntime, expression: &str) {
    assert_eq!(runtime.eval(expression).unwrap(), Value::Bool(true));
}

fn wait_for(runtime: &mut ScriptRuntime, expression: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        runtime.pump();
        if runtime.eval(expression).unwrap() == Value::Bool(true) {
            assert!(
                runtime.diagnostics().is_empty(),
                "{:?}",
                runtime.diagnostics()
            );
            return;
        }
        let next = runtime
            .next_deadline()
            .expect("observer task still pending");
        assert!(
            next <= deadline && Instant::now() < deadline,
            "{expression}: {:?}",
            runtime.diagnostics()
        );
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

#[test]
fn horizontal_viewport_scroll_reuses_the_scene_for_paint_hits_and_observers() {
    use rust_browser_engine::{Fonts, layout::ScrollOffset, paint::Canvas};
    use std::sync::Arc;
    let (page, mut runtime) = runtime(
        "<!doctype html><style>body{margin:0;width:600px}
        a{display:block;margin-left:250px;width:40px;height:40px;background:#ff0000}
        </style><a id=target href=destination></a>",
    );
    check(
        &mut runtime,
        "var target = document.getElementById('target'), entries = [];
        new IntersectionObserver(e => entries.push(e[0])).observe(target); true",
    );
    wait_for(
        &mut runtime,
        "entries.length === 1 && !entries[0].isIntersecting",
    );
    let viewport = runtime.viewport();
    let first = runtime.layout_for_paint(viewport).unwrap();
    let current = Viewport {
        scroll_x: 220.,
        ..viewport
    };
    let scene = runtime.layout_for_paint(current).unwrap();
    assert!(Arc::ptr_eq(&scene, &first));
    check(
        &mut runtime,
        "window.scrollX === 220 && target.getBoundingClientRect().left === 30",
    );
    wait_for(
        &mut runtime,
        "entries.length === 2 && entries[1].isIntersecting",
    );
    check(
        &mut runtime,
        "entries[1].boundingClientRect.left === 30 && entries[1].intersectionRatio === 1",
    );
    let target = page.document.html_elements("a").next().unwrap();
    assert_eq!(scene.element_at(35. + current.scroll_x, 10.), Some(target));
    let mut fonts = Fonts::new().unwrap();
    let mut frame = Canvas::new(200, 160, scene.background);
    frame.document_at(&scene, &mut fonts, 0., ScrollOffset { x: 220., y: 0. }, 1.);
    assert_eq!(frame.pixels[10 * 200 + 35], 0x00ff0000);
    assert_eq!(frame.pixels[10 * 200 + 5], 0x00ffffff);
    // Existing snapshot consumers use the two-argument camera (Y supplied
    // on paint, X in the snapshot), even after a compositor reused a scene.
    let snapshot = runtime.layout_for_viewport(current).unwrap();
    assert_eq!(snapshot.viewport.scroll_x, 220.);
    assert_eq!(first.viewport.scroll_x, 0.);
    let mut legacy = Canvas::new(200, 160, scene.background);
    legacy.document(&snapshot, &mut fonts, 0., 0.);
    assert_eq!(frame.pixels, legacy.pixels);
}

#[test]
fn take_records_drains_another_observers_snapshot_before_its_callback() {
    let (_, mut runtime) = runtime(
        r#"<!doctype html>
        <style>
          body { margin: 0 }
          #target { position: absolute; left: 20px; top: 40px; width: 40px; height: 50px }
        </style>
        <div id="target"></div>"#,
    );
    check(
        &mut runtime,
        r#"
        var trace = [], drained, firstEntry, currentLeft, firstCallback, drainedAgain;
        var target = document.getElementById('target');
        function callback(entries, observer) {
            trace.push('callback');
            if (trace.length !== 1) return;
            firstCallback = this === observer && (observer === first || observer === second) &&
                arguments.length === 2 && Array.isArray(entries) && entries.length === 1;
            firstEntry = entries[0];
            observer.disconnect();
            target.style.left = '320px';
            currentLeft = target.getBoundingClientRect().left;
            const pending = observer === first ? second : first;
            queueMicrotask(function() {
                trace.push('microtask');
                drained = pending.takeRecords();
                drainedAgain = pending.takeRecords();
                pending.disconnect();
            });
        }
        var first = new IntersectionObserver(callback);
        var second = new IntersectionObserver(callback);
        first.observe(target);
        second.observe(target);
        trace.length === 0 && first.takeRecords().length === 0 &&
            second.takeRecords().length === 0
        "#,
    );
    wait_for(&mut runtime, "drained !== undefined");
    check(
        &mut runtime,
        r#"
        var entry = drained[0];
        firstCallback && trace.join(',') === 'callback,microtask' &&
        currentLeft === 320 && drained.length === 1 && drainedAgain.length === 0 &&
        entry instanceof IntersectionObserverEntry && entry.target === target &&
        entry.boundingClientRect instanceof DOMRectReadOnly &&
        entry.boundingClientRect.left === 20 && entry.boundingClientRect.top === 40 &&
        entry.intersectionRect.left === 20 && entry.intersectionRect.width === 40 &&
        entry.rootBounds.x === 0 && entry.rootBounds.width === 200 &&
        entry.isIntersecting && entry.intersectionRatio === 1 && entry.time === firstEntry.time
        "#,
    );
    runtime.pump();
    check(&mut runtime, "trace.join(',') === 'callback,microtask'");
}

#[test]
fn timer_scheduled_from_an_animation_frame_can_take_its_intersection() {
    let (_, mut runtime) = runtime("<!doctype html><div id='target' style='height:40px'></div>");
    check(
        &mut runtime,
        r#"
        var target = document.getElementById('target');
        var trace = [], taken;
        var observer = new IntersectionObserver(function() { trace.push('callback'); });
        requestAnimationFrame(function() {
            observer.observe(target);
            trace.push('raf:' + observer.takeRecords().length);
            setTimeout(function() {
                taken = observer.takeRecords();
                trace.push('timer:' + taken.length);
            }, 0);
        });
        trace.length === 0
        "#,
    );
    wait_for(&mut runtime, "taken !== undefined");
    runtime.pump();
    check(
        &mut runtime,
        r#"
        trace.join(',') === 'raf:0,timer:1' && taken[0].target === target &&
        taken[0].isIntersecting && observer.takeRecords().length === 0
        "#,
    );
}

#[test]
fn nested_host_scroll_and_viewport_resize_queue_new_intersections() {
    let (page, mut runtime) = runtime(
        r#"<!doctype html>
        <style>
          body { margin: 0 }
          #outer { width: 120px; height: 80px; overflow: auto }
          #inner { width: 90px; height: 100px; overflow: auto }
          #before { height: 70px }
          #target { width: 60px; height: 40px }
          #after { height: 100px }
        </style>
        <div id="outer"><div id="inner"><div id="before"></div>
          <div id="target"></div><div id="after"></div></div></div>"#,
    );
    let node = |id| {
        page.document
            .html_elements("div")
            .find(|&node| page.document.attr(node, "id") == Some(id))
            .unwrap()
    };
    check(
        &mut runtime,
        r#"
        var entries = [];
        var observer = new IntersectionObserver(function(records) {
            entries = entries.concat(records);
        }, {threshold: [0, 0.5, 1]});
        observer.observe(document.getElementById('target'));
        entries.length === 0
        "#,
    );
    wait_for(&mut runtime, "entries.length === 1");
    check(
        &mut runtime,
        r#"
        entries[0].boundingClientRect.top === 70 && entries[0].boundingClientRect.height === 40 &&
        entries[0].intersectionRect.bottom === 80 && entries[0].intersectionRatio === 0.25
        "#,
    );

    assert!(runtime.scroll_element(node("inner"), 0., 20.).unwrap());
    check(&mut runtime, "entries.length === 1");
    wait_for(&mut runtime, "entries.length === 2");
    check(
        &mut runtime,
        r#"
        entries[1].boundingClientRect.top === 50 &&
        entries[1].intersectionRect.bottom === 80 && entries[1].intersectionRatio === 0.75
        "#,
    );

    assert!(runtime.scroll_element(node("outer"), 0., 20.).unwrap());
    check(&mut runtime, "entries.length === 2");
    wait_for(&mut runtime, "entries.length === 3");
    check(
        &mut runtime,
        r#"
        entries[2].boundingClientRect.top === 30 &&
        entries[2].intersectionRect.bottom === 70 && entries[2].intersectionRatio === 1
        "#,
    );

    runtime.set_viewport(Viewport {
        height: 45.,
        ..runtime.viewport()
    });
    check(&mut runtime, "entries.length === 3");
    wait_for(&mut runtime, "entries.length === 4");
    check(
        &mut runtime,
        r#"
        entries[3].rootBounds.bottom === 45 && entries[3].boundingClientRect.top === 30 &&
        entries[3].intersectionRect.bottom === 45 && entries[3].intersectionRatio === 0.375
        "#,
    );
}

#[test]
fn detached_nodes_can_be_observed_with_a_cached_layout_then_connected() {
    let (_, mut runtime) = runtime(
        r#"<!doctype html>
        <style>
          body { margin: 0 }
          #root { position: absolute; left: 10px; top: 190px; width: 100px; height: 100px }
          #target { width: 40px; height: 50px }
          #spacer { height: 480px }
        </style>
        <div id="spacer"></div>"#,
    );
    check(
        &mut runtime,
        r#"
        var viewportEntries = [], explicitEntries = [];
        var viewportObserver = new IntersectionObserver(function(records) {
            viewportEntries = viewportEntries.concat(records);
        });
        var spacer = document.getElementById('spacer');
        viewportObserver.observe(spacer);
        viewportEntries.length === 0
        "#,
    );
    wait_for(&mut runtime, "viewportEntries.length === 1");
    check(
        &mut runtime,
        r#"
        var root = document.createElement('div');
        root.id = 'root';
        var target = document.createElement('div');
        target.id = 'target';
        root.appendChild(target);
        var explicitObserver = new IntersectionObserver(function(records) {
            explicitEntries = explicitEntries.concat(records);
        }, {root});
        viewportObserver.unobserve(spacer);
        viewportObserver.observe(target);
        explicitObserver.observe(target);
        viewportEntries.length === 1 && explicitEntries.length === 0
        "#,
    );
    wait_for(
        &mut runtime,
        "viewportEntries.length === 2 && explicitEntries.length === 1",
    );
    check(
        &mut runtime,
        r#"
        !viewportEntries[1].isIntersecting && !explicitEntries[0].isIntersecting &&
        viewportEntries[1].boundingClientRect.width === 0 &&
        explicitEntries[0].rootBounds.width === 0 &&
        explicitEntries[0].target === target
        "#,
    );
    check(
        &mut runtime,
        "document.body.appendChild(root); explicitEntries.length === 1",
    );
    wait_for(&mut runtime, "explicitEntries.length === 2");
    check(
        &mut runtime,
        r#"
        viewportEntries.length === 2 && explicitEntries[1].isIntersecting &&
        explicitEntries[1].rootBounds.top === 190 && explicitEntries[1].intersectionRatio === 1
        "#,
    );

    runtime.set_viewport(Viewport {
        scroll_y: 80.,
        ..runtime.viewport()
    });
    wait_for(&mut runtime, "viewportEntries.length === 3");
    check(
        &mut runtime,
        r#"
        explicitEntries.length === 2 && viewportEntries[2].isIntersecting &&
        viewportEntries[2].intersectionRect.top === 110 &&
        viewportEntries[2].intersectionRect.height === 50
        "#,
    );
    check(
        &mut runtime,
        "target.remove(); viewportEntries.length === 3 && explicitEntries.length === 2",
    );
    wait_for(
        &mut runtime,
        "viewportEntries.length === 4 && explicitEntries.length === 3",
    );
    check(
        &mut runtime,
        r#"
        !viewportEntries[3].isIntersecting && !explicitEntries[2].isIntersecting &&
        viewportEntries[3].boundingClientRect.width === 0 &&
        explicitEntries[2].intersectionRect.height === 0
        "#,
    );
}
