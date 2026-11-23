use rust_browser_engine::{Fonts, Layout, LoadedPage, dom::NodeId};
const GOLDEN: &str = r##"{"html":"<!doctype html><meta charset=\"utf-8\"><style>body{margin:0;font-size:0;line-height:0}section{margin:0 0 20px}</style><section><div id=\"fixed\" style=\"display:grid;width:400px;grid-template-columns:100px 200px;grid-template-rows:30px 40px;gap:10px\"><div id=\"fixed-0\" style=\"\"></div><div id=\"fixed-1\" style=\"\"></div><div id=\"fixed-2\" style=\"\"></div><div id=\"fixed-3\" style=\"\"></div></div></section><section><div id=\"fraction\" style=\"display:grid;width:500px;grid-template-columns:100px 1fr 2fr;gap:10px\"><div id=\"fraction-0\" style=\"height:20px\"></div><div id=\"fraction-1\" style=\"height:30px\"></div><div id=\"fraction-2\" style=\"height:10px\"></div></div></section><section><div id=\"repeat\" style=\"display:grid;width:300px;grid-template-columns:repeat(3,minmax(0,1fr));gap:10px\"><div id=\"repeat-0\" style=\"height:10px\"></div><div id=\"repeat-1\" style=\"height:20px\"></div><div id=\"repeat-2\" style=\"height:30px\"></div><div id=\"repeat-3\" style=\"height:40px\"></div></div></section><section><div id=\"rowfraction\" style=\"display:grid;width:200px;height:300px;grid-template-rows:1fr 2fr;gap:10px\"><div id=\"rowfraction-0\" style=\"\"></div><div id=\"rowfraction-1\" style=\"\"></div></div></section><section><div id=\"placed\" style=\"display:grid;width:320px;grid-template-columns:repeat(3,100px);grid-auto-rows:30px;gap:10px\"><div id=\"placed-0\" style=\"grid-column:2/span 2;grid-row:1/3\"></div><div id=\"placed-1\" style=\"\"></div><div id=\"placed-2\" style=\"\"></div><div id=\"placed-3\" style=\"\"></div></div></section><section><div id=\"sparse\" style=\"display:grid;width:320px;grid-template-columns:repeat(3,100px);grid-auto-rows:30px;gap:10px\"><div id=\"sparse-0\" style=\"grid-column:span 2\"></div><div id=\"sparse-1\" style=\"grid-column:span 2\"></div><div id=\"sparse-2\" style=\"\"></div></div></section><section><div id=\"dense\" style=\"display:grid;width:320px;grid-template-columns:repeat(3,100px);grid-auto-rows:30px;gap:10px;grid-auto-flow:row dense\"><div id=\"dense-0\" style=\"grid-column:span 2\"></div><div id=\"dense-1\" style=\"grid-column:span 2\"></div><div id=\"dense-2\" style=\"\"></div></div></section><section><div id=\"columnflow\" style=\"display:grid;width:300px;grid-template-rows:repeat(2,25px);grid-auto-columns:70px;grid-auto-flow:column;gap:5px\"><div id=\"columnflow-0\" style=\"\"></div><div id=\"columnflow-1\" style=\"\"></div><div id=\"columnflow-2\" style=\"\"></div><div id=\"columnflow-3\" style=\"\"></div><div id=\"columnflow-4\" style=\"\"></div></div></section><section><div id=\"negative\" style=\"display:grid;width:300px;grid-template-columns:100px 100px;grid-auto-columns:30px;grid-auto-rows:20px;gap:5px\"><div id=\"negative-0\" style=\"grid-column:-4/-3\"></div><div id=\"negative-1\" style=\"\"></div><div id=\"negative-2\" style=\"grid-column:1/-1\"></div></div></section><section><div id=\"cycle\" style=\"display:grid;width:200px;grid-template-columns:repeat(2,90px);grid-auto-rows:10px 20px;gap:2px\"><div id=\"cycle-0\" style=\"\"></div><div id=\"cycle-1\" style=\"\"></div><div id=\"cycle-2\" style=\"\"></div><div id=\"cycle-3\" style=\"\"></div><div id=\"cycle-4\" style=\"\"></div></div></section><section><div id=\"auto\" style=\"display:grid;width:400px;grid-template-columns:auto auto;gap:10px\"><div id=\"auto-0\" style=\"width:80px;height:10px\"></div><div id=\"auto-1\" style=\"width:50px;height:20px\"></div></div></section><section><div id=\"intrinsic\" style=\"display:grid;width:400px;grid-template-columns:max-content min-content;gap:10px\"><div id=\"intrinsic-0\" style=\"width:80px;height:10px\"></div><div id=\"intrinsic-1\" style=\"width:50px;height:20px\"></div></div></section><section><div id=\"minmax\" style=\"display:grid;width:300px;grid-template-columns:minmax(100px,50px) minmax(0,1fr)\"><div id=\"minmax-0\" style=\"height:10px\"></div><div id=\"minmax-1\" style=\"height:20px\"></div></div></section><section><div id=\"smallfr\" style=\"display:grid;width:400px;grid-template-columns:.25fr .25fr\"><div id=\"smallfr-0\" style=\"height:10px\"></div><div id=\"smallfr-1\" style=\"height:20px\"></div></div></section><section><div id=\"spancontent\" style=\"display:grid;width:400px;grid-template-columns:minmax(0,max-content) minmax(0,max-content)\"><div id=\"spancontent-0\" style=\"grid-column:span 2;width:300px;height:20px\"></div></div></section><section><div id=\"between\" style=\"display:grid;width:400px;grid-template-columns:100px 100px;justify-content:space-between;gap:10px\"><div id=\"between-0\" style=\"height:20px\"></div><div id=\"between-1\" style=\"height:30px\"></div></div></section><section><div id=\"center\" style=\"display:grid;width:300px;grid-template-columns:100px 100px;grid-template-rows:80px;place-items:center\"><div id=\"center-0\" style=\"width:20px;height:10px\"></div><div id=\"center-1\" style=\"width:50px;height:30px\"></div></div></section><section><div id=\"margins\" style=\"display:grid;width:200px;grid-template-rows:100px\"><div id=\"margins-0\" style=\"width:40px;height:20px;margin:auto\"></div></div></section><section><div id=\"padding\" style=\"display:grid;width:300px;padding:20px;border:2px solid red;grid-template-columns:1fr 1fr;gap:10px\"><div id=\"padding-0\" style=\"height:20px\"></div><div id=\"padding-1\" style=\"height:30px\"></div></div></section><section><div id=\"percent\" style=\"display:grid;width:400px;height:100px;grid-template-columns:25% 75%;grid-template-rows:50% 50%\"><div id=\"percent-0\" style=\"\"></div><div id=\"percent-1\" style=\"\"></div><div id=\"percent-2\" style=\"\"></div><div id=\"percent-3\" style=\"\"></div></div></section><section><div id=\"alignment\" style=\"display:grid;width:400px;height:200px;grid-template-columns:100px 100px;grid-template-rows:50px 50px;gap:10px;place-content:center\"><div id=\"alignment-0\" style=\"\"></div><div id=\"alignment-1\" style=\"\"></div><div id=\"alignment-2\" style=\"\"></div><div id=\"alignment-3\" style=\"\"></div></div></section><section><div id=\"maxwidth\" style=\"display:grid;width:300px;grid-template-columns:1fr;justify-items:stretch\"><div id=\"maxwidth-0\" style=\"max-width:100px;height:20px\"></div></div></section><section><div id=\"endspan\" style=\"display:grid;width:320px;grid-template-columns:repeat(3,100px);grid-auto-rows:20px;gap:10px\"><div id=\"endspan-0\" style=\"grid-column:span 2/4;grid-row:2\"></div><div id=\"endspan-1\" style=\"\"></div><div id=\"endspan-2\" style=\"\"></div><div id=\"endspan-3\" style=\"\"></div></div></section><section><div id=\"order\" style=\"display:grid;width:200px;grid-template-columns:100px 100px;grid-auto-rows:20px\"><div id=\"order-0\" style=\"order:2\"></div><div id=\"order-1\" style=\"order:-1\"></div><div id=\"order-2\" style=\"order:1\"></div></div></section><section><div id=\"inline\" style=\"display:inline-grid;grid-template-columns:50px 70px;gap:10px\"><div id=\"inline-0\" style=\"height:20px\"></div><div id=\"inline-1\" style=\"height:10px\"></div></div></section><section><div id=\"intrinsicboxes\" style=\"display:grid;width:300px;grid-template-columns:max-content min-content;gap:10px;font-size:0;line-height:0\"><div id=\"intrinsicboxes-0\" style=\"\"><i style=\"display:inline-block;width:20px;height:10px\"></i><i style=\"display:inline-block;width:40px;height:10px\"></i></div><div id=\"intrinsicboxes-1\" style=\"\"><i style=\"display:inline-block;width:30px;height:10px\"></i><i style=\"display:inline-block;width:80px;height:10px\"></i></div></div></section><section><div id=\"autominfr\" style=\"display:grid;width:300px;grid-template-columns:1fr 1fr;font-size:0;line-height:0\"><div id=\"autominfr-0\" style=\"\"><i style=\"display:inline-block;width:500px;height:10px\"></i></div><div id=\"autominfr-1\" style=\"height:20px\"></div></div></section><section><div id=\"zerominfr\" style=\"display:grid;width:300px;grid-template-columns:minmax(0,1fr) minmax(0,1fr);font-size:0;line-height:0\"><div id=\"zerominfr-0\" style=\"\"><i style=\"display:inline-block;width:500px;height:10px\"></i></div><div id=\"zerominfr-1\" style=\"height:20px\"></div></div></section><section><div id=\"mixedspan\" style=\"display:grid;width:400px;grid-template-columns:minmax(0,max-content) minmax(0,auto);grid-auto-rows:20px\"><div id=\"mixedspan-0\" style=\"grid-column:span 2;width:300px\"></div><div id=\"mixedspan-1\" style=\"grid-column:1\"></div><div id=\"mixedspan-2\" style=\"grid-column:2\"></div></div></section><section><div id=\"spanningfixedwidth\" style=\"display:grid;width:400px;grid-template-columns:1fr 1fr;grid-auto-rows:20px\"><div id=\"spanningfixedwidth-0\" style=\"grid-column:span 2;width:600px\"></div><div id=\"spanningfixedwidth-1\" style=\"grid-column:1\"></div><div id=\"spanningfixedwidth-2\" style=\"grid-column:2\"></div></div></section><section><div id=\"clampedautomin\" style=\"display:grid;width:300px;grid-template-columns:minmax(auto,100px) 1fr\"><div id=\"clampedautomin-0\" style=\"width:200px;height:20px\"></div><div id=\"clampedautomin-1\" style=\"height:20px\"></div></div></section><section><div id=\"minorimplicit\" style=\"display:grid;width:300px;grid-template-columns:50px 50px;grid-template-rows:20px;grid-auto-columns:10px 20px;gap:5px\"><div id=\"minorimplicit-0\" style=\"grid-column:-5/-4\"></div><div id=\"minorimplicit-1\" style=\"grid-column:-4/-3\"></div><div id=\"minorimplicit-2\" style=\"grid-column:1/-1\"></div></div></section><section><div id=\"columnDense\" style=\"display:grid;width:300px;grid-template-rows:20px 20px 20px;grid-auto-columns:30px;grid-auto-flow:column dense;gap:5px\"><div id=\"columnDense-0\" style=\"grid-row:span 2\"></div><div id=\"columnDense-1\" style=\"grid-row:span 2\"></div><div id=\"columnDense-2\" style=\"\"></div></div></section><section><div id=\"percentcyclic\" style=\"display:grid;width:200px;grid-template-columns:100px 100px;grid-template-rows:50% 50%;row-gap:10px\"><div id=\"percentcyclic-0\" style=\"height:10px\"></div><div id=\"percentcyclic-1\" style=\"height:10px\"></div><div id=\"percentcyclic-2\" style=\"height:10px\"></div><div id=\"percentcyclic-3\" style=\"height:10px\"></div></div></section><section><div id=\"gapcyclic\" style=\"display:grid;width:200px;grid-template-columns:100px 100px;row-gap:10%\"><div id=\"gapcyclic-0\" style=\"height:10px\"></div><div id=\"gapcyclic-1\" style=\"height:10px\"></div><div id=\"gapcyclic-2\" style=\"height:10px\"></div><div id=\"gapcyclic-3\" style=\"height:10px\"></div></div></section><section><div id=\"minheight\" style=\"display:grid;width:200px;min-height:200px;grid-template-columns:100px 100px\"><div id=\"minheight-0\" style=\"height:10px\"></div><div id=\"minheight-1\" style=\"height:10px\"></div><div id=\"minheight-2\" style=\"height:10px\"></div><div id=\"minheight-3\" style=\"height:10px\"></div></div></section><section><div id=\"maxheight\" style=\"display:grid;width:200px;max-height:15px;grid-template-columns:100px 100px\"><div id=\"maxheight-0\" style=\"height:10px\"></div><div id=\"maxheight-1\" style=\"height:10px\"></div><div id=\"maxheight-2\" style=\"height:10px\"></div><div id=\"maxheight-3\" style=\"height:10px\"></div></div></section><section><div id=\"percentitems\" style=\"display:grid;width:200px;grid-template-columns:1fr;grid-template-rows:100px\"><div id=\"percentitems-0\" style=\"width:50%;height:50%\"></div></div></section>","rects":{"fixed":[0,0,400,80],"fixed-0":[0,0,100,30],"fixed-1":[110,0,200,30],"fixed-2":[0,40,100,40],"fixed-3":[110,40,200,40],"fraction":[0,100,500,30],"fraction-0":[0,100,100,20],"fraction-1":[110,100,126.65625,30],"fraction-2":[246.65625,100,253.34375,10],"repeat":[0,150,300,80],"repeat-0":[0,150,93.328125,10],"repeat-1":[103.328125,150,93.328125,20],"repeat-2":[206.65625,150,93.34375,30],"repeat-3":[0,190,93.328125,40],"rowfraction":[0,250,200,300],"rowfraction-0":[0,250,200,96.65625],"rowfraction-1":[0,356.65625,200,193.34375],"placed":[0,570,320,110],"placed-0":[110,570,210,70],"placed-1":[0,570,100,30],"placed-2":[0,610,100,30],"placed-3":[0,650,100,30],"sparse":[0,700,320,70],"sparse-0":[0,700,210,30],"sparse-1":[0,740,210,30],"sparse-2":[220,740,100,30],"dense":[0,790,320,70],"dense-0":[0,790,210,30],"dense-1":[0,830,210,30],"dense-2":[220,790,100,30],"columnflow":[0,880,300,55],"columnflow-0":[0,880,70,25],"columnflow-1":[0,910,70,25],"columnflow-2":[75,880,70,25],"columnflow-3":[75,910,70,25],"columnflow-4":[150,880,70,25],"negative":[0,955,300,45],"negative-0":[0,955,30,20],"negative-1":[35,955,100,20],"negative-2":[35,980,205,20],"cycle":[0,1020,200,44],"cycle-0":[0,1020,90,10],"cycle-1":[92,1020,90,10],"cycle-2":[0,1032,90,20],"cycle-3":[92,1032,90,20],"cycle-4":[0,1054,90,10],"auto":[0,1084,400,20],"auto-0":[0,1084,80,10],"auto-1":[220,1084,50,20],"intrinsic":[0,1124,400,20],"intrinsic-0":[0,1124,80,10],"intrinsic-1":[90,1124,50,20],"minmax":[0,1164,300,20],"minmax-0":[0,1164,100,10],"minmax-1":[100,1164,200,20],"smallfr":[0,1204,400,20],"smallfr-0":[0,1204,100,10],"smallfr-1":[100,1204,100,20],"spancontent":[0,1244,400,20],"spancontent-0":[0,1244,300,20],"between":[0,1284,400,30],"between-0":[0,1284,100,20],"between-1":[300,1284,100,30],"center":[0,1334,300,80],"center-0":[40,1369,20,10],"center-1":[125,1359,50,30],"margins":[0,1434,200,100],"margins-0":[80,1474,40,20],"padding":[0,1554,344,74],"padding-0":[22,1576,145,20],"padding-1":[177,1576,145,30],"percent":[0,1648,400,100],"percent-0":[0,1648,100,50],"percent-1":[100,1648,300,50],"percent-2":[0,1698,100,50],"percent-3":[100,1698,300,50],"alignment":[0,1768,400,200],"alignment-0":[95,1813,100,50],"alignment-1":[205,1813,100,50],"alignment-2":[95,1873,100,50],"alignment-3":[205,1873,100,50],"maxwidth":[0,1988,300,20],"maxwidth-0":[0,1988,100,20],"endspan":[0,2028,320,50],"endspan-0":[110,2058,210,20],"endspan-1":[0,2028,100,20],"endspan-2":[110,2028,100,20],"endspan-3":[220,2028,100,20],"order":[0,2098,200,40],"order-0":[0,2118,100,20],"order-1":[0,2098,100,20],"order-2":[100,2098,100,20],"inline":[0,2158,130,20],"inline-0":[0,2158,50,20],"inline-1":[60,2158,70,10],"intrinsicboxes":[0,2198,300,20],"intrinsicboxes-0":[0,2198,60,20],"intrinsicboxes-1":[70,2198,80,20],"autominfr":[0,2238,300,20],"autominfr-0":[0,2238,500,20],"autominfr-1":[500,2238,0,20],"zerominfr":[0,2278,300,20],"zerominfr-0":[0,2278,150,20],"zerominfr-1":[150,2278,150,20],"mixedspan":[0,2318,400,40],"mixedspan-0":[0,2318,300,20],"mixedspan-1":[0,2338,150,20],"mixedspan-2":[150,2338,250,20],"spanningfixedwidth":[0,2378,400,40],"spanningfixedwidth-0":[0,2378,600,20],"spanningfixedwidth-1":[0,2398,300,20],"spanningfixedwidth-2":[300,2398,300,20],"clampedautomin":[0,2438,300,20],"clampedautomin-0":[0,2438,200,20],"clampedautomin-1":[200,2438,100,20],"minorimplicit":[0,2478,300,20],"minorimplicit-0":[0,2478,10,20],"minorimplicit-1":[15,2478,20,20],"minorimplicit-2":[40,2478,105,20],"columnDense":[0,2518,300,70],"columnDense-0":[0,2518,30,45],"columnDense-1":[35,2518,30,45],"columnDense-2":[0,2568,30,20],"percentcyclic":[0,2608,200,30],"percentcyclic-0":[0,2608,100,10],"percentcyclic-1":[100,2608,100,10],"percentcyclic-2":[0,2633,100,10],"percentcyclic-3":[100,2633,100,10],"gapcyclic":[0,2658,200,20],"gapcyclic-0":[0,2658,100,10],"gapcyclic-1":[100,2658,100,10],"gapcyclic-2":[0,2670,100,10],"gapcyclic-3":[100,2670,100,10],"minheight":[0,2698,200,200],"minheight-0":[0,2698,100,10],"minheight-1":[100,2698,100,10],"minheight-2":[0,2798,100,10],"minheight-3":[100,2798,100,10],"maxheight":[0,2918,200,15],"maxheight-0":[0,2918,100,10],"maxheight-1":[100,2918,100,10],"maxheight-2":[0,2928,100,10],"maxheight-3":[100,2928,100,10],"percentitems":[0,2953,200,100],"percentitems-0":[0,2953,100,50]},"cases":[{"name":"fixed","children":4},{"name":"fraction","children":3},{"name":"repeat","children":4},{"name":"rowfraction","children":2},{"name":"placed","children":4},{"name":"sparse","children":3},{"name":"dense","children":3},{"name":"columnflow","children":5},{"name":"negative","children":3},{"name":"cycle","children":5},{"name":"auto","children":2},{"name":"intrinsic","children":2},{"name":"minmax","children":2},{"name":"smallfr","children":2},{"name":"spancontent","children":1},{"name":"between","children":2},{"name":"center","children":2},{"name":"margins","children":1},{"name":"padding","children":2},{"name":"percent","children":4},{"name":"alignment","children":4},{"name":"maxwidth","children":1},{"name":"endspan","children":4},{"name":"order","children":3},{"name":"inline","children":2},{"name":"intrinsicboxes","children":2},{"name":"autominfr","children":2},{"name":"zerominfr","children":2},{"name":"mixedspan","children":3},{"name":"spanningfixedwidth","children":3},{"name":"clampedautomin","children":2},{"name":"minorimplicit","children":3},{"name":"columnDense","children":3},{"name":"percentcyclic","children":4},{"name":"gapcyclic","children":4},{"name":"minheight","children":4},{"name":"maxheight","children":4},{"name":"percentitems","children":1}],"width":984,"height":905}"##;

