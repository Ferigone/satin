use rust_browser_engine::{
    Fonts, Layout, LoadedPage, layout::Viewport, paint::Canvas, script::ScriptRuntime,
};
use rust_browser_javascript::Value;

#[test]
fn nested_column_subgrids_follow_parent_tracks_gap_and_live_edits() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>
        body { margin:0 }
        #outer { display:grid; grid-template-columns:25% 1fr; column-gap:12px }
        #outer.compact { grid:auto / 80px 1fr }
        .group,.row { display:grid; grid:auto / subgrid; grid-column:1 / span 2 }
        .first,.second { height:20px; }
        .first { background:blue } .second { background:green }
        </style>
        <div id=outer><div class=group><div class=row><span class=first></span><span class=second></span></div></div></div>
    "#,
    );
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 400.);
    let mut canvas = Canvas::new(400, 32, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited && !canvas.limited);
    assert_eq!(canvas.pixels[5 * 400 + 99], 0x0000ff);
    assert_eq!(canvas.pixels[5 * 400 + 107], 0xffffff);
    assert_eq!(canvas.pixels[5 * 400 + 112], 0x008000);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 400.,
        height: 400.,
        ..Default::default()
    });
    assert_eq!(runtime.eval(r#"
        const e=document.querySelector('.second');
        function bounds(){const r=e.getBoundingClientRect();return [r.x,r.y,r.width,r.height].join(',')}
        let first=bounds();document.getElementById('outer').className='compact';
        first==='112,0,288,20' && bounds()==='92,0,308,20'
        && getComputedStyle(e).display==='block'
        && getComputedStyle(document.querySelector('.row')).gridTemplateColumns==='subgrid [] [] []'
    "#).unwrap(),Value::Bool(true));
    runtime
        .eval("document.getElementById('outer').className=''")
        .unwrap();
    runtime.set_viewport(Viewport {
        width: 320.,
        ..runtime.viewport()
    });
    assert_eq!(
        runtime.eval("bounds()==='92,0,228,20'").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn nowrap_intrinsic_width_preserves_adjacent_inline_runs_and_forced_breaks() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>
        body { margin:0 } #grid {display:grid;width:1px;grid-template-columns:auto 10px}
        #label {white-space:nowrap} .unit {display:inline-block;width:40px;height:16px}
        </style>
        <div id="grid"><div id="label"><span class="unit"></span><span class="unit"></span><br><span class="unit"></span></div><div id="next"></div></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        const n=document.getElementById('next'), label=document.getElementById('label');
        const x=n.getBoundingClientRect().x;
        label.style.setProperty('white-space','normal');
        [x,n.getBoundingClientRect().x].join(',')
    "#
            )
            .unwrap(),
        "80,40".into()
    );
}

#[test]
fn grid_shorthand_places_implicit_tracks_and_resets_the_previous_axis() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>body{margin:0}#outer{display:grid;width:200px;grid:18px/auto-flow dense 40px}
        #outer.rows{grid:auto-flow 30px/50px}</style>
        <div id=outer><div></div><div id=second></div></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
        const n=document.getElementById('second');
        function box(){const r=n.getBoundingClientRect();return [r.x,r.y,r.width,r.height].join(',')}
        let before=box();document.getElementById('outer').className='rows';
        [before,box()].join('|')
    "#).unwrap(),"40,0,40,18|0,30,50,30".into());
}

#[test]
fn oversized_subgrid_gutters_keep_the_parent_line_centers() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>body{margin:0}.outer{display:grid;width:300px;grid-template-columns:repeat(3,100px)}
        #child{display:grid;grid-template-columns:subgrid;grid-column:span 3;gap:150px}
        i{width:100px;height:10px}#a{grid-column:2}#b{grid-column:3}</style>
        <div class=outer><div id=child><i id=a></i><i id=b></i></div></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
        const x=id=>document.getElementById(id).getBoundingClientRect().x;
        const first=[x('a'),x('b')];document.getElementById('child').style.setProperty('gap','250px');
        [...first,x('a'),x('b')].join(',')
    "#).unwrap(),"175,275,225,325".into());
}
