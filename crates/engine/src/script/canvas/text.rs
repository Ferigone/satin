//! Bind Canvas text drawing to the document font set and shared compositor.
mod shaped;

use super::{context_node, drawing::OutlineCommand};
use crate::{
    css::{
        self, Length,
        canvas_font::{Basis, Font},
    },
    dom::NodeId,
    fonts::MAX_SHAPE_INPUT_BYTES,
    script::{Shared, arg, layout_api::LayoutHost, webidl},
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use rustybuzz::ttf_parser::Face;
use std::{rc::Rc, sync::Arc};

#[derive(Clone)]
pub(super) struct TextState {
    font: Font,
    align: &'static str,
    baseline: &'static str,
    direction: &'static str,
    language: Arc<str>,
    letter_spacing: Spacing,
    word_spacing: Spacing,
    kerning: &'static str,
    stretch: &'static str,
    caps: &'static str,
    rendering: &'static str,
}

impl Default for TextState {
    fn default() -> Self {
        static INHERIT: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
        Self {
            font: Font::default(),
            align: "start",
            baseline: "alphabetic",
            direction: "inherit",
            language: INHERIT.get_or_init(|| "inherit".into()).clone(),
            letter_spacing: Spacing::default(),
            word_spacing: Spacing::default(),
            kerning: "auto",
            stretch: "normal",
            caps: "normal",
            rendering: "auto",
        }
    }
}

#[derive(Clone, Copy)]
enum Attribute {
    Font,
    Align,
    Baseline,
    Direction,
    Language,
    LetterSpacing,
    WordSpacing,
    Kerning,
    Stretch,
    Caps,
    Rendering,
}

impl Attribute {
    const ALL: [(&'static str, Self); 11] = [
        ("font", Self::Font),
        ("textAlign", Self::Align),
        ("textBaseline", Self::Baseline),
        ("direction", Self::Direction),
        ("lang", Self::Language),
        ("letterSpacing", Self::LetterSpacing),
        ("wordSpacing", Self::WordSpacing),
        ("fontKerning", Self::Kerning),
        ("fontStretch", Self::Stretch),
        ("fontVariantCaps", Self::Caps),
        ("textRendering", Self::Rendering),
    ];

    fn get(self, style: &TextState) -> Value {
        match self {
            Self::Font => style.font.serialized.as_ref().into(),
            Self::Align => style.align.into(),
            Self::Baseline => style.baseline.into(),
            Self::Direction => style.direction.into(),
            Self::Language => style.language.as_ref().into(),
            Self::LetterSpacing => style
                .letter_spacing
                .serialized
                .as_deref()
                .unwrap_or("0px")
                .into(),
            Self::WordSpacing => style
                .word_spacing
                .serialized
                .as_deref()
                .unwrap_or("0px")
                .into(),
            Self::Kerning => style.kerning.into(),
            Self::Stretch => style.stretch.into(),
            Self::Caps => style.caps.into(),
            Self::Rendering => style.rendering.into(),
        }
    }

    fn assign(self, style: &mut TextState, value: &str) {
        use Attribute::*;
        let (target, values): (&mut &str, &[&'static str]) = match self {
            Font => unreachable!("the font setter also resolves relative units"),
            Language => {
                style.language = value.into();
                return;
            }
            LetterSpacing | WordSpacing => {
                if let Some(length) = Spacing::parse(value) {
                    if matches!(self, LetterSpacing) {
                        style.letter_spacing = length;
                    } else {
                        style.word_spacing = length;
                    }
                }
                return;
            }
            Align => (
                &mut style.align,
                &["start", "end", "left", "right", "center"],
            ),
            Baseline => (
                &mut style.baseline,
                &[
                    "top",
                    "hanging",
                    "middle",
                    "alphabetic",
                    "ideographic",
                    "bottom",
                ],
            ),
            Direction => (&mut style.direction, &["ltr", "rtl", "inherit"]),
            Kerning => (&mut style.kerning, &["auto", "normal", "none"]),
            Stretch => (
                &mut style.stretch,
                &[
                    "ultra-condensed",
                    "extra-condensed",
                    "condensed",
                    "semi-condensed",
                    "normal",
                    "semi-expanded",
                    "expanded",
                    "extra-expanded",
                    "ultra-expanded",
                ],
            ),
            Caps => (
                &mut style.caps,
                &[
                    "normal",
                    "small-caps",
                    "all-small-caps",
                    "petite-caps",
                    "all-petite-caps",
                    "unicase",
                    "titling-caps",
                ],
            ),
            Rendering => (
                &mut style.rendering,
                &[
                    "auto",
                    "optimizeSpeed",
                    "optimizeLegibility",
                    "geometricPrecision",
                ],
            ),
        };
        if let Some(&valid) = values.iter().find(|&&entry| entry == value) {
            *target = valid;
        }
    }
}

#[derive(Clone)]
struct Spacing {
    length: SpacingLength,
    serialized: Option<Arc<str>>,
}

#[derive(Clone, Copy)]
enum SpacingLength {
    Css(Length),
    XHeight(f32),
    ZeroAdvance(f32),
    IdeographicAdvance(f32),
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            length: SpacingLength::Css(Length::Px(0.)),
            serialized: None,
        }
    }
}

impl Spacing {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        let numeric = value.trim_end_matches(|ch: char| ch.is_ascii_alphabetic());
        let unit = &value[numeric.len()..];
        let number = numeric.parse::<f32>().ok().filter(|n| n.is_finite())?;
        let number = if number == 0. { 0. } else { number };
        let length = match unit {
            "ex" => SpacingLength::XHeight(number),
            "ch" => SpacingLength::ZeroAdvance(number),
            "ic" => SpacingLength::IdeographicAdvance(number),
            _ => {
                let length = css::length(&value)?;
                match length {
                    Length::Px(n) if n.is_finite() => {}
                    Length::Em(_) | Length::Rem(_) | Length::Vw(_) => {}
                    _ => return None,
                }
                SpacingLength::Css(length)
            }
        };
        let unit = if unit.is_empty() { "px" } else { unit };
        Some(Self {
            length,
            serialized: Some(format!("{number}{unit}").into()),
        })
    }

    fn resolve(&self, viewport: f32, font_size: f32, face: &Face<'_>) -> f64 {
        let size = f64::from(font_size);
        let scale = size / f64::from(face.units_per_em());
        let advance = |ch, fallback| {
            face.glyph_index(ch)
                .and_then(|id| face.glyph_hor_advance(id))
                .map_or(fallback, |value| f64::from(value) * scale)
        };
        match self.length {
            SpacingLength::Css(length) => {
                f64::from(length.resolve(viewport, font_size).unwrap_or(0.))
            }
            SpacingLength::XHeight(number) => {
                let x_height = face
                    .x_height()
                    .map_or(size * 0.5, |value| f64::from(value) * scale);
                f64::from(number) * x_height
            }
            SpacingLength::ZeroAdvance(number) => f64::from(number) * advance('0', size * 0.5),
            SpacingLength::IdeographicAdvance(number) => f64::from(number) * advance('水', size),
        }
    }
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    prototype: Value,
    layout: &LayoutHost,
) -> Result<(), JsError> {
    let metrics = install_metrics(vm)?;
    for (name, attribute) in Attribute::ALL {
        let state_get = state.clone();
        let get = vm.new_native_function(&format!("get {name}"), move |_, this, _| {
            let id = context_node(&state_get, &this)?;
            let state = state_get.borrow();
            Ok(attribute.get(state.canvases.entries[&id].drawing.text()))
        });
        let state_set = state.clone();
        let host = layout.clone();
        let set = vm.new_native_function(&format!("set {name}"), move |vm, this, args| {
            let id = context_node(&state_set, &this)?;
            let value = webidl::usv_string(vm, &arg(args, 0), 4096)?;
            if matches!(attribute, Attribute::Font) {
                if let Some(font) = css::canvas_font::parse(&value) {
                    let basis = if font.uses_font_basis() {
                        host.canvas_font_basis(vm, id)?
                    } else {
                        Basis::default()
                    };
                    if let Some(font) = font.resolve(basis) {
                        state_set
                            .borrow_mut()
                            .canvases
                            .entries
                            .get_mut(&id)
                            .unwrap()
                            .drawing
                            .text_mut()
                            .font = font;
                    }
                }
            } else {
                let mut state = state_set.borrow_mut();
                let style = state
                    .canvases
                    .entries
                    .get_mut(&id)
                    .unwrap()
                    .drawing
                    .text_mut();
                attribute.assign(style, &value);
            }
            Ok(Value::Undefined)
        });
        vm.define_data_property(set.clone(), "length", 1.into(), false, false, true)?;
        vm.define_accessor(prototype.clone(), name, Some(get), Some(set))?;
    }
    for (name, stroke) in [("fillText", false), ("strokeText", true)] {
        let state = state.clone();
        let host = layout.clone();
        webidl::operation(vm, prototype.clone(), name, 3, move |vm, this, args| {
            let id = context_node(&state, &this)?;
            webidl::required(args, 3, name)?;
            let text = from_js(vm, &args[0])?;
            let x = vm.to_number(&args[1])?;
            let y = vm.to_number(&args[2])?;
            let max_width = args
                .get(3)
                .filter(|value| !matches!(value, Value::Undefined))
                .map(|value| vm.to_number(value))
                .transpose()?;
            if !x.is_finite()
                || !y.is_finite()
                || max_width.is_some_and(|w| !w.is_finite() || w <= 0.)
                || text.is_empty()
            {
                return Ok(Value::Undefined);
            }
            let (style, rtl, language) = style_for(&state, id);
            let run = shaped::prepare(vm, &host, &style, rtl, language.as_deref(), &text)?;
            let outline = run.outline(vm, x, y, max_width)?;
            if !outline.is_empty() {
                let mut state = state.borrow_mut();
                let canvas = state.canvases.entries.get_mut(&id).unwrap();
                if canvas
                    .drawing
                    .paint_outline(vm, &mut canvas.bitmap, &outline, stroke)?
                {
                    state.canvases.changed(id);
                }
            }
            Ok(Value::Undefined)
        })?;
    }
    let state = state.clone();
    let host = layout.clone();
    webidl::operation(vm, prototype, "measureText", 1, move |vm, this, args| {
        let id = context_node(&state, &this)?;
        webidl::required(args, 1, "measureText")?;
        let text = from_js(vm, &args[0])?;
        let (style, rtl, language) = style_for(&state, id);
        let run = shaped::prepare(vm, &host, &style, rtl, language.as_deref(), &text)?;
        let metrics_values = run.metrics(vm)?;
        let object = vm.new_object_with_prototype(metrics.clone())?;
        vm.set_host_data(object.clone(), Rc::new(metrics_values))?;
        Ok(object)
    })?;
    Ok(())
}