#[test]
fn pinned_chrome_157_grid_reference() {
    // 2026-11-10, owned headless Chrome 157.0.8103.51, scale1, viewport984x905.
    // Empty boxes isolate Grid/box geometry from glyph rendering.
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let page = LoadedPage::from_html("https://example.test/", golden["html"].as_str().unwrap());
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, golden["width"].as_f64().unwrap() as f32);
    assert!(
        !layout.limited,
        "reference unexpectedly reached a work/depth limit"
    );
    let mut failures = vec![];
    let mut maximum = 0f32;
    let mut coordinates = 0;
    for case in golden["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        for name in std::iter::once(name.to_owned())
            .chain((0..case["children"].as_u64().unwrap()).map(|index| format!("{name}-{index}")))
        {
            let id = page
                .document
                .connected_nodes()
                .find(|&id| page.document.attr(id, "id") == Some(name.as_str()))
                .unwrap();
            let actual = layout
                .rect_for(id)
                .unwrap_or_else(|| panic!("missing box {name} {id:?}"));
            let expected = golden["rects"][&name].as_array().unwrap();
            for (axis, (actual, expected)) in [actual.x, actual.y, actual.width, actual.height]
                .into_iter()
                .zip(expected)
                .enumerate()
            {
                let expected = expected.as_f64().unwrap() as f32;
                maximum = maximum.max((actual - expected).abs());
                coordinates += 1;
                if (actual - expected).abs() > 0.05 {
                    failures.push(format!("{name} axis{axis}: {actual} != Chrome {expected}"));
                }
            }
        }
    }
    println!("38 Grid fixtures / {coordinates} coordinates; max error {maximum:.6} CSS px");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn enormous_grid_placement_uses_explicit_bounded_fallback() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "about:grid",
        "<!doctype html><style>body{margin:0}</style><div style='display:grid;grid-template-columns:repeat(2,10px)'><a href='about:next' style='grid-column:32000;grid-row:32000'>still visible</a></div>",
    );
    let layout = Layout::build(&page, &fonts, 800.);
    assert!(layout.limited);
    assert!(layout.links.iter().any(|link| link.url == "about:next"));
}

