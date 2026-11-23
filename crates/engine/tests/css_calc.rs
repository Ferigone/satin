use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../../tools/fixtures/css-calc.html");

fn viewport(width: f32, height: f32) -> Viewport {
    Viewport {
        width,
        height,
        ..Viewport::default()
    }
}

fn runtime() -> ScriptRuntime {
    load_fixture(FIXTURE)
}

fn load_fixture(fixture: &str) -> ScriptRuntime {
    let mut page = LoadedPage::from_html("https://example.test/css-calc.html", fixture);
    let runtime = ScriptRuntime::for_page_with_viewport(&mut page, viewport(800., 600.))
        .unwrap()
        .unwrap();
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
    runtime
}

fn snapshot(runtime: &mut ScriptRuntime) -> Value {
    json_result(runtime, "JSON.stringify(calcSnapshot())")
}

fn json_result(runtime: &mut ScriptRuntime, expression: &str) -> Value {
    let measured = runtime.eval(expression).unwrap();
    serde_json::from_str(&measured.as_string().unwrap().to_string_lossy()).unwrap()
}

fn geometry(result: &Value, name: &str, expected: [f64; 2]) {
    let values = result[name].as_array().unwrap();
    assert_eq!(values.len(), 2, "{name}: {result}");
    for (observed, expected) in values.iter().zip(expected) {
        let observed = observed.as_f64().unwrap();
        assert!(
            (observed - expected).abs() < 0.02,
            "{name}: expected {expected} CSS px, got {observed} in {result}"
        );
    }
}

#[test]
fn viewport_lengths_recalculate_for_each_axis_in_the_same_document() {
    let mut runtime = runtime();
    for (width, height, box_width, box_height) in [
        (800., 600., 420., 700.),
        (800., 430., 420., 530.),
        (620., 430., 330., 530.),
        (800., 600., 420., 700.),
    ] {
        runtime.set_viewport(viewport(width, height));
        let result = snapshot(&mut runtime);
        geometry(&result, "viewport", [f64::from(width), f64::from(height)]);
        geometry(&result, "viewportRect", [box_width, box_height]);
        geometry(&result, "initialViewportRect", [420., 700.]);
        assert_eq!(
            result["viewportStyle"],
            json!([format!("{box_width}px"), format!("{box_height}px")])
        );
    }
    let same = runtime
        .eval("String(viewportBox === document.getElementById('viewport'))")
        .unwrap();
    assert_eq!(same.as_string().unwrap().to_string_lossy(), "true");
}

#[test]
fn percentages_use_container_axes_and_live_cssom() {
    let mut runtime = runtime();
    let initial = snapshot(&mut runtime);
    geometry(&initial, "parentRect", [320., 240.]);
    geometry(&initial, "percentRect", [214., 90.]);
    assert_eq!(
        initial["percentStyle"],
        json!(["180px", "70px", "10px", "17px"])
    );

    runtime.eval("changeCalcContainer()").unwrap();
    let changed = snapshot(&mut runtime);
    geometry(&changed, "parentRect", [480., 320.]);
    geometry(&changed, "percentRect", [310., 118.]);
    assert_eq!(
        changed["percentStyle"],
        json!(["260px", "90px", "14px", "25px"])
    );

    runtime
        .eval("var container = calcElement('container'); container.style.boxSizing = 'border-box'; container.style.padding = '20px';")
        .unwrap();
    let padded = snapshot(&mut runtime);
    geometry(&padded, "parentRect", [480., 320.]);
    geometry(&padded, "percentRect", [286., 106.]);
    assert_eq!(
        padded["percentStyle"],
        json!(["240px", "80px", "13px", "23px"])
    );
}

#[test]
fn nested_arithmetic_and_absolute_and_font_relative_units_resolve() {
    let mut runtime = runtime();
    let initial = snapshot(&mut runtime);
    geometry(&initial, "math", [58., 192.]);
    geometry(&initial, "scalar", [10., 10.5]);
    assert_eq!(initial["font"], json!([54, "22px", "25px"]));

    runtime.eval("changeCalcRootFont()").unwrap();
    let changed = snapshot(&mut runtime);
    geometry(&changed, "math", [57., 192.]);
    assert_eq!(changed["font"], json!([50, "20px", "23px"]));
}

#[test]
fn percentage_heights_stay_indefinite_and_nonnegative_ranges_clamp() {
    let mut runtime = runtime();
    let initial = snapshot(&mut runtime);
    geometry(&initial, "indefinite", [14., 17.]);
    geometry(&initial, "constraints", [52., 15.]);
    assert_eq!(initial["negative"], json!([0, 0, -7, "0px"]));

    runtime.eval("changeCalcContainer()").unwrap();
    let changed = snapshot(&mut runtime);
    geometry(&changed, "constraints", [52., 15.]);
    assert_eq!(changed["negative"], json!([0, 0, -7, "0px"]));
}

#[test]
fn invalid_calculations_do_not_override_valid_declarations() {
    let mut runtime = runtime();
    assert_eq!(
        snapshot(&mut runtime)["invalid"],
        json!([73, 73, 73, 73, 73, 73, 73, 73, 11])
    );
    let width = runtime
        .eval(
            "var element = calcElement('valid-after-invalid'); \
             element.style.cssText = '--size: 15px; width: calc(var(/**/--size) + 10px);'; \
             element.style.width = 'calc(25px/**/+ 9px)'; \
             element.getBoundingClientRect().width",
        )
        .unwrap();
    assert_eq!(width, rust_browser_javascript::Value::Number(25.));
}

const GRID_TRANSFORM_FIXTURE: &str = r#"<!doctype html>
<style>
  body { margin: 0; }
  #grid {
    display: grid;
    width: 300px;
    grid-template-columns: calc(20% + 15px) calc(2 * 20px);
    column-gap: calc(1vw + 2px);
  }
  #grid div { height: 10px; }
  #translated {
    width: 100px;
    height: 40px;
    transform: translate(calc(50% - 10px), calc(25% + 7px));
  }
</style>
<div id="grid"><div id="first"></div><div id="second"></div></div>
<div id="translated"></div>
<script>
  var grid = document.getElementById('grid');
  var translated = document.getElementById('translated');
  var translationStyle = getComputedStyle(translated);
  function gridTransformSnapshot() {
    var first = document.getElementById('first').getBoundingClientRect();
    var second = document.getElementById('second').getBoundingClientRect();
    var moved = translated.getBoundingClientRect();
    return [first.width, second.width, second.left - grid.getBoundingClientRect().left,
      moved.left - translated.offsetLeft, moved.top - translated.offsetTop, translationStyle.transform];
  }
  function changeGridAndTransformSizes() {
    grid.style.width = '400px';
    translated.style.width = '120px';
    translated.style.height = '60px';
  }
</script>"#;

#[test]
fn grid_tracks_and_translates_resolve_against_their_own_boxes() {
    let mut runtime = load_fixture(GRID_TRANSFORM_FIXTURE);
    let measure = "JSON.stringify(gridTransformSnapshot())";
    assert_eq!(
        json_result(&mut runtime, measure),
        json!([75, 40, 85, 40, 17, "matrix(1, 0, 0, 1, 40, 17)"])
    );
    runtime.eval("changeGridAndTransformSizes()").unwrap();
    assert_eq!(
        json_result(&mut runtime, measure),
        json!([95, 40, 105, 50, 22, "matrix(1, 0, 0, 1, 50, 22)"])
    );
}