fn from_js(vm: &mut Vm, value: &Value) -> Result<String, JsError> {
    let mut text = webidl::usv_string(vm, value, MAX_SHAPE_INPUT_BYTES)?;
    if text
        .bytes()
        .any(|b| b == 0 || b.is_ascii_whitespace() && b != b' ')
    {
        text = text
            .chars()
            .filter(|&ch| ch != '\0')
            .map(|ch| if ch.is_ascii_whitespace() { ' ' } else { ch })
            .collect();
    }
    Ok(text)
}

fn style_for(state: &Shared, id: NodeId) -> (TextState, bool, Option<Arc<str>>) {
    let state = state.borrow();
    let style = state.canvases.entries[&id].drawing.text().clone();
    let mut direction = (style.direction != "inherit").then_some(style.direction);
    let mut language_known = style.language.as_ref() != "inherit";
    let mut language = language_known.then(|| style.language.clone());
    let mut ancestor = Some(id);
    while let Some(node) = ancestor.filter(|_| direction.is_none() || !language_known) {
        if direction.is_none() {
            direction = state.document.attr(node, "dir").and_then(|value| {
                if value.eq_ignore_ascii_case("rtl") {
                    Some("rtl")
                } else if value.eq_ignore_ascii_case("ltr") {
                    Some("ltr")
                } else {
                    None
                }
            });
        }
        if !language_known && let Some(value) = state.document.attr(node, "lang") {
            language = (value.len() <= 256).then(|| Arc::from(value));
            language_known = true;
        }
        ancestor = state.document.nodes[node.0].parent;
    }
    (
        style,
        direction == Some("rtl"),
        language.filter(|value| !value.is_empty()),
    )
}

struct Metrics([f64; 12]);

fn install_metrics(vm: &mut Vm) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "TextMetrics", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal constructor"))
    })?;
    vm.root(prototype.clone());
    for (index, property) in [
        "width",
        "actualBoundingBoxLeft",
        "actualBoundingBoxRight",
        "fontBoundingBoxAscent",
        "fontBoundingBoxDescent",
        "actualBoundingBoxAscent",
        "actualBoundingBoxDescent",
        "emHeightAscent",
        "emHeightDescent",
        "hangingBaseline",
        "alphabeticBaseline",
        "ideographicBaseline",
    ]
    .into_iter()
    .enumerate()
    {
        let get = vm.new_native_function(&format!("get {property}"), move |vm, this, _| {
            let metrics = vm
                .host_data::<Metrics>(&this)
                .ok_or_else(|| JsError::type_error("Illegal TextMetrics receiver"))?;
            Ok(Value::Number(metrics.0[index]))
        });
        vm.define_accessor(prototype.clone(), property, Some(get), None)?;
    }
    Ok(prototype)
}

fn limit(message: &str) -> JsError {
    JsError::new(ErrorKind::ExecutionLimit, message)
}