#[test]
fn grid_paint_and_hit_order_preserve_dom_identity() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "about:grid",
        "<!doctype html><style>body{margin:0}</style><div style='display:grid;grid-template-columns:100px;grid-template-rows:50px'><a id='low' href='about:low' style='grid-area:1/1;order:1'>low</a><a id='high' href='about:high' style='grid-area:1/1;order:2'>high</a></div>",
    );
    let layout = Layout::build(&page, &fonts, 800.);
    let high=page.document.nodes.iter().position(|node|matches!(&node.kind,rust_browser_engine::dom::NodeKind::Element{attrs,..} if attrs.get("id").is_some_and(|value|value=="high"))).map(NodeId).unwrap();
    assert_eq!(layout.element_at(90., 40.), Some(high));
    assert_eq!(layout.link_at(90., 40.), Some("about:high"));
}

#[test]
fn anonymous_grid_text_does_not_inherit_container_box_metrics() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "about:grid",
        "<!doctype html><style>body{margin:0}</style><div id='grid' style='display:grid;width:200px;padding:20px;line-height:20px'>word</div>",
    );
    let layout = Layout::build(&page, &fonts, 800.);
    let id = page
        .document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some("grid"))
        .unwrap();
    let rect = layout.rect_for(id).unwrap();
    assert!(
        (rect.height - 60.).abs() < 0.05,
        "anonymous text gained a second box: {rect:?}"
    );
    let first_x = layout
        .commands
        .iter()
        .find_map(|command| {
            if let rust_browser_engine::DrawCommand::Text { x, .. } = command {
                Some(*x)
            } else {
                None
            }
        })
        .unwrap();
    assert!((first_x - 20.).abs() < 0.05);
}

