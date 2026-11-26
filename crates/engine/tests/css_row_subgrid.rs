use rust_browser_engine::{Fonts, Layout, LoadedPage, paint::Canvas, script::ScriptRuntime};

#[test]
fn subgrid_tracks_follow_parent_rows_and_dynamic_row_gaps() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>
        body{margin:0} #outer{display:grid;grid:30px 70px / 100px;row-gap:10px}
        #inner{display:grid;grid:subgrid / subgrid;grid-row:1 / 3}
        #first{background:blue} #second{background:green}
        #outer.changed{grid-template-rows:60px 40px}
        </style>
        <div id=outer><div id=inner><div id=first></div><div id=second></div></div></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        const outer=document.getElementById('outer'), inner=document.getElementById('inner');
        const second=document.getElementById('second');
        function rect(){const r=second.getBoundingClientRect();return [r.y,r.height].join(',')}
        let a=rect();outer.className='changed';let b=rect();
        inner.style.setProperty('row-gap','20px');
        [a,b,rect(),getComputedStyle(inner).gridTemplateRows].join('|')
    "#
            )
            .unwrap(),
        "40,70|70,40|75,35|subgrid [] [] []".into()
    );
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 150.);
    let mut canvas = Canvas::new(150, 120, layout.background);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert!(!layout.limited && !canvas.limited);
    assert_eq!(canvas.pixels[29 * 150 + 10], 0x0000ff);
    assert_eq!(canvas.pixels[35 * 150 + 10], 0xffffff);
    assert_eq!(canvas.pixels[40 * 150 + 10], 0x008000);
}

#[test]
fn subgrid_grandchildren_contribute_to_individual_auto_rows() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>
        body{margin:0} #parent{display:grid;grid:auto auto / 50px;row-gap:10px}
        #group{display:grid;grid-row:span 2;grid-template-rows:subgrid;padding:3px 0 7px}
        #a{height:20px} #b{height:80px}
        </style>
        <div id=parent><div id=group><div id=a></div><div id=b></div></div></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        function geometry(){const b=document.getElementById('b').getBoundingClientRect();
          return [b.y,document.getElementById('parent').getBoundingClientRect().height].join(',')}
        let initial=geometry(); document.getElementById('a').style.height='35px';
        [initial,geometry()].join('|')
    "#
            )
            .unwrap(),
        "33,120|48,135".into()
    );
}

#[test]
fn grid_items_share_first_and_last_baselines_across_a_row() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"<style>
    body{margin:0} #grid{display:grid;grid:100px / repeat(2,40px);align-items:first baseline}
    #grid.end{align-items:last baseline} #short{height:20px} #tall{height:40px}
    </style><div id=grid><div id=short></div><div id=tall></div></div>"#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
      function ys(){return [document.getElementById('short').offsetTop,document.getElementById('tall').offsetTop].join(',')}
      let first=ys(); document.getElementById('grid').className='end';[first,ys()].join('|')
    "#).unwrap(),"20,0|80,60".into());
}

#[test]
fn percentage_height_uses_baseline_fallback_only_for_intrinsic_rows() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>
        body{margin:0}
        #g{display:grid;height:100px;grid:auto / 40px 40px;align-items:baseline}
        #g.fixed{grid-template-rows:100px}
        #a{height:60px} #b{height:20%}
        </style><div id=g><div id=a></div><div id=b></div></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        let g=document.getElementById('g'),b=document.getElementById('b');
        let initial=b.offsetTop;g.className='fixed';[initial,b.offsetTop].join(',')
    "#
            )
            .unwrap(),
        "0,40".into()
    );
}

#[test]
fn horizontal_flex_lines_share_baselines_under_reverse_wrapping() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r#"<style>
      body{margin:0} #f{display:flex;flex-wrap:wrap-reverse;width:80px;align-items:baseline}
      #f>div{width:40px} .small{height:10px}.tall{height:30px}
      </style><div id=f><div class=small></div><div class=tall></div><div class=tall></div><div class=small></div></div>"#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"function positions(){return Array.from(document.querySelectorAll('#f>div'),e=>e.offsetTop).join(',')}
    let reversed=positions();document.getElementById('f').style.setProperty('flex-wrap','wrap');
    [reversed,positions()].join('|')"#).unwrap(),"50,30,0,20|20,0,30,50".into());
}