#[test]
fn adjacent_grid_text_and_comment_separators_create_one_anonymous_item() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "about:grid",
        "<!doctype html><style>body{margin:0}</style><div style='display:grid;grid-template-columns:100px 100px;grid-auto-rows:20px;line-height:20px'>hello<!--split-->world<span style='display:none'>hidden</span>again<div id='item'></div></div>",
    );
    let layout = Layout::build(&page, &fonts, 800.);
    let id = page
        .document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some("item"))
        .unwrap();
    let rect = layout.rect_for(id).unwrap();
    assert!(
        (rect.x - 100.).abs() < 0.05 && rect.y.abs() < 0.05,
        "text nodes were placed as separate grid items: {rect:?}"
    );
}

#[test]
fn deep_and_remeasured_grid_layout_is_bounded_on_one_mib_stack() {
    use rust_browser_engine::dom::NodeKind;
    use std::{collections::BTreeMap, sync::Arc};
    let fonts = Fonts::new().unwrap();
    let mut page = LoadedPage::from_html("about:grid", "<!doctype html><body style='margin:0'>");
    let body = page.document.elements("body").next().unwrap();
    let doc = Arc::make_mut(&mut page.document);
    let mut parent = body;
    for _ in 0..253 {
        parent = doc.append(
            parent,
            NodeKind::Element {
                tag: "div".into(),
                attrs: BTreeMap::from([(
                    "style".into(),
                    "display:grid;grid-template-columns:1fr".into(),
                )]),
            },
        );
    }
    let anchor = doc.append(
        parent,
        NodeKind::Element {
            tag: "a".into(),
            attrs: BTreeMap::from([("href".into(), "about:deep".into())]),
        },
    );
    doc.append(anchor, NodeKind::Text("Deep grid content".into()));
    let layout = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || Layout::build(&page, &fonts, 800.))
        .unwrap()
        .join()
        .unwrap();
    assert!(layout.limited);
    assert!(layout.height.is_finite());
    assert!(layout.links.iter().any(|link| link.url == "about:deep"));
}

fn check_grid(source: &str, expected: &[(&str, [f32; 4])]) {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "about:grid",
        &format!(
            "<!doctype html><style>body{{margin:0;font-size:0;line-height:0}}</style>{source}"
        ),
    );
    let layout = Layout::build(&page, &fonts, 800.);
    assert!(!layout.limited, "unexpected Grid limit");
    for (name, expected) in expected {
        let id = page
            .document
            .connected_nodes()
            .find(|&id| page.document.attr(id, "id") == Some(name))
            .unwrap();
        let actual = layout.rect_for(id).unwrap();
        let coords = [actual.x, actual.y, actual.width, actual.height];
        assert!(
            coords
                .into_iter()
                .zip(expected)
                .all(|(actual, expected)| (actual - expected).abs() < 0.05),
            "{name}: {coords:?} != {expected:?}",
        );
    }
}

#[test]
fn areas_supply_explicit_track_extent_and_generated_line_names() {
    check_grid(
        r#"<div style='display:grid;width:300px;grid-template-areas:"Head Head" "Side Card" "Side Foot";
                       grid-template-columns:[left]90px[main];grid-auto-columns:170px;
                       grid-auto-rows:26px 42px;gap:8px 10px'>
                <div id=head style='grid-area:Head'></div>
                <div id=side style='grid-area:Side'></div>
                <div id=card style='grid-area:Card'></div>
                <div id=foot style='grid-row:-2/-1;grid-column:main/-1'></div>
                <div id=generated style='grid-row:Side-start/Side-end;grid-column:1 Card-start/1 Foot-end'></div>
           </div>"#,
        &[
            ("head", [0., 0., 270., 26.]),
            ("side", [0., 34., 90., 76.]),
            ("card", [100., 34., 170., 42.]),
            ("foot", [100., 84., 170., 26.]),
            ("generated", [100., 34., 170., 76.]),
        ],
    );
}

#[test]
fn named_occurrences_and_spans_count_merged_repeat_edges() {
    check_grid(
        r#"<div style='display:grid;width:500px;gap:0 10px;grid-template-rows:repeat(4,24px);
                 grid-template-columns:[frame-start]40px[alpha] repeat(3,[tick lane-start]60px[step lane-end tick])[frame-end]'>
                <div id=span style='grid-row:1;grid-column:2 tick/span 2 tick'></div>
                <div id=negative style='grid-row:2;grid-column:-2 tick/-1 tick'></div>
                <div id=backward style='grid-row:3;grid-column:span 2 tick/frame-end'></div>
                <div id=area style='grid-row:4;grid-column:frame'></div>
           </div>"#,
        &[
            ("span", [120., 0., 130., 24.]),
            ("negative", [190., 24., 60., 24.]),
            ("backward", [120., 48., 130., 24.]),
            ("area", [0., 72., 250., 24.]),
        ],
    );
}

#[test]
fn named_placement_extends_the_implicit_grid_on_the_search_side() {
    check_grid(
        r#"<div style='display:grid;width:320px;grid-template-columns:[mark]30px[beta]40px[mark];
                  grid-template-rows:repeat(4,10px);grid-auto-columns:15px 25px;gap:0 5px'>
                <div id=left style='grid-row:1;grid-column:-3 mark/1'></div>
                <div id=right style='grid-row:2;grid-column:3 mark/span missing'></div>
                <div id=across style='grid-row:3;grid-column:span missing/5'></div>
                <div id=auto-span style='grid-row:4;grid-column:span 12 missing'></div>
           </div>"#,
        &[
            ("left", [0., 0., 25., 10.]),
            ("right", [130., 10., 25., 10.]),
            ("across", [0., 20., 155., 10.]),
            ("auto-span", [0., 30., 25., 10.]),
        ],
    );
}

#[test]
fn auto_fill_preserves_tracks_and_auto_fit_collapses_gutters_after_placement() {
    check_grid(
        r#"<div style='display:grid;width:430px;grid-template-columns:repeat(auto-fill,minmax(100px,1fr));
                       grid-template-rows:20px;gap:10px'>
                <div id=fill-a></div><div id=fill-b></div>
           </div>
           <div style='display:grid;width:430px;grid-template-columns:repeat(auto-fit,minmax(100px,1fr));
                       grid-template-rows:20px;gap:10px'>
                <div id=fit-a style='grid-column:1'></div><div id=fit-b style='grid-column:-2/-1'></div>
           </div>
           <div id=all-empty style='display:grid;width:430px;grid-template-rows:repeat(auto-fit,30px);gap:10px'></div>
           <div style='display:grid;width:500px;grid-template-columns:repeat(auto-fit,100px);
                       grid-template-rows:20px;justify-content:space-between;gap:10px'>
                <div id=distributed-a style='grid-column:1'></div><div id=distributed-b style='grid-column:4'></div>
           </div>"#,
        &[
            ("fill-a", [0., 0., 100., 20.]),
            ("fill-b", [110., 0., 100., 20.]),
            ("fit-a", [0., 20., 210., 20.]),
            ("fit-b", [220., 20., 210., 20.]),
            ("all-empty", [0., 40., 430., 0.]),
            ("distributed-a", [0., 40., 100., 20.]),
            ("distributed-b", [400., 40., 100., 20.]),
        ],
    );
}

#[test]
fn auto_fit_names_keep_outer_edges_and_spanning_tracks() {
    check_grid(
        r#"<div style='display:grid;width:470px;gap:0 10px;grid-template-rows:20px;
                 grid-template-columns:[frame]30px[band] repeat(auto-fit,[card]minmax(100px,1fr)[after-card])[trail]40px[end]'>
                <div id=first style='grid-row:1;grid-column:card/1 after-card'></div>
                <div id=last style='grid-row:1;grid-column:3 card/trail'></div>
                <div id=prefix style='grid-row:1;grid-column:frame/band'></div>
                <div id=suffix style='grid-row:1;grid-column:trail/end'></div>
           </div>
           <div style='display:grid;width:430px;gap:0 10px;grid-template-rows:20px;
                       grid-template-columns:repeat(auto-fit,[panel]minmax(100px,1fr))'>
                <div id=spanning style='grid-column:1/span 2'></div>
                <div id=after style='grid-column:-2'></div>
           </div>"#,
        &[
            ("first", [40., 0., 185., 20.]),
            ("last", [235., 0., 185., 20.]),
            ("prefix", [0., 0., 30., 20.]),
            ("suffix", [430., 0., 40., 20.]),
            ("spanning", [0., 20., 283.33333, 20.]),
            ("after", [293.33333, 20., 136.66667, 20.]),
        ],
    );
}

#[test]
fn auto_repeat_uses_definite_row_bounds_and_one_copy_for_unconstrained_axes() {
    check_grid(
        r#"<div style='display:grid;width:150px;min-height:85px;grid-auto-flow:column;
                      grid-template-rows:repeat(auto-fill,20px);grid-auto-columns:50px;row-gap:5px'>
                <div></div><div></div><div></div><div id=min-four></div><div id=min-five></div>
           </div>
           <div style='display:grid;width:150px;max-height:85px;grid-auto-flow:column;
                       grid-template-rows:repeat(auto-fill,20px);grid-auto-columns:50px;row-gap:5px'>
                <div></div><div></div><div></div><div id=max-four></div>
           </div>
           <div style='display:grid;width:150px;grid-auto-flow:column;
                       grid-template-rows:repeat(auto-fill,20px);grid-auto-columns:50px;row-gap:5px'>
                <div></div><div id=once></div>
           </div>
           <span id=inline style='display:inline-grid;grid-template-columns:repeat(auto-fill,30px);grid-auto-rows:14px;gap:2px'>
                <b></b><b></b>
           </span>"#,
        &[
            ("min-four", [0., 75., 50., 20.]),
            ("min-five", [50., 0., 50., 20.]),
            ("max-four", [50., 95., 50., 20.]),
            ("once", [50., 165., 50., 20.]),
            ("inline", [0., 185., 30., 30.]),
        ],
    );
}

#[test]
fn huge_auto_repetition_marks_a_bounded_partial_layout() {
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "about:grid",
        "<!doctype html><style>body{margin:0}</style>\
         <div style='display:grid;width:2048px;grid-template-columns:repeat(auto-fill,minmax(0,1fr));grid-template-rows:10px'>\
           <div id=last style='grid-column:-2/-1'></div>\
         </div>",
    );
    let layout = Layout::build(&page, &fonts, 800.);
    assert!(layout.limited);
    let id = page
        .document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some("last"))
        .unwrap();
    let last = layout.rect_for(id).unwrap();
    assert!(
        (last.x - 2032.).abs() < 0.05 && (last.width - 16.).abs() < 0.05,
        "{last:?}"
    );
}

#[test]
fn intrinsic_repetition_counts_percent_tracks_and_gutters_against_their_bases() {
    check_grid(
        r#"<div style='display:grid;width:100px;max-height:100px;
                      grid-template-rows:repeat(auto-fill,minmax(20px,40%))'>
                <div id=mixed style='grid-row:-2/-1;height:10px'></div>
           </div>
           <div style='display:grid;width:100px;max-height:100px;
                       grid-template-rows:repeat(auto-fill,20%)'>
                <div id=percent style='grid-row:-2/-1;height:10px'></div>
           </div>
           <div style='display:grid;width:100px;min-height:100px;
                       grid-template-rows:repeat(auto-fill,20px);row-gap:10%'>
                <div id=percent-gap style='grid-row:-2/-1;height:10px'></div>
           </div>"#,
        &[
            ("mixed", [0., 80., 100., 10.]),
            ("percent", [0., 298., 100., 10.]),
            ("percent-gap", [0., 230., 100., 10.]),
        ],
    );
}
