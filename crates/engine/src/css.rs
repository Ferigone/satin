//! A bounded, independently implemented CSS subset. Selectors and declarations
//! are compiled once per layout; matching does not tokenize or allocate strings.
mod calc;
pub(crate) mod canvas_font;
mod grid;
pub(crate) mod inline_style;
pub(crate) use grid::GRID_TRACK_LIMIT;
pub use grid::{GridAreas, GridLine, GridTemplate};

use crate::dom::{Document, Namespace, NodeId};
use crate::fonts::{
    FontDisplay, FontFaceRule, FontFamily, FontFormat, FontSource, FontStyle, GenericFamily,
    UnicodeRange,
};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);
impl Color {
    pub const BLACK: Self = Self(0, 0, 0, 255);
    pub const WHITE: Self = Self(255, 255, 255, 255);
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b, 255)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(f32),
    Em(f32),
    Rem(f32),
    Vw(f32),
    Vh(f32),
    Percent(f32),
    /// The absolute and percentage terms of a computed calculation.
    Calc(f32, f32),
    /// The same terms, with a lower bound of zero at used-value time.
    ClampedCalc(f32, f32),
    Auto,
}

const _: () = assert!(std::mem::size_of::<Length>() <= 12);

/// The linear terms retained while a declaration awaits style computation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CalcLength {
    px: f32,
    percent: f32,
    em: f32,
    rem: f32,
    vw: f32,
    vh: f32,
    has_percent: bool,
    nonnegative: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum SpecifiedLength {
    Plain(Length),
    Calc(CalcLength),
}

impl From<Length> for SpecifiedLength {
    fn from(length: Length) -> Self {
        Self::Plain(length)
    }
}

impl CalcLength {
    fn absolute(self, root_font: f32, viewport: [f32; 2], font: f32) -> f64 {
        f64::from(self.px)
            + f64::from(self.em) * f64::from(font)
            + f64::from(self.rem) * f64::from(root_font)
            + f64::from(self.vw) * f64::from(viewport[0]) / 100.
            + f64::from(self.vh) * f64::from(viewport[1]) / 100.
    }
}

fn constrain_calculation(value: f64, nonnegative: bool) -> f32 {
    let lower = if nonnegative { 0. } else { -f32::MAX };
    value.clamp(f64::from(lower), f64::from(f32::MAX)) as f32
}

impl SpecifiedLength {
    fn has_percent(self) -> bool {
        match self {
            Self::Plain(length) => length.has_percent(),
            Self::Calc(calc) => calc.has_percent,
        }
    }

    fn needs_context(self) -> bool {
        match self {
            Self::Plain(Length::Rem(_) | Length::Vw(_) | Length::Vh(_)) => true,
            Self::Calc(calc) => [calc.em, calc.rem, calc.vw, calc.vh]
                .into_iter()
                .any(|n| n != 0.),
            _ => false,
        }
    }

    fn computed(self, root_font: f32, viewport: [f32; 2], font: f32) -> Length {
        match self {
            Self::Plain(Length::Rem(v)) => Length::Px(v * root_font),
            Self::Plain(Length::Vw(v)) => Length::Px(v * viewport[0] / 100.),
            Self::Plain(Length::Vh(v)) => Length::Px(v * viewport[1] / 100.),
            Self::Plain(length) => length,
            Self::Calc(calc) => {
                let px = calc.absolute(root_font, viewport, font);
                if !calc.has_percent {
                    return Length::Px(constrain_calculation(px, calc.nonnegative));
                }
                // A negative absolute term may cancel against a positive
                // percentage, so the range applies only after combining them.
                let px = constrain_calculation(px, false);
                if calc.nonnegative {
                    Length::ClampedCalc(px, calc.percent)
                } else {
                    Length::Calc(px, calc.percent)
                }
            }
        }
    }
}

impl Length {
    pub fn has_percent(self) -> bool {
        matches!(
            self,
            Self::Percent(_) | Self::Calc(..) | Self::ClampedCalc(..)
        )
    }

    pub(crate) fn percentage_calculation(self) -> Option<(f32, f32)> {
        match self {
            Self::Calc(px, percent) | Self::ClampedCalc(px, percent) => Some((px, percent)),
            _ => None,
        }
    }

    pub(crate) fn resolve_with_basis(self, base: Option<f32>, font: f32) -> Option<f32> {
        if base.is_none() && self.has_percent() {
            return None;
        }
        self.resolve(base.unwrap_or(0.), font)
    }

    pub fn resolve(self, base: f32, font: f32) -> Option<f32> {
        match self {
            Self::Px(v) => Some(v),
            Self::Em(v) => Some(v * font),
            Self::Rem(v) => Some(v * 16.),
            Self::Vw(v) | Self::Vh(v) | Self::Percent(v) => Some(base * v / 100.),
            Self::Calc(px, percent) | Self::ClampedCalc(px, percent) => {
                let value = f64::from(px) + f64::from(base) * f64::from(percent) / 100.;
                Some(constrain_calculation(
                    value,
                    matches!(self, Self::ClampedCalc(..)),
                ))
            }
            Self::Auto => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Float {
    #[default]
    None,
    Left,
    Right,
    InlineStart,
    InlineEnd,
}
impl Float {
    pub(crate) fn as_css(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Left => "left",
            Self::Right => "right",
            Self::InlineStart => "inline-start",
            Self::InlineEnd => "inline-end",
        }
    }
    pub(crate) fn is_right(self, rtl: bool) -> bool {
        match self {
            Self::None | Self::Left => false,
            Self::Right => true,
            Self::InlineStart => rtl,
            Self::InlineEnd => !rtl,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Clear {
    #[default]
    None,
    Left,
    Right,
    Both,
    InlineStart,
    InlineEnd,
}
impl Clear {
    pub(crate) fn as_css(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Left => "left",
            Self::Right => "right",
            Self::Both => "both",
            Self::InlineStart => "inline-start",
            Self::InlineEnd => "inline-end",
        }
    }
    pub(crate) fn sides(self, rtl: bool) -> [bool; 2] {
        match self {
            Self::None => [false, false],
            Self::Both => [true, true],
            Self::Left => [true, false],
            Self::Right => [false, true],
            Self::InlineStart => [!rtl, rtl],
            Self::InlineEnd => [rtl, !rtl],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    #[default]
    Static,
    Relative,
    Sticky,
    Absolute,
    Fixed,
}
impl Position {
    pub fn out_of_flow(self) -> bool {
        matches!(self, Self::Absolute | Self::Fixed)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Clip,
    Auto,
    Scroll,
}
impl Overflow {
    pub fn clips(self) -> bool {
        self != Self::Visible
    }
    pub fn scrollable(self) -> bool {
        matches!(self, Self::Hidden | Self::Auto | Self::Scroll)
    }
    pub fn user_scrollable(self) -> bool {
        matches!(self, Self::Auto | Self::Scroll)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransformOp<L = Length> {
    Matrix([f32; 6]),
    Translate([L; 2]),
    Scale([f32; 2]),
    Rotate(f32),
    Skew([f32; 2]),
}
impl<L> TransformOp<L> {
    fn map_lengths<M>(self, map: impl FnMut(L) -> M) -> TransformOp<M> {
        match self {
            Self::Translate(values) => TransformOp::Translate(values.map(map)),
            Self::Matrix(values) => TransformOp::Matrix(values),
            Self::Scale(values) => TransformOp::Scale(values),
            Self::Rotate(value) => TransformOp::Rotate(value),
            Self::Skew(values) => TransformOp::Skew(values),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    End,
    Center,
    Stretch,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GridBreadth<L = Length> {
    Length(L),
    Flex(f32),
    Auto,
    MinContent,
    MaxContent,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridTrack<L = Length> {
    pub min: GridBreadth<L>,
    pub max: GridBreadth<L>,
}
impl<L> Default for GridTrack<L> {
    fn default() -> Self {
        Self {
            min: GridBreadth::Auto,
            max: GridBreadth::Auto,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridAutoFlow {
    #[default]
    Row,
    Column,
}

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub float: Float,
    pub clear: Clear,
    pub position: Position,
    pub inset: [Length; 4],
    pub z_index: Option<i32>,
    pub overflow: [Overflow; 2],
    pub transform: Arc<Vec<TransformOp>>,
    pub transform_origin: [Length; 2],
    pub color: Color,
    pub background: Option<Color>,
    pub font_size: f32,
    pub font_family: Arc<[FontFamily]>,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub underline: bool,
    pub pre: bool,
    pub wrap: bool,
    pub break_words: bool,
    pub align: String,
    pub line_height: f32,
    /// Length-valued line heights inherit their computed pixel value.
    pub line_height_absolute: Option<f32>,
    pub margin: [Length; 4],
    pub padding: [Length; 4],
    /// Compatibility summaries; layout uses the per-side fields below.
    pub border: f32,
    pub border_color: Color,
    pub border_width: [f32; 4],
    pub border_visible: [bool; 4],
    pub border_colors: [Option<Color>; 4],
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub max_width: Length,
    pub min_height: Length,
    pub max_height: Length,
    pub box_sizing: BoxSizing,
    pub flex_direction: FlexDirection,
    pub flex_wrap: bool,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub justify_content: Align,
    pub align_items: Align,
    pub align_self: Align,
    pub gap: [Length; 2],
    pub order: i32,
    pub grid_template_columns: Arc<GridTemplate>,
    pub grid_template_rows: Arc<GridTemplate>,
    pub grid_template_areas: Arc<GridAreas>,
    pub grid_auto_columns: Arc<Vec<GridTrack>>,
    pub grid_auto_rows: Arc<Vec<GridTrack>>,
    pub grid_column: [GridLine; 2],
    pub grid_row: [GridLine; 2],
    pub grid_auto_flow: GridAutoFlow,
    pub grid_dense: bool,
    pub justify_items: Align,
    pub justify_self: Align,
    pub align_content: Align,
    pub svg_overrides: Arc<HashMap<Arc<str>, Arc<str>>>,
    /// Inherited inline direction for logical box edges in horizontal writing.
    pub(crate) rtl: bool,
    custom: Arc<HashMap<Arc<str>, Arc<str>>>,
}
impl Default for Style {
    fn default() -> Self {
        static EMPTY: OnceLock<Arc<HashMap<Arc<str>, Arc<str>>>> = OnceLock::new();
        static EMPTY_TEMPLATE: OnceLock<Arc<GridTemplate>> = OnceLock::new();
        static EMPTY_AREAS: OnceLock<Arc<GridAreas>> = OnceLock::new();
        static AUTO_TRACKS: OnceLock<Arc<Vec<GridTrack>>> = OnceLock::new();
        static DEFAULT_FAMILIES: OnceLock<Arc<[FontFamily]>> = OnceLock::new();
        Self {
            display: Display::Inline,
            float: Float::None,
            clear: Clear::None,
            position: Position::Static,
            inset: [Length::Auto; 4],
            z_index: None,
            overflow: [Overflow::Visible; 2],
            transform: Arc::new(vec![]),
            transform_origin: [Length::Percent(50.); 2],
            color: Color::BLACK,
            background: None,
            font_size: 16.,
            font_family: DEFAULT_FAMILIES
                .get_or_init(|| vec![FontFamily::Named(Arc::from("Segoe UI"))].into())
                .clone(),
            font_weight: 400,
            font_style: FontStyle::Normal,
            bold: false,
            italic: false,
            mono: false,
            underline: false,
            pre: false,
            wrap: true,
            break_words: false,
            align: "left".into(),
            line_height: 1.2,
            line_height_absolute: None,
            margin: [Length::Px(0.); 4],
            padding: [Length::Px(0.); 4],
            border: 0.,
            border_color: Color::BLACK,
            border_width: [3.; 4],
            border_visible: [false; 4],
            border_colors: [None; 4],
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Auto,
            max_width: Length::Auto,
            min_height: Length::Auto,
            max_height: Length::Auto,
            box_sizing: BoxSizing::ContentBox,
            flex_direction: FlexDirection::Row,
            flex_wrap: false,
            flex_grow: 0.,
            flex_shrink: 1.,
            flex_basis: Length::Auto,
            justify_content: Align::Stretch,
            align_items: Align::Stretch,
            align_self: Align::Auto,
            gap: [Length::Px(0.); 2],
            order: 0,
            grid_template_columns: EMPTY_TEMPLATE.get_or_init(Arc::default).clone(),
            grid_template_rows: EMPTY_TEMPLATE.get_or_init(Arc::default).clone(),
            grid_template_areas: EMPTY_AREAS.get_or_init(Arc::default).clone(),
            grid_auto_columns: AUTO_TRACKS
                .get_or_init(|| Arc::new(vec![GridTrack::default()]))
                .clone(),
            grid_auto_rows: AUTO_TRACKS
                .get_or_init(|| Arc::new(vec![GridTrack::default()]))
                .clone(),
            grid_column: Default::default(),
            grid_row: Default::default(),
            grid_auto_flow: GridAutoFlow::Row,
            grid_dense: false,
            justify_items: Align::Stretch,
            justify_self: Align::Auto,
            align_content: Align::Stretch,
            svg_overrides: EMPTY.get_or_init(|| Arc::new(HashMap::new())).clone(),
            rtl: false,
            custom: EMPTY.get_or_init(|| Arc::new(HashMap::new())).clone(),
        }
    }
}
impl Style {
    pub fn font_request(&self) -> crate::fonts::FontRequest {
        crate::fonts::FontRequest {
            families: self.font_family.clone(),
            weight: self.font_weight,
            style: self.font_style,
        }
    }
    pub fn font_family_css(&self) -> String {
        self.font_family
            .iter()
            .map(|f| match f {
                FontFamily::Generic(g) => g.as_css().to_owned(),
                FontFamily::Named(n) => {
                    let mut i = 0;
                    let plain = identifier(n, &mut i)
                        .is_some_and(|name| name.as_str() == n.as_ref())
                        && i == n.len()
                        && font_families(n).is_some_and(|list| {
                            matches!(list.as_slice(),
                            [FontFamily::Named(name)] if name == n)
                        });
                    if plain {
                        n.to_string()
                    } else {
                        format!(
                            "\"{}\"",
                            n.replace('\\', "\\\\")
                                .replace('"', "\\\"")
                                .replace('\n', "\\a ")
                                .replace('\r', "\\d ")
                                .replace('\u{c}', "\\c ")
                        )
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub fn svg_style(&self) -> crate::svg::SvgStyle {
        let mut style = crate::svg::SvgStyle::default();
        for (name, value) in self.svg_overrides.iter() {
            style.parse_property(name, value);
        }
        style
    }
    pub fn custom_property(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(|value| value.as_ref())
    }
    pub fn line_height_px(&self) -> f32 {
        self.line_height_absolute
            .unwrap_or(self.font_size * self.line_height)
    }
    pub fn borders(&self) -> [f32; 4] {
        std::array::from_fn(|i| {
            if self.border_visible[i] {
                self.border_width[i]
            } else {
                0.
            }
        })
    }
}

#[derive(Clone, Debug)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}
#[derive(Clone, Debug)]
pub struct Rule {
    pub selector: String,
    pub declarations: Vec<Declaration>,
    pub specificity: u32,
}

// A raw newline ends a bad-string token; the caller resumes at that newline.
// Treat CRLF and FF as CSS newlines without changing the stored source text.
fn quoted_end(bytes: &[u8], open: usize) -> Result<usize, usize> {
    let quote = bytes[open];
    let mut i = open + 1;
    while let Some(&b) = bytes.get(i) {
        match b {
            b if b == quote => return Ok(i + 1),
            b'\n' | b'\r' | b'\x0c' => return Err(i),
            b'\\' => {
                i += 1;
                let start = i;
                while i - start < 6 && bytes.get(i).is_some_and(u8::is_ascii_hexdigit) {
                    i += 1;
                }
                // A newline can be escaped directly or consumed as the
                // optional whitespace at the end of a hexadecimal escape.
                i += match bytes.get(i) {
                    Some(b'\r') if bytes.get(i + 1) == Some(&b'\n') => 2,
                    Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') => 1,
                    Some(_) if i == start => 1,
                    _ => 0,
                };
            }
            _ => i += 1,
        }
    }
    Ok(i)
}

// Comments separate CSS tokens without creating a whitespace token. The private
// NUL boundary preserves that difference until a grammar consumes the tokens.
// Raw NUL input is normalized to U+FFFD, so it cannot impersonate a boundary.
// Public rules/declarations serialize boundaries back as comments.
fn without_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            let end = (i + 2).min(bytes.len());
            // Copy bytes rather than slicing through a possibly multibyte escape.
            out.push('\\');
            i += 1;
            if end > i {
                let c = source[i..].chars().next().unwrap();
                out.push(if c == '\0' { '\u{fffd}' } else { c });
                i += c.len_utf8();
            }
        } else if matches!(b, b'\'' | b'"') {
            let end = quoted_end(bytes, i).unwrap_or_else(|newline| newline);
            out.extend(
                source[i..end]
                    .chars()
                    .map(|c| if c == '\0' { '\u{fffd}' } else { c }),
            );
            i = end;
        } else if bytes[i..].starts_with(b"/*") {
            i = source[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |n| i + n + 4);
            if !out.ends_with('\0') {
                out.push('\0');
            }
        } else {
            let c = source[i..].chars().next().unwrap();
            out.push(if c == '\0' { '\u{fffd}' } else { c });
            i += c.len_utf8();
        }
    }
    out
}
fn trim_boundary(source: &str) -> &str {
    source.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\0')
}
fn serialized_tokens(source: &str) -> String {
    source.replace('\0', "/**/")
}

enum CssByte {
    Unquoted(u8),
    BadString,
}

fn structural_bytes(source: &str) -> impl Iterator<Item = (usize, CssByte)> + '_ {
    let bytes = source.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while let Some(&b) = bytes.get(i) {
            let start = i;
            match b {
                b'\\' => i += 2,
                b'\'' | b'"' => match quoted_end(bytes, start) {
                    Ok(end) => i = end,
                    Err(newline) => {
                        i = newline;
                        return Some((start, CssByte::BadString));
                    }
                },
                _ => {
                    i += 1;
                    return Some((start, CssByte::Unquoted(b)));
                }
            }
        }
        None
    })
}

/// Top-level separators and bad strings (including within simple blocks).
fn component_boundaries(
    source: &str,
    separator: u8,
) -> impl Iterator<Item = (usize, CssByte)> + '_ {
    let (mut round, mut square, mut curly) = (0u32, 0u32, 0u32);
    structural_bytes(source).filter(move |(_, token)| {
        let CssByte::Unquoted(b) = *token else {
            return true;
        };
        let is_separator = round == 0 && square == 0 && curly == 0 && b == separator;
        match b {
            b'(' => round += 1,
            b')' => round = round.saturating_sub(1),
            b'[' => square += 1,
            b']' => square = square.saturating_sub(1),
            b'{' => curly += 1,
            b'}' => curly = curly.saturating_sub(1),
            _ => {}
        }
        is_separator
    })
}

/// Indices of a separator at the top level of a CSS component value list.
fn separators(source: &str, separator: u8) -> Vec<usize> {
    component_boundaries(source, separator)
        .filter_map(|(i, token)| matches!(token, CssByte::Unquoted(_)).then_some(i))
        .collect()
}
fn split_top(source: &str, separator: u8) -> Vec<&str> {
    let mut result = vec![];
    let mut start = 0;
    for i in separators(source, separator) {
        result.push(source[start..i].trim());
        start = i + 1;
    }
    result.push(source[start..].trim());
    result
}
fn matching_close(source: &str, open: usize, left: u8, right: u8) -> Option<usize> {
    let mut depth = 0u32;
    for (i, token) in structural_bytes(source.get(open..)?) {
        let CssByte::Unquoted(b) = token else {
            continue;
        };
        if b == left {
            depth += 1;
        } else if b == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + i);
            }
        }
    }
    None
}

fn unquoted_stylesheet_url_end(source: &[u8], open: usize) -> Option<usize> {
    let body = &source[open + 1..];
    let first = body
        .iter()
        .copied()
        .find(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c'));
    if matches!(first, Some(b'\'' | b'"')) {
        return None;
    }
    let mut i = open + 1;
    while let Some(&b) = source.get(i) {
        match b {
            b')' => return Some(i + 1),
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    Some(source.len())
}

fn stylesheet_block_end(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = open + 1;
    let mut closers = Vec::new();
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\'' | b'"' => {
                i = quoted_end(bytes, i).unwrap_or_else(|newline| newline);
                continue;
            }
            b if css_name_byte(b) || b == b'\\' => {
                // Consume the whole name, including any dimension unit, so a
                // suffix of a name cannot be mistaken for a url() token.
                let mut is_url = !matches!(bytes[i - 1], b'#' | b'@');
                let mut expected = "url".chars();
                if consume_name(source, &mut i, |c| {
                    is_url = is_url && expected.next().is_some_and(|e| c.eq_ignore_ascii_case(&e));
                })
                .is_none()
                {
                    // An invalid escape leaves the cursor at the backslash.
                    i += 2;
                } else if is_url
                    && expected.next().is_none()
                    && bytes.get(i) == Some(&b'(')
                    && let Some(end) = unquoted_stylesheet_url_end(bytes, i)
                {
                    // An unquoted url() is a token, not a component block.
                    i = end;
                }
                continue;
            }
            b'(' => closers.push(b')'),
            b'[' => closers.push(b']'),
            b'{' => closers.push(b'}'),
            b'}' if closers.is_empty() => return Some(i),
            // Other closing tokens within a component cannot close it.
            b')' | b']' | b'}' if closers.last() == Some(&b) => {
                closers.pop();
            }
            _ => {}
        }
        i += 1;
    }
    None
}

pub fn parse(source: &str, viewport: f32) -> Vec<Rule> {
    let clean = without_comments(source);
    let mut out = vec![];
    // Bound selector-list expansion by bytes as well as rule/declaration count;
    // one enormous declaration must not be copied thousands of times.
    let mut declaration_budget = 16 * 1024 * 1024;
    parse_rules(&clean, viewport, 0, &mut out, &mut declaration_budget);
    out
}

/// Bounded descriptor parsing; each returned URL belongs to the containing
/// stylesheet's response URL, which the loader resolves before downloading.
pub fn font_faces(source: &str, viewport: f32) -> Vec<FontFaceRule> {
    fn collect(source: &str, viewport: f32, depth: usize, out: &mut Vec<FontFaceRule>) {
        if depth > 32 || out.len() >= 32 {
            return;
        }
        let mut offset = 0;
        while offset < source.len() && out.len() < 32 {
            let tail = &source[offset..];
            let Some((stop, byte)) = first_top_delimiter(tail, b"{;") else {
                break;
            };
            if byte == b';' {
                offset += stop + 1;
                continue;
            }
            let open = offset + stop;
            let header = trim_boundary(&source[offset..open]);
            let Some(end) = stylesheet_block_end(source, open) else {
                break;
            };
            let body = &source[open + 1..end];
            if header.eq_ignore_ascii_case("@font-face") {
                if let Some(rule) = font_face_rule(body) {
                    out.push(rule);
                }
            } else if header
                .get(..6)
                .is_some_and(|h| h.eq_ignore_ascii_case("@media"))
                && media_allows(&header[6..], viewport)
            {
                collect(body, viewport, depth + 1, out);
            }
            offset = end + 1;
        }
    }
    let source = without_comments(source);
    let mut result = vec![];
    collect(&source, viewport, 0, &mut result);
    result
}
pub fn font_families(source: &str) -> Option<Vec<FontFamily>> {
    if source.len() > 4096 {
        return None;
    }
    let clean = without_comments(source).replace('\0', " ");
    let mut result = vec![];
    for part in split_top(&clean, b',') {
        if result.len() >= 16 {
            return None;
        }
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        if part.starts_with(['\'', '"']) {
            let name = font_string(part)?;
            if name.is_empty() || name.len() > 256 {
                return None;
            }
            result.push(FontFamily::Named(name.into()));
        } else {
            let mut offset = 0;
            let mut words = vec![];
            while offset < part.len() {
                while part
                    .as_bytes()
                    .get(offset)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    offset += 1;
                }
                if offset == part.len() {
                    break;
                }
                if part.as_bytes()[offset].is_ascii_digit()
                    || part.as_bytes()[offset] == b'-'
                        && part
                            .as_bytes()
                            .get(offset + 1)
                            .is_some_and(u8::is_ascii_digit)
                {
                    return None;
                }
                let word = identifier(part, &mut offset)?;
                if matches!(
                    word.to_ascii_lowercase().as_str(),
                    "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
                ) {
                    return None;
                }
                words.push(word);
                if offset < part.len() && !part.as_bytes()[offset].is_ascii_whitespace() {
                    return None;
                }
            }
            let name = words.join(" ");
            if name.is_empty() || name.len() > 256 {
                return None;
            }
            if let Some(generic) = generic_family(&name) {
                result.push(FontFamily::Generic(generic));
            } else {
                result.push(FontFamily::Named(name.into()));
            }
        }
    }
    (!result.is_empty()).then_some(result)
}
fn generic_family(source: &str) -> Option<GenericFamily> {
    Some(match source.to_ascii_lowercase().as_str() {
        "serif" => GenericFamily::Serif,
        "sans-serif" => GenericFamily::SansSerif,
        "monospace" => GenericFamily::Monospace,
        "cursive" => GenericFamily::Cursive,
        "fantasy" => GenericFamily::Fantasy,
        "system-ui" => GenericFamily::SystemUi,
        "ui-serif" => GenericFamily::UiSerif,
        "ui-sans-serif" => GenericFamily::UiSansSerif,
        "ui-monospace" => GenericFamily::UiMonospace,
        "ui-rounded" => GenericFamily::UiRounded,
        "emoji" => GenericFamily::Emoji,
        "math" => GenericFamily::Math,
        "fangsong" => GenericFamily::FangSong,
        _ => return None,
    })
}
fn absolute_weight(source: &str) -> Option<u16> {
    let lowercase = source.trim().to_ascii_lowercase();
    match lowercase.as_str() {
        "normal" => Some(400),
        "bold" => Some(700),
        value => number(value)
            .filter(|n| *n >= 1. && *n <= 1000.)
            .map(|n| n.round() as u16),
    }
}
fn parse_font_style(source: &str) -> Option<FontStyle> {
    Some(match source.trim().to_ascii_lowercase().as_str() {
        "normal" => FontStyle::Normal,
        "italic" => FontStyle::Italic,
        "oblique" => FontStyle::Oblique,
        _ => return None,
    })
}
fn font_face_rule(body: &str) -> Option<FontFaceRule> {
    let mut family = None;
    let mut sources = None;
    let mut weight = [400; 2];
    let mut style = FontStyle::Normal;
    let mut ranges = vec![UnicodeRange {
        start: 0,
        end: 0x10ffff,
    }];
    let mut display = FontDisplay::Auto;
    for d in declarations(body).into_iter().filter(|d| !d.important) {
        match d.name.as_str() {
            "font-family" => {
                if let Some(values) = font_families(&d.value)
                    && let [FontFamily::Named(name)] = values.as_slice()
                {
                    family = Some(name.clone());
                }
            }
            "src" => {
                if let Some(values) = font_sources(&d.value) {
                    sources = Some(values);
                }
            }
            "font-style" => {
                if let Some(value) = parse_font_style(&d.value) {
                    style = value;
                }
            }
            "font-weight" => {
                let parts = whitespace_tokens(&d.value);
                if (1..=2).contains(&parts.len())
                    && let Some(a) = absolute_weight(parts[0])
                    && let Some(b) = absolute_weight(parts.get(1).copied().unwrap_or(parts[0]))
                {
                    // CSS Fonts normalizes descending descriptor ranges before matching.
                    weight = [a.min(b), a.max(b)];
                }
            }
            "unicode-range" => {
                if let Some(value) = unicode_ranges(&d.value) {
                    ranges = value;
                }
            }
            "font-display" => match d.value.trim() {
                "auto" => display = FontDisplay::Auto,
                "block" => display = FontDisplay::Block,
                "swap" => display = FontDisplay::Swap,
                "fallback" => display = FontDisplay::Fallback,
                "optional" => display = FontDisplay::Optional,
                _ => {}
            },
            _ => {}
        }
    }
    Some(FontFaceRule {
        family: family?,
        weight,
        style,
        sources: sources?,
        unicode_ranges: ranges,
        display,
    })
}
fn font_string(source: &str) -> Option<String> {
    let quote = *source.as_bytes().first()?;
    if !matches!(quote, b'\'' | b'"') {
        return None;
    }
    let mut escaped = false;
    for (index, byte) in source.bytes().enumerate().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return if index + 1 == source.len() {
                unescape_string(&source[1..index])
            } else {
                None
            };
        } else if matches!(byte, b'\n' | b'\r' | 12) {
            return None;
        }
    }
    None
}

fn font_component(source: &str) -> Option<(&str, &str, &str)> {
    let open = source.find('(')?;
    let name = &source[..open];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let close = matching_close(source, open, b'(', b')')?;
    Some((
        name,
        source[open + 1..close].trim(),
        source[close + 1..].trim(),
    ))
}

fn unquoted_font_url(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            index += 1;
            let start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_hexdigit) && index - start < 6 {
                index += 1;
            }
            if index > start {
                if bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                    index += 1;
                }
            } else {
                let ch = source.get(index..)?.chars().next()?;
                if matches!(ch, '\r' | '\n' | '\u{c}') {
                    return None;
                }
                index += ch.len_utf8();
            }
        } else {
            if byte.is_ascii_whitespace()
                || byte.is_ascii_control()
                || matches!(byte, b'\'' | b'"' | b'(' | b')')
            {
                return None;
            }
            index += 1;
        }
    }
    unescape_string(source)
}

fn font_sources(source: &str) -> Option<Vec<FontSource>> {
    let value = without_comments(source);
    let value = value.replace('\0', " ");
    let parts = split_top(&value, b',');
    if parts.len() > 16 {
        return None;
    }
    let mut result = Vec::new();
    for part in parts {
        let Some((name, source, mut rest)) = font_component(part) else {
            continue;
        };
        if source.is_empty() || source.len() > 65536 {
            continue;
        }
        if name.eq_ignore_ascii_case("local") && rest.is_empty() {
            let local = if source.starts_with(['\'', '"']) {
                font_string(source)
            } else {
                font_families(source).and_then(|families| match families.as_slice() {
                    [FontFamily::Named(name)] => Some(name.to_string()),
                    _ => None,
                })
            };
            if let Some(name) = local.filter(|name| !name.is_empty() && name.len() <= 256) {
                result.push(FontSource::Local(name.into()));
            }
        } else if name.eq_ignore_ascii_case("url") {
            let url = if source.starts_with(['\'', '"']) {
                font_string(source)
            } else {
                unquoted_font_url(source)
            };
            let Some(url) = url.filter(|url| !url.is_empty()) else {
                continue;
            };
            let mut format = None;
            if let Some((kind, value, suffix)) = font_component(rest)
                && kind.eq_ignore_ascii_case("format")
            {
                let value = if value.starts_with(['\'', '"']) {
                    font_string(value)
                } else {
                    Some(value.to_owned())
                };
                let Some(value) = value else {
                    continue;
                };
                format = Some(match value.to_ascii_lowercase().as_str() {
                    "truetype" | "truetype-variations" => FontFormat::TrueType,
                    "opentype" | "opentype-variations" => FontFormat::OpenType,
                    "woff" | "woff-variations" => FontFormat::Woff,
                    "woff2" | "woff2-variations" => FontFormat::Woff2,
                    _ => FontFormat::Unsupported,
                });
                rest = suffix;
            }
            if let Some((kind, value, suffix)) = font_component(rest)
                && kind.eq_ignore_ascii_case("tech")
            {
                if !value.eq_ignore_ascii_case("variations") {
                    format = Some(FontFormat::Unsupported);
                }
                rest = suffix;
            }
            if rest.is_empty() {
                result.push(FontSource::Url {
                    url: url.into(),
                    format,
                });
            }
        }
    }
    (!result.is_empty()).then_some(result)
}
fn unicode_ranges(source: &str) -> Option<Vec<UnicodeRange>> {
    fn codepoint(digits: &str) -> Option<u32> {
        if !(1..=6).contains(&digits.len()) || !digits.as_bytes().iter().all(u8::is_ascii_hexdigit)
        {
            return None;
        }
        u32::from_str_radix(digits, 16).ok()
    }
    let mut result = vec![];
    for part in split_top(source, b',') {
        if result.len() >= 256 {
            return None;
        }
        let lower = part.trim().to_ascii_lowercase();
        let value = lower.strip_prefix("u+")?;
        let (start, end) = if value.contains('?') {
            if value.len() > 6
                || !value
                    .trim_end_matches('?')
                    .chars()
                    .all(|c| c.is_ascii_hexdigit())
                || !value.chars().all(|c| c.is_ascii_hexdigit() || c == '?')
            {
                return None;
            }
            (
                u32::from_str_radix(&value.replace('?', "0"), 16).ok()?,
                u32::from_str_radix(&value.replace('?', "f"), 16).ok()?,
            )
        } else if let Some((a, b)) = value.split_once('-') {
            (codepoint(a)?, codepoint(b)?)
        } else {
            let n = codepoint(value)?;
            (n, n)
        };
        // An out-of-bounds endpoint invalidates the descriptor; do not clip it.
        if start > end || end > 0x10ffff {
            return None;
        }
        result.push(UnicodeRange { start, end });
    }
    (!result.is_empty()).then_some(result)
}
fn parse_rules(
    source: &str,
    viewport: f32,
    depth: usize,
    out: &mut Vec<Rule>,
    declaration_budget: &mut usize,
) {
    if depth > 32 || out.len() >= 8192 {
        return;
    }
    let mut offset = 0;
    while offset < source.len() && out.len() < 8192 {
        let tail = &source[offset..];
        // Semicolon at-rules (e.g. @charset) must not absorb the next rule.
        let Some((stop, byte)) = first_top_delimiter(tail, b"{;") else {
            break;
        };
        if byte == b';' {
            offset += stop + 1;
            continue;
        }
        let open = offset + stop;
        let header = trim_boundary(&source[offset..open]);
        let end = stylesheet_block_end(source, open).unwrap_or(source.len());
        let body = &source[open + 1..end];
        if header
            .get(..6)
            .is_some_and(|s| s.eq_ignore_ascii_case("@media"))
        {
            if media_allows(&header[6..], viewport) {
                parse_rules(body, viewport, depth + 1, out, declaration_budget);
            }
        } else if !header.starts_with('@') {
            let declarations = clean_declarations(body);
            let declaration_cost: usize = declarations
                .iter()
                .map(|d| d.name.len() + d.value.len() + std::mem::size_of::<Declaration>())
                .sum();
            // A regular selector list is invalid as a whole if any selector fails.
            let selectors = split_top(header, b',');
            let compiled: Option<Vec<_>> =
                selectors.iter().map(|s| compile_selector(s, 0)).collect();
            if let Some(compiled) = compiled {
                for (selector, compiled) in selectors.into_iter().zip(compiled) {
                    if out.len() >= 8192 {
                        break;
                    }
                    if declaration_cost > *declaration_budget {
                        break;
                    }
                    *declaration_budget -= declaration_cost;
                    out.push(Rule {
                        selector: serialized_tokens(selector),
                        declarations: declarations.clone(),
                        specificity: compiled.specificity.packed(),
                    });
                }
            }
        }
        offset = end.saturating_add(1);
    }
}
pub(crate) fn stylesheet_media_applies(source: &str, width: f32) -> bool {
    source.trim().is_empty()
        || (source.len() <= 4096
            && !source.contains(['{', '}', ';'])
            && media_allows(source, width))
}

fn media_allows(source: &str, width: f32) -> bool {
    let source = source.replace('\0', " ");
    split_top(&source, b',').into_iter().any(|query| {
        let q = query.trim().to_ascii_lowercase();
        let (q, negate) = q
            .strip_prefix("not ")
            .map_or((q.as_str(), false), |v| (v, true));
        let q = q.strip_prefix("only ").unwrap_or(q).trim();
        let mut allows = true;
        let mut offset = 0;
        if !q.starts_with('(') {
            let end = q
                .find(|c: char| c.is_ascii_whitespace() || c == '(')
                .unwrap_or(q.len());
            allows = matches!(&q[..end], "screen" | "all");
            offset = end;
        }
        let mut conditions = 0;
        while let Some(open) = q[offset..].find('(').map(|n| n + offset) {
            let between = q[offset..open].trim();
            if !between.is_empty() && between != "and" {
                allows = false;
            }
            let Some(end) = matching_close(q, open, b'(', b')') else {
                return false;
            };
            let expr = q[open + 1..end].trim();
            let valid = if let Some((name, value)) = expr.split_once(':') {
                let n = length(value)
                    .filter(|l| !matches!(l, Length::Vh(_)))
                    .and_then(|l| l.resolve(width, 16.));
                match name.trim() {
                    "min-width" => n.is_some_and(|n| width >= n),
                    "max-width" => n.is_some_and(|n| width <= n),
                    "width" => n.is_some_and(|n| (width - n).abs() < 0.01),
                    "prefers-color-scheme" => value.trim() == "light",
                    "prefers-reduced-motion" => value.trim() == "no-preference",
                    _ => false,
                }
            } else {
                false
            };
            allows &= valid;
            conditions += 1;
            offset = end + 1;
        }
        if !q[offset..].trim().is_empty() {
            allows = false;
        }
        if q.is_empty() || (conditions == 0 && q.starts_with('(')) {
            allows = false;
        }
        allows != negate
    })
}
fn first_top_delimiter(source: &str, delimiters: &[u8]) -> Option<(usize, u8)> {
    let (mut round, mut square) = (0u32, 0u32);
    for (i, token) in structural_bytes(source) {
        let CssByte::Unquoted(b) = token else {
            continue;
        };
        if round == 0 && square == 0 && delimiters.contains(&b) {
            return Some((i, b));
        }
        match b {
            b'(' => round += 1,
            b')' => round = round.saturating_sub(1),
            b'[' => square += 1,
            b']' => square = square.saturating_sub(1),
            _ => {}
        }
    }
    None
}
pub fn declarations(source: &str) -> Vec<Declaration> {
    let clean = without_comments(source);
    clean_declarations(&clean)
}
fn clean_declarations(source: &str) -> Vec<Declaration> {
    let mut start = 0;
    let mut bad_string = false;
    component_boundaries(source, b';')
        .chain(std::iter::once((source.len(), CssByte::Unquoted(b';'))))
        .filter_map(|(end, token)| {
            if matches!(token, CssByte::BadString) {
                // Discard the declaration at its boundary, not the rest of the list.
                bad_string = true;
                return None;
            }
            let s = source[start..end].trim();
            start = end + 1;
            if std::mem::take(&mut bad_string) {
                return None;
            }
            let colon = separators(s, b':').into_iter().next()?;
            let raw_name = trim_boundary(&s[..colon]);
            if raw_name.is_empty()
                || !raw_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') || b >= 128)
            {
                return None;
            }
            let name = if raw_name.starts_with("--") {
                raw_name.into()
            } else {
                raw_name.to_ascii_lowercase()
            };
            let raw_value = trim_boundary(&s[colon + 1..]);
            let bang = separators(raw_value, b'!').into_iter().last();
            let important = bang.is_some_and(|i| {
                trim_boundary(&raw_value[i + 1..]).eq_ignore_ascii_case("important")
            });
            let value = if important {
                trim_boundary(&raw_value[..bang.unwrap()])
            } else {
                raw_value
            };
            if value.is_empty() && !raw_name.starts_with("--") {
                return None;
            }
            Some(Declaration {
                name,
                value: serialized_tokens(value),
                important,
            })
        })
        .take(4096)
        .collect()
}

/// Validate one property/value pair against the implemented CSS subset. Useful
/// for CSSOM setters: invalid supported values must preserve the previous value.
/// Declaration-list delimiters and the priority argument belong to the caller.
pub fn supports_declaration(name: &str, value: &str) -> bool {
    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') || b >= 128)
    {
        return false;
    }
    let clean = without_comments(value);
    if !valid_components(&clean, 0) {
        return false;
    }
    if name.starts_with("--") {
        return name.len() > 2 && clean.len() <= 16384;
    }
    let name = name.to_ascii_lowercase();
    !parse_assignment(&name, value).is_empty()
        || svg_property(&name)
            && clean.len() <= 16384
            && (contains_variable(&clean)
                || crate::svg::SvgStyle::default().parse_property(&name, &clean.replace('\0', " ")))
}
fn valid_components(source: &str, depth: usize) -> bool {
    if depth > 32 || source.len() > 65536 {
        return false;
    }
    let (mut quote, mut escaped) = (0, false);
    let mut stack = vec![];
    for &b in source.as_bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if quote != 0 {
            if b == quote {
                quote = 0;
            } else if matches!(b, b'\n' | b'\r') {
                return false;
            }
            continue;
        }
        if matches!(b, b'\'' | b'"') {
            quote = b;
            continue;
        }
        match b {
            b'(' | b'[' | b'{' => {
                stack.push(b);
                if stack.len() > 64 {
                    return false;
                }
            }
            b')' | b']' | b'}' => {
                let expected = match b {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if stack.pop() != Some(expected) {
                    return false;
                }
            }
            b';' | b'!' if stack.is_empty() => return false,
            _ => {}
        }
    }
    if quote != 0 || escaped || !stack.is_empty() {
        return false;
    }
    let mut offset = 0;
    while let Some(start) = variable_offset(source, offset) {
        let Some(end) = matching_close(source, start + 3, b'(', b')') else {
            return false;
        };
        let args = &source[start + 4..end];
        let comma = separators(args, b',').into_iter().next();
        let name = trim_boundary(&args[..comma.unwrap_or(args.len())]);
        let mut n = 0;
        if !name.starts_with("--")
            || name.len() < 3
            || identifier(name, &mut n).is_none()
            || n != name.len()
        {
            return false;
        }
        if let Some(comma) = comma
            && !valid_components(&args[comma + 1..], depth + 1)
        {
            return false;
        }
        offset = end + 1;
    }
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Specificity(u16, u16, u16);
impl Specificity {
    fn add(&mut self, other: Self) {
        self.0 = self.0.saturating_add(other.0);
        self.1 = self.1.saturating_add(other.1);
        self.2 = self.2.saturating_add(other.2);
    }
    fn packed(self) -> u32 {
        u32::from(self.0).min(1023) << 20
            | u32::from(self.1).min(1023) << 10
            | u32::from(self.2).min(1023)
    }
}
#[derive(Clone, Copy, Debug)]
enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}
#[derive(Clone, Debug, Default)]
struct Compound {
    tag: Option<String>,
    tests: Vec<Test>,
}
#[derive(Clone, Debug)]
enum Test {
    Id(String),
    Class(String),
    Attr {
        name: String,
        op: AttrOp,
        value: String,
        insensitive: bool,
    },
    Root,
    Empty,
    Link,
    Checked,
    Disabled(bool),
    Nth {
        a: i32,
        b: i32,
        from_end: bool,
        of_type: bool,
    },
    OnlyChild(bool),
    Any(Vec<Selector>),
    Not(Vec<Selector>),
}
#[derive(Clone, Copy, Debug)]
enum AttrOp {
    Exists,
    Equal,
    Word,
    Dash,
    Prefix,
    Suffix,
    Contains,
}
#[derive(Clone, Debug)]
struct Selector {
    parts: Vec<Compound>,
    combinators: Vec<Combinator>,
    specificity: Specificity,
}
fn css_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-') || b >= 128
}

/// Consume CSS name code points and escapes without allocating the decoded name.
fn consume_name(source: &str, offset: &mut usize, mut push: impl FnMut(char)) -> Option<()> {
    let bytes = source.as_bytes();
    while let Some(&b) = bytes.get(*offset) {
        if b == b'\\' {
            let start = *offset + 1;
            let mut end = start;
            while end - start < 6 && bytes.get(end).is_some_and(u8::is_ascii_hexdigit) {
                end += 1;
            }
            if end > start {
                let code = u32::from_str_radix(&source[start..end], 16).ok()?;
                push(
                    char::from_u32(code)
                        .filter(|c| *c != '\0')
                        .unwrap_or('\u{fffd}'),
                );
                // CSS preprocessing treats CRLF as one newline.
                end += match bytes.get(end) {
                    Some(b'\r') if bytes.get(end + 1) == Some(&b'\n') => 2,
                    Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') => 1,
                    _ => 0,
                };
            } else {
                let c = source[start..].chars().next()?;
                if matches!(c, '\n' | '\r' | '\x0c') {
                    return None;
                }
                push(c);
                end += c.len_utf8();
            }
            *offset = end;
        } else if css_name_byte(b) {
            let c = if b.is_ascii() {
                b as char
            } else {
                source[*offset..].chars().next()?
            };
            push(c);
            *offset += c.len_utf8();
        } else {
            break;
        }
    }
    Some(())
}

fn identifier(source: &str, offset: &mut usize) -> Option<String> {
    let mut out = String::new();
    while source.as_bytes().get(*offset) == Some(&0) {
        *offset += 1;
    }
    consume_name(source, offset, |c| out.push(c))?;
    if out.is_empty() { None } else { Some(out) }
}
fn compile_selector(source: &str, depth: usize) -> Option<Selector> {
    if source.is_empty() || source.len() > 1024 || depth > 16 {
        return None;
    }
    let mut parts = vec![];
    let mut combinators = vec![];
    let mut specificity = Specificity::default();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while bytes
            .get(i)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == 0)
        {
            i += 1;
        }
        let mut part = Compound::default();
        let start = i;
        if bytes.get(i) == Some(&b'*') {
            i += 1;
        } else if bytes.get(i).is_some_and(|b| {
            b.is_ascii_alphabetic() || matches!(b, b'_' | b'-' | b'\\') || *b >= 128
        }) {
            part.tag = Some(identifier(source, &mut i)?.to_ascii_lowercase());
            specificity.add(Specificity(0, 0, 1));
        }
        loop {
            while bytes.get(i) == Some(&0) {
                i += 1;
            }
            let Some(&kind) = bytes.get(i) else {
                break;
            };
            if !matches!(kind, b'.' | b'#' | b'[' | b':') {
                break;
            }
            i += 1;
            let test = match kind {
                b'.' => {
                    specificity.add(Specificity(0, 1, 0));
                    Test::Class(identifier(source, &mut i)?)
                }
                b'#' => {
                    // '#'+comment+ident is a delimiter followed by an ident,
                    // not the hash token required by an ID selector.
                    if bytes.get(i) == Some(&0) {
                        return None;
                    }
                    specificity.add(Specificity(1, 0, 0));
                    Test::Id(identifier(source, &mut i)?)
                }
                b'[' => {
                    let end = matching_close(source, i - 1, b'[', b']')?;
                    let test = attribute_test(&source[i..end])?;
                    i = end + 1;
                    specificity.add(Specificity(0, 1, 0));
                    test
                }
                b':' => {
                    let name = identifier(source, &mut i)?.to_ascii_lowercase();
                    let argument = if bytes.get(i) == Some(&b'(') {
                        let end = matching_close(source, i, b'(', b')')?;
                        let arg = &source[i + 1..end];
                        i = end + 1;
                        Some(arg)
                    } else {
                        None
                    };
                    if matches!(name.as_str(), "is" | "not" | "where") {
                        let arguments = split_top(argument?, b',');
                        let args: Vec<_> = if name == "not" {
                            arguments
                                .into_iter()
                                .map(|s| compile_selector(s, depth + 1))
                                .collect::<Option<Vec<_>>>()?
                        } else {
                            arguments
                                .into_iter()
                                .filter_map(|s| compile_selector(s, depth + 1))
                                .collect()
                        };
                        // An empty forgiving list is valid but matches nothing.
                        // :not() has already failed if any member could not parse.
                        if !args.is_empty() && name != "where" {
                            specificity.add(args.iter().map(|s| s.specificity).max()?);
                        }
                        if name == "not" {
                            Test::Not(args)
                        } else {
                            Test::Any(args)
                        }
                    } else {
                        specificity.add(Specificity(0, 1, 0));
                        match (name.as_str(), argument) {
                            ("root", None) => Test::Root,
                            ("empty", None) => Test::Empty,
                            ("link" | "any-link", None) => Test::Link,
                            ("checked", None) => Test::Checked,
                            ("disabled", None) => Test::Disabled(true),
                            ("enabled", None) => Test::Disabled(false),
                            ("only-child", None) => Test::OnlyChild(false),
                            ("only-of-type", None) => Test::OnlyChild(true),
                            (
                                "first-child" | "last-child" | "first-of-type" | "last-of-type",
                                None,
                            ) => Test::Nth {
                                a: 0,
                                b: 1,
                                from_end: name.starts_with("last"),
                                of_type: name.ends_with("of-type"),
                            },
                            (
                                "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type",
                                Some(arg),
                            ) => {
                                let (a, b) = nth_expression(arg)?;
                                Test::Nth {
                                    a,
                                    b,
                                    from_end: name.contains("last"),
                                    of_type: name.ends_with("of-type"),
                                }
                            }
                            _ => return None,
                        }
                    }
                }
                _ => return None,
            };
            part.tests.push(test);
        }
        if i == start || parts.len() >= 128 {
            return None;
        }
        parts.push(part);
        let mut whitespace = false;
        while bytes
            .get(i)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == 0)
        {
            whitespace |= bytes[i].is_ascii_whitespace();
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        let combinator = match bytes[i] {
            b'>' => Combinator::Child,
            b'+' => Combinator::Adjacent,
            b'~' => Combinator::Sibling,
            _ if whitespace => {
                combinators.push(Combinator::Descendant);
                continue;
            }
            _ => return None,
        };
        combinators.push(combinator);
        i += 1;
        while bytes
            .get(i)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == 0)
        {
            i += 1;
        }
        if i == bytes.len() {
            return None;
        }
    }
    if parts.is_empty() || parts.len() != combinators.len() + 1 {
        return None;
    }
    Some(Selector {
        parts,
        combinators,
        specificity,
    })
}
fn attribute_test(source: &str) -> Option<Test> {
    let mut i = 0;
    let source = trim_boundary(source);
    let name = identifier(source, &mut i)?.to_ascii_lowercase();
    let rest = trim_boundary(&source[i..]);
    if rest.is_empty() {
        return Some(Test::Attr {
            name,
            op: AttrOp::Exists,
            value: String::new(),
            insensitive: false,
        });
    }
    let (op, count) = if rest.starts_with("~=") {
        (AttrOp::Word, 2)
    } else if rest.starts_with("|=") {
        (AttrOp::Dash, 2)
    } else if rest.starts_with("^=") {
        (AttrOp::Prefix, 2)
    } else if rest.starts_with("$=") {
        (AttrOp::Suffix, 2)
    } else if rest.starts_with("*=") {
        (AttrOp::Contains, 2)
    } else if rest.starts_with('=') {
        (AttrOp::Equal, 1)
    } else {
        return None;
    };
    let rest = trim_boundary(&rest[count..]);
    let mut offset = 0;
    let value = if rest.starts_with(['\'', '"']) {
        let quote = rest.as_bytes()[0];
        let mut end = 1;
        let mut escaped = false;
        while end < rest.len() {
            let b = rest.as_bytes()[end];
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == quote {
                break;
            }
            end += 1;
        }
        if end == rest.len() {
            return None;
        }
        offset = end + 1;
        unescape_string(&rest[1..end])?
    } else {
        identifier(rest, &mut offset)?
    };
    let flag = trim_boundary(&rest[offset..]);
    if !flag.is_empty() && !flag.eq_ignore_ascii_case("i") && !flag.eq_ignore_ascii_case("s") {
        return None;
    }
    Some(Test::Attr {
        name,
        op,
        value,
        insensitive: flag.eq_ignore_ascii_case("i"),
    })
}
fn unescape_string(source: &str) -> Option<String> {
    let mut result = String::new();
    let mut i = 0;
    while i < source.len() {
        if source.as_bytes()[i] == b'\\' {
            let start = i;
            i += 1;
            let mut escaped = String::new();
            // Reuse CSS identifier escape decoding for one escape only.
            let mut end = i;
            while source
                .as_bytes()
                .get(end)
                .is_some_and(u8::is_ascii_hexdigit)
                && end - i < 6
            {
                end += 1;
            }
            if end > i {
                let code = u32::from_str_radix(&source[i..end], 16).ok()?;
                escaped.push(
                    char::from_u32(code)
                        .filter(|c| *c != '\0')
                        .unwrap_or('\u{fffd}'),
                );
                i = end;
                if source
                    .as_bytes()
                    .get(i)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    i += 1;
                }
            } else {
                let c = source[i..].chars().next()?;
                if c != '\n' && c != '\r' {
                    escaped.push(c);
                }
                i += c.len_utf8();
            }
            debug_assert!(i > start);
            result.push_str(&escaped);
        } else {
            let c = source[i..].chars().next()?;
            result.push(c);
            i += c.len_utf8();
        }
    }
    Some(result)
}
fn nth_expression(source: &str) -> Option<(i32, i32)> {
    // CSS Syntax §6.2 distinguishes an integer dimension (2n), an ident
    // (n-3), and a sign followed by an unsigned integer (n + 3). Preserve
    // token boundaries: whitespace/comments cannot make `2 n` a dimension
    // or supply the missing sign in `n 3`.
    let source = trim_boundary(source);
    let sign_len = usize::from(source.starts_with(['+', '-']));
    let number_end = sign_len
        + source.as_bytes()[sign_len..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
    let (mut a, explicit_plus, ident_source) = if number_end > sign_len {
        let number = source[..number_end].parse().ok()?;
        let rest = &source[number_end..];
        if trim_boundary(rest).is_empty() {
            return Some((0, number));
        }
        if rest.starts_with('\0') {
            return None;
        }
        (number, false, rest)
    } else if let Some(rest) = source.strip_prefix('+') {
        // A '+' delimiter can precede n, but not with a whitespace token.
        (1, true, rest.trim_start_matches('\0'))
    } else {
        (1, false, source)
    };
    let mut offset = 0;
    let ident = identifier(ident_source, &mut offset)?.to_ascii_lowercase();
    let rest = trim_boundary(&ident_source[offset..]);
    let ident_only = number_end == sign_len && !explicit_plus;
    if ident_only && rest.is_empty() {
        match ident.as_str() {
            "odd" => return Some((2, 1)),
            "even" => return Some((2, 0)),
            _ => {}
        }
    }
    let n = if ident_only && let Some(n) = ident.strip_prefix('-') {
        a = -1;
        n
    } else {
        &ident
    };
    let suffix = n.strip_prefix('n')?;
    let b = if let Some(digits) = suffix.strip_prefix('-') {
        if digits.is_empty() {
            nth_offset(rest, true)?
        } else if rest.is_empty() {
            nth_offset(digits, true)?
        } else {
            return None;
        }
    } else if !suffix.is_empty() {
        return None;
    } else if rest.is_empty() {
        0
    } else {
        let negative = rest.starts_with('-');
        nth_offset(trim_boundary(rest.strip_prefix(['+', '-'])?), negative)?
    };
    Some((a, b))
}

fn nth_offset(digits: &str, negative: bool) -> Option<i32> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let magnitude: i64 = digits.parse().ok()?;
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}
pub fn matches_selector(doc: &Document, id: NodeId, source: &str) -> bool {
    let clean = without_comments(source);
    split_top(&clean, b',')
        .into_iter()
        .any(|s| compile_selector(s, 0).is_some_and(|s| s.matches(doc, id)))
}

pub(crate) struct DomSelectorList(Vec<Selector>);

impl DomSelectorList {
    pub(crate) fn parse(source: &str) -> Option<Self> {
        let clean = without_comments(source);
        // DOM selector APIs reject the entire list if any member fails to
        // parse. Do not test candidates before the last member is validated.
        split_top(&clean, b',')
            .into_iter()
            .map(|s| compile_selector(s, 0))
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }

    pub(crate) fn matches(&self, doc: &Document, id: NodeId) -> bool {
        self.0.iter().any(|selector| selector.matches(doc, id))
    }
}

impl Selector {
    fn matches(&self, doc: &Document, id: NodeId) -> bool {
        self.matches_part(doc, id, self.parts.len() - 1, &mut 16384)
    }
    fn matches_part(&self, doc: &Document, id: NodeId, index: usize, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        if !self.parts[index].matches(doc, id, budget) {
            return false;
        }
        if index == 0 {
            return true;
        }
        match self.combinators[index - 1] {
            Combinator::Child => doc.nodes[id.0]
                .parent
                .is_some_and(|p| self.matches_part(doc, p, index - 1, budget)),
            Combinator::Descendant => {
                let mut parent = doc.nodes[id.0].parent;
                while let Some(p) = parent {
                    if self.matches_part(doc, p, index - 1, budget) {
                        return true;
                    }
                    parent = doc.nodes[p.0].parent;
                    if *budget == 0 {
                        return false;
                    }
                }
                false
            }
            Combinator::Adjacent | Combinator::Sibling => {
                let Some(parent) = doc.nodes[id.0].parent else {
                    return false;
                };
                let children = &doc.nodes[parent.0].children;
                let Some(pos) = children.iter().position(|n| *n == id) else {
                    return false;
                };
                for &sibling in children[..pos].iter().rev() {
                    if doc.tag(sibling).is_empty() {
                        continue;
                    }
                    if self.matches_part(doc, sibling, index - 1, budget) {
                        return true;
                    }
                    if matches!(self.combinators[index - 1], Combinator::Adjacent) {
                        break;
                    }
                    if *budget == 0 {
                        return false;
                    }
                }
                false
            }
        }
    }
}
impl Compound {
    fn matches(&self, doc: &Document, id: NodeId, budget: &mut usize) -> bool {
        let tag = doc.tag(id);
        if tag.is_empty() || self.tag.as_ref().is_some_and(|t| t != tag) {
            return false;
        }
        self.tests.iter().all(|test| match test {
            Test::Id(v) => doc.attr(id, "id") == Some(v.as_str()),
            Test::Class(v) => doc
                .attr(id, "class")
                .is_some_and(|a| a.split_ascii_whitespace().any(|c| c == v)),
            Test::Attr {
                name,
                op,
                value,
                insensitive,
            } => doc
                .attr(id, name)
                .is_some_and(|actual| attribute_matches(actual, value, *op, *insensitive)),
            Test::Root => doc.nodes[id.0].parent.is_some_and(|p| p == doc.root),
            Test::Empty => doc.nodes[id.0]
                .children
                .iter()
                .all(|&n| match &doc.nodes[n.0].kind {
                    crate::dom::NodeKind::Element { .. } => false,
                    crate::dom::NodeKind::Text(s) => s.is_empty(),
                    _ => true,
                }),
            Test::Link => matches!(tag, "a" | "area") && doc.attr(id, "href").is_some(),
            Test::Checked => {
                matches!(tag, "input" | "option")
                    && (doc.attr(id, "checked").is_some() || doc.attr(id, "selected").is_some())
            }
            Test::Disabled(disabled) => {
                matches!(
                    tag,
                    "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
                ) && doc.attr(id, "disabled").is_some() == *disabled
            }
            Test::Nth {
                a,
                b,
                from_end,
                of_type,
            } => {
                let Some(parent) = doc.nodes[id.0].parent else {
                    return false;
                };
                let siblings = &doc.nodes[parent.0].children;
                let same_type = of_type.then(|| (tag, doc.namespace(id)));
                let mut position = 0i64;
                let mut check = |&node: &NodeId| {
                    if nth_sibling_matches(doc, node, same_type) {
                        position += 1;
                    }
                    node == id
                };
                if *from_end {
                    for n in siblings.iter().rev() {
                        if check(n) {
                            break;
                        }
                    }
                } else {
                    for n in siblings {
                        if check(n) {
                            break;
                        }
                    }
                }
                let delta = position - i64::from(*b);
                let a = i64::from(*a);
                if a == 0 {
                    delta == 0
                } else {
                    delta % a == 0 && delta / a >= 0
                }
            }
            Test::OnlyChild(of_type) => doc.nodes[id.0].parent.is_some_and(|p| {
                let same_type = of_type.then(|| (tag, doc.namespace(id)));
                doc.nodes[p.0]
                    .children
                    .iter()
                    .filter(|&&n| nth_sibling_matches(doc, n, same_type))
                    .count()
                    == 1
            }),
            Test::Any(selectors) => selectors
                .iter()
                .any(|s| s.matches_part(doc, id, s.parts.len() - 1, budget)),
            Test::Not(selectors) => {
                let matches = selectors
                    .iter()
                    .any(|s| s.matches_part(doc, id, s.parts.len() - 1, budget));
                *budget > 0 && !matches
            }
        })
    }
}

fn nth_sibling_matches(
    doc: &Document,
    sibling: NodeId,
    same_type: Option<(&str, Namespace)>,
) -> bool {
    let tag = doc.tag(sibling);
    // An expanded element type is its local name and its namespace, not
    // just the local name (Selectors §14.4).
    !tag.is_empty()
        && same_type
            .is_none_or(|(local, namespace)| tag == local && doc.namespace(sibling) == namespace)
}

fn attribute_matches(actual: &str, value: &str, op: AttrOp, insensitive: bool) -> bool {
    let eq = |a: &str, b: &str| {
        if insensitive {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    match op {
        AttrOp::Exists => true,
        AttrOp::Equal => eq(actual, value),
        AttrOp::Word => !value.is_empty() && actual.split_ascii_whitespace().any(|a| eq(a, value)),
        AttrOp::Dash => {
            eq(actual, value)
                || actual.get(..value.len()).is_some_and(|a| eq(a, value))
                    && actual.as_bytes().get(value.len()) == Some(&b'-')
        }
        AttrOp::Prefix => {
            !value.is_empty() && actual.get(..value.len()).is_some_and(|a| eq(a, value))
        }
        AttrOp::Suffix => {
            !value.is_empty()
                && actual
                    .len()
                    .checked_sub(value.len())
                    .and_then(|i| actual.get(i..))
                    .is_some_and(|a| eq(a, value))
        }
        AttrOp::Contains => {
            !value.is_empty()
                && actual.as_bytes().windows(value.len()).any(|a| {
                    if insensitive {
                        a.eq_ignore_ascii_case(value.as_bytes())
                    } else {
                        a == value.as_bytes()
                    }
                })
        }
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Property {
    FontSize,
    Color,
    Display,
    Float,
    Clear,
    Background,
    Bold,
    Italic,
    Mono,
    Underline,
    WhiteSpace,
    TextAlign,
    LineHeight,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    BorderTopWidth,
    BorderRightWidth,
    BorderBottomWidth,
    BorderLeftWidth,
    BorderTopStyle,
    BorderRightStyle,
    BorderBottomStyle,
    BorderLeftStyle,
    BorderTopColor,
    BorderRightColor,
    BorderBottomColor,
    BorderLeftColor,
    Width,
    Height,
    MinWidth,
    MaxWidth,
    MinHeight,
    MaxHeight,
    BoxSizing,
    FlexDirection,
    FlexWrap,
    FlexGrow,
    FlexShrink,
    FlexBasis,
    JustifyContent,
    AlignItems,
    AlignSelf,
    RowGap,
    ColumnGap,
    Order,
    BreakWords,
    GridTemplateColumns,
    GridTemplateRows,
    GridTemplateAreas,
    GridAutoColumns,
    GridAutoRows,
    GridColumnStart,
    GridColumnEnd,
    GridRowStart,
    GridRowEnd,
    GridAutoFlow,
    JustifyItems,
    JustifySelf,
    AlignContent,
    Position,
    Top,
    Right,
    Bottom,
    Left,
    ZIndex,
    OverflowX,
    OverflowY,
    Transform,
    TransformOrigin,
    Direction,
    MarginInlineStart,
    MarginInlineEnd,
    PaddingInlineStart,
    PaddingInlineEnd,
    InsetInlineStart,
    InsetInlineEnd,
}
// Direction is resolved separately, then logical inline edges join this cascade.
const PROPERTIES: [Property; Property::TransformOrigin as usize + 1] = [
    Property::FontSize,
    Property::Color,
    Property::Display,
    Property::Float,
    Property::Clear,
    Property::Background,
    Property::Bold,
    Property::Italic,
    Property::Mono,
    Property::Underline,
    Property::WhiteSpace,
    Property::TextAlign,
    Property::LineHeight,
    Property::MarginTop,
    Property::MarginRight,
    Property::MarginBottom,
    Property::MarginLeft,
    Property::PaddingTop,
    Property::PaddingRight,
    Property::PaddingBottom,
    Property::PaddingLeft,
    Property::BorderTopWidth,
    Property::BorderRightWidth,
    Property::BorderBottomWidth,
    Property::BorderLeftWidth,
    Property::BorderTopStyle,
    Property::BorderRightStyle,
    Property::BorderBottomStyle,
    Property::BorderLeftStyle,
    Property::BorderTopColor,
    Property::BorderRightColor,
    Property::BorderBottomColor,
    Property::BorderLeftColor,
    Property::Width,
    Property::Height,
    Property::MinWidth,
    Property::MaxWidth,
    Property::MinHeight,
    Property::MaxHeight,
    Property::BoxSizing,
    Property::FlexDirection,
    Property::FlexWrap,
    Property::FlexGrow,
    Property::FlexShrink,
    Property::FlexBasis,
    Property::JustifyContent,
    Property::AlignItems,
    Property::AlignSelf,
    Property::RowGap,
    Property::ColumnGap,
    Property::Order,
    Property::BreakWords,
    Property::GridTemplateColumns,
    Property::GridTemplateRows,
    Property::GridTemplateAreas,
    Property::GridAutoColumns,
    Property::GridAutoRows,
    Property::GridColumnStart,
    Property::GridColumnEnd,
    Property::GridRowStart,
    Property::GridRowEnd,
    Property::GridAutoFlow,
    Property::JustifyItems,
    Property::JustifySelf,
    Property::AlignContent,
    Property::Position,
    Property::Top,
    Property::Right,
    Property::Bottom,
    Property::Left,
    Property::ZIndex,
    Property::OverflowX,
    Property::OverflowY,
    Property::Transform,
    Property::TransformOrigin,
];
const PROPERTY_COUNT: usize = Property::InsetInlineEnd as usize + 1;
const LOGICAL_INLINE_EDGES: [Property; 6] = [
    Property::MarginInlineStart,
    Property::MarginInlineEnd,
    Property::PaddingInlineStart,
    Property::PaddingInlineEnd,
    Property::InsetInlineStart,
    Property::InsetInlineEnd,
];
pub(crate) const LOGICAL_BOX_LONGHANDS: &[(&str, &str)] = &[
    ("direction", "direction"),
    ("inlineSize", "inline-size"),
    ("blockSize", "block-size"),
    ("minInlineSize", "min-inline-size"),
    ("minBlockSize", "min-block-size"),
    ("maxInlineSize", "max-inline-size"),
    ("maxBlockSize", "max-block-size"),
    ("marginBlockStart", "margin-block-start"),
    ("marginBlockEnd", "margin-block-end"),
    ("marginInlineStart", "margin-inline-start"),
    ("marginInlineEnd", "margin-inline-end"),
    ("paddingBlockStart", "padding-block-start"),
    ("paddingBlockEnd", "padding-block-end"),
    ("paddingInlineStart", "padding-inline-start"),
    ("paddingInlineEnd", "padding-inline-end"),
    ("insetBlockStart", "inset-block-start"),
    ("insetBlockEnd", "inset-block-end"),
    ("insetInlineStart", "inset-inline-start"),
    ("insetInlineEnd", "inset-inline-end"),
];
pub(crate) const LOGICAL_BOX_SHORTHANDS: &[(&str, &str)] = &[
    ("marginBlock", "margin-block"),
    ("marginInline", "margin-inline"),
    ("paddingBlock", "padding-block"),
    ("paddingInline", "padding-inline"),
    ("insetBlock", "inset-block"),
    ("insetInline", "inset-inline"),
];

impl Property {
    fn physical(self, rtl: bool) -> Self {
        use Property::*;
        match (self, rtl) {
            (MarginInlineStart, false) | (MarginInlineEnd, true) => MarginLeft,
            (MarginInlineStart, true) | (MarginInlineEnd, false) => MarginRight,
            (PaddingInlineStart, false) | (PaddingInlineEnd, true) => PaddingLeft,
            (PaddingInlineStart, true) | (PaddingInlineEnd, false) => PaddingRight,
            (InsetInlineStart, false) | (InsetInlineEnd, true) => Left,
            (InsetInlineStart, true) | (InsetInlineEnd, false) => Right,
            _ => self,
        }
    }
}
#[derive(Clone, Debug)]
enum Value {
    Initial,
    Inherit,
    Unset,
    Color(Option<Color>),
    Length(SpecifiedLength),
    Number(f32),
    Bool(bool),
    FontWeight(u16),
    RelativeWeight(bool),
    FontStyle(FontStyle),
    Families(Arc<[FontFamily]>),
    Display(Display),
    Float(Float),
    Clear(Clear),
    Position(Position),
    Overflow(Overflow),
    ZIndex(Option<i32>),
    Transforms(Arc<Vec<TransformOp<SpecifiedLength>>>),
    Origin([SpecifiedLength; 2]),
    Sizing(BoxSizing),
    Direction(FlexDirection),
    Align(Align),
    Text(String),
    WhiteSpace { pre: bool, wrap: bool },
    Deferred { name: Arc<str>, source: Arc<str> },
    Tracks(grid::SpecifiedTracks),
    GridTemplate(grid::SpecifiedTemplate),
    GridAreas(Arc<GridAreas>),
    GridLine(GridLine),
    GridFlow { axis: GridAutoFlow, dense: bool },
}
#[derive(Clone, Debug)]
struct CompiledDeclaration {
    property: Property,
    value: Value,
    important: bool,
    order: usize,
}
struct CompiledRule {
    selector: Selector,
    declarations: Vec<CompiledDeclaration>,
    custom: Vec<Declaration>,
    svg: Vec<SvgDeclaration>,
}
struct SvgDeclaration {
    name: Arc<str>,
    value: Arc<str>,
    important: bool,
    order: usize,
}
fn svg_property(name: &str) -> bool {
    matches!(
        name,
        "fill"
            | "stroke"
            | "color"
            | "opacity"
            | "fill-opacity"
            | "stroke-opacity"
            | "fill-rule"
            | "clip-rule"
            | "stroke-width"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-miterlimit"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "display"
            | "visibility"
            | "clip-path"
            | "stop-color"
            | "stop-opacity"
            | "x"
            | "y"
            | "width"
            | "height"
            | "cx"
            | "cy"
            | "r"
            | "rx"
            | "ry"
    )
}
fn svg_declarations(declarations: &[Declaration]) -> Vec<SvgDeclaration> {
    declarations
        .iter()
        .enumerate()
        .filter_map(|(order, d)| {
            if !svg_property(&d.name) || d.value.len() > 16384 {
                return None;
            }
            let value = without_comments(&d.value).replace('\0', " ");
            if !contains_variable(&value)
                && !crate::svg::SvgStyle::default().parse_property(&d.name, &value)
            {
                return None;
            }
            Some(SvgDeclaration {
                name: Arc::from(d.name.as_str()),
                value: Arc::from(value),
                important: d.important,
                order,
            })
        })
        .collect()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Priority {
    important: bool,
    inline: bool,
    specificity: Specificity,
    rule: usize,
    declaration: usize,
}
#[derive(Clone, Copy)]
struct Winner<'a> {
    declaration: &'a CompiledDeclaration,
    priority: Priority,
}
impl<'a> Winner<'a> {
    fn value(self, custom: &HashMap<Arc<str>, Arc<str>>) -> Cow<'a, Value> {
        let Value::Deferred { name, source } = &self.declaration.value else {
            return Cow::Borrowed(&self.declaration.value);
        };
        let resolved = resolve_variables(source, custom, 0)
            .and_then(|source| {
                parse_assignment(name, &source)
                    .into_iter()
                    .find(|(property, _)| *property == self.declaration.property)
                    .map(|(_, value)| value)
            })
            .unwrap_or(Value::Unset);
        Cow::Owned(match resolved {
            Value::Deferred { .. } => Value::Unset,
            value => value,
        })
    }
}

/// Compiled author stylesheet with one rightmost-selector bucket per rule.
/// Id/class/tag lookups skip unrelated rules without allocating per-node strings.
pub struct Stylesheet {
    rules: Vec<CompiledRule>,
    ids: HashMap<String, Vec<usize>>,
    classes: HashMap<String, Vec<usize>>,
    tags: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    viewport: [f32; 2],
}
impl Stylesheet {
    pub fn new(rules: Vec<Rule>, viewport: f32) -> Self {
        Self::with_viewport(rules, viewport, 600.)
    }

    pub fn with_viewport(rules: Vec<Rule>, width: f32, height: f32) -> Self {
        let mut result = Self {
            rules: vec![],
            ids: HashMap::new(),
            classes: HashMap::new(),
            tags: HashMap::new(),
            universal: vec![],
            viewport: [width, height],
        };
        for rule in rules.into_iter().take(8192) {
            let Some(selector) = compile_selector(&without_comments(&rule.selector), 0) else {
                continue;
            };
            let index = result.rules.len();
            let right = selector.parts.last().unwrap();
            if let Some(id) = right
                .tests
                .iter()
                .find_map(|t| if let Test::Id(v) = t { Some(v) } else { None })
            {
                result.ids.entry(id.clone()).or_default().push(index);
            } else if let Some(class) = right.tests.iter().find_map(|t| {
                if let Test::Class(v) = t {
                    Some(v)
                } else {
                    None
                }
            }) {
                result.classes.entry(class.clone()).or_default().push(index);
            } else if let Some(tag) = &right.tag {
                result.tags.entry(tag.clone()).or_default().push(index);
            } else {
                result.universal.push(index);
            }
            let declarations = compile_declarations(&rule.declarations);
            let svg = svg_declarations(&rule.declarations);
            let custom = rule
                .declarations
                .into_iter()
                .filter(|d| d.name.starts_with("--") && d.value.len() <= 16384)
                .collect();
            result.rules.push(CompiledRule {
                selector,
                declarations,
                custom,
                svg,
            });
        }
        result
    }
    fn candidates(&self, doc: &Document, id: NodeId, mut visit: impl FnMut(usize)) {
        for &i in &self.universal {
            visit(i);
        }
        if let Some(indices) = self.tags.get(doc.tag(id)) {
            for &i in indices {
                visit(i);
            }
        }
        if let Some(indices) = doc.attr(id, "id").and_then(|v| self.ids.get(v)) {
            for &i in indices {
                visit(i);
            }
        }
        if let Some(classes) = doc.attr(id, "class") {
            for class in classes.split_ascii_whitespace() {
                if let Some(indices) = self.classes.get(class) {
                    for &i in indices {
                        visit(i);
                    }
                }
            }
        }
    }
    /// A diagnostic count of candidate visits, useful for bounded synthetic benchmarks.
    pub fn candidate_rule_count(&self, doc: &Document, id: NodeId) -> usize {
        let mut count = 0;
        self.candidates(doc, id, |_| count += 1);
        count
    }
    pub fn compute(&self, doc: &Document, id: NodeId, parent: &Style, root_font: f32) -> Style {
        let mut style = user_agent_style(doc, id, parent);
        let is_root = doc.nodes[id.0].parent == Some(doc.root);
        let is_svg = doc.namespace(id) == crate::dom::Namespace::Svg;
        if is_svg
            && let Some(value) = doc.attr(id, "color")
            && let Some(value) = parse_value(Property::Color, value)
        {
            apply_value(
                &mut style,
                parent,
                Property::Color,
                &value,
                root_font,
                self.viewport,
            );
        }
        let inline = doc.attr(id, "style").map(declarations).unwrap_or_default();
        let inline_svg = if is_svg {
            svg_declarations(&inline)
        } else {
            vec![]
        };
        let mut svg_winners = HashMap::new();
        let inline_declarations = compile_declarations(&inline);
        let mut winners: [Option<Winner<'_>>; PROPERTY_COUNT] = [None; PROPERTY_COUNT];
        let mut custom: HashMap<&str, (&Declaration, Priority)> = HashMap::new();
        // Candidate processing order is irrelevant: source order is in Priority.
        self.candidates(doc, id, |index| {
            let rule = &self.rules[index];
            if !rule.selector.matches(doc, id) {
                return;
            }
            if is_svg {
                choose_svg(
                    &mut svg_winners,
                    &rule.svg,
                    false,
                    rule.selector.specificity,
                    index,
                );
            }
            for declaration in &rule.declarations {
                choose(
                    &mut winners,
                    declaration,
                    false,
                    rule.selector.specificity,
                    index,
                );
            }
            choose_custom(
                &mut custom,
                &rule.custom,
                false,
                rule.selector.specificity,
                index,
            );
        });
        for declaration in &inline_declarations {
            choose(
                &mut winners,
                declaration,
                true,
                Specificity::default(),
                usize::MAX,
            );
        }
        if is_svg {
            choose_svg(
                &mut svg_winners,
                &inline_svg,
                true,
                Specificity::default(),
                usize::MAX,
            );
        }
        choose_custom(
            &mut custom,
            &inline,
            true,
            Specificity::default(),
            usize::MAX,
        );
        if !custom.is_empty() {
            let mut raw = (*parent.custom).clone();
            for (name, (decl, _)) in custom {
                match decl.value.trim() {
                    value if value.eq_ignore_ascii_case("initial") => {
                        raw.remove(name);
                    }
                    value
                        if value.eq_ignore_ascii_case("inherit")
                            || value.eq_ignore_ascii_case("unset") =>
                    {
                        if let Some(v) = parent.custom.get(name) {
                            raw.insert(name.into(), v.clone());
                        } else {
                            raw.remove(name);
                        }
                    }
                    _ => {
                        if raw.len() < 256 || raw.contains_key(name) {
                            raw.insert(name.into(), Arc::from(decl.value.as_str()));
                        }
                    }
                }
            }
            style.custom = Arc::new(compute_custom(raw));
        }
        if let Some(winner) = winners[Property::Direction as usize] {
            style.rtl = match winner.value(&style.custom).as_ref() {
                Value::Bool(rtl) => *rtl,
                Value::Initial => false,
                _ => parent.rtl,
            };
        }
        // Physical declarations and their mapped logical counterparts share one cascade.
        for property in LOGICAL_INLINE_EDGES {
            if let Some(winner) = winners[property as usize] {
                let target = &mut winners[property.physical(style.rtl) as usize];
                if target.is_none_or(|old| winner.priority > old.priority) {
                    *target = Some(winner);
                }
            }
        }
        let initial = Style::default();
        for property in PROPERTIES {
            let Some(winner) = winners[property as usize] else {
                continue;
            };
            match winner.value(&style.custom).as_ref() {
                Value::Initial => copy_property(&mut style, &initial, property, property),
                Value::Inherit => copy_property(
                    &mut style,
                    parent,
                    property,
                    winner.declaration.property.physical(parent.rtl),
                ),
                Value::Unset => copy_property(
                    &mut style,
                    if inherited(property) {
                        parent
                    } else {
                        &initial
                    },
                    property,
                    property,
                ),
                value => {
                    let root_font = if is_root && property != Property::FontSize {
                        style.font_size
                    } else {
                        root_font
                    };
                    apply_value(
                        &mut style,
                        parent,
                        property,
                        value,
                        root_font,
                        self.viewport,
                    );
                }
            }
        }
        if !svg_winners.is_empty() {
            let mut values = HashMap::new();
            let mut budget = 65536;
            for (name, (decl, _)) in svg_winners {
                let value = if contains_variable(&decl.value) {
                    let resolved = resolve_variables(&decl.value, &style.custom, 0)
                        .map(|v| without_comments(&v).replace('\0', " "))
                        .filter(|v| {
                            v.len() <= budget
                                && crate::svg::SvgStyle::default().parse_property(&name, v)
                        });
                    if let Some(value) = resolved {
                        budget -= value.len();
                        Arc::from(value)
                    } else {
                        Arc::from("unset")
                    }
                } else {
                    decl.value.clone()
                };
                values.insert(name, value);
            }
            style.svg_overrides = Arc::new(values);
        }
        if style.position.out_of_flow() {
            style.float = Float::None;
        }
        if style.position.out_of_flow() || style.float != Float::None {
            style.display = match style.display {
                Display::Inline | Display::InlineBlock => Display::Block,
                Display::InlineFlex => Display::Flex,
                Display::InlineGrid => Display::Grid,
                v => v,
            };
        }
        let original = style.overflow;
        for axis in 0..2 {
            if original[axis] == Overflow::Visible && original[1 - axis].scrollable() {
                style.overflow[axis] = Overflow::Auto;
            }
        }
        let borders = style.borders();
        style.border = borders.into_iter().fold(0., f32::max);
        style.border_color = style.border_colors[0].unwrap_or(style.color);
        if doc.namespace(id) == crate::dom::Namespace::Html
            && doc.tag(id) == "input"
            && doc
                .attr(id, "type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden"))
        {
            // HTML's non-rendered hidden input state is a UA-important rule.
            style.display = Display::None;
        }
        style
    }
}
fn choose_svg<'a>(
    winners: &mut HashMap<Arc<str>, (&'a SvgDeclaration, Priority)>,
    declarations: &'a [SvgDeclaration],
    inline: bool,
    specificity: Specificity,
    rule: usize,
) {
    for d in declarations {
        let priority = Priority {
            important: d.important,
            inline,
            specificity,
            rule,
            declaration: d.order,
        };
        if winners.get(&d.name).is_none_or(|(_, p)| priority >= *p) {
            winners.insert(d.name.clone(), (d, priority));
        }
    }
}
fn choose<'a>(
    winners: &mut [Option<Winner<'a>>; PROPERTY_COUNT],
    declaration: &'a CompiledDeclaration,
    inline: bool,
    specificity: Specificity,
    rule: usize,
) {
    let priority = Priority {
        important: declaration.important,
        inline,
        specificity,
        rule,
        declaration: declaration.order,
    };
    let target = &mut winners[declaration.property as usize];
    if target.is_none_or(|w| priority >= w.priority) {
        *target = Some(Winner {
            declaration,
            priority,
        });
    }
}
fn choose_custom<'a>(
    custom: &mut HashMap<&'a str, (&'a Declaration, Priority)>,
    declarations: &'a [Declaration],
    inline: bool,
    specificity: Specificity,
    rule: usize,
) {
    for (order, d) in declarations.iter().enumerate() {
        if !d.name.starts_with("--") || d.value.len() > 16384 {
            continue;
        }
        let priority = Priority {
            important: d.important,
            inline,
            specificity,
            rule,
            declaration: order,
        };
        if custom
            .get(d.name.as_str())
            .is_none_or(|(_, old)| priority >= *old)
        {
            custom.insert(d.name.as_str(), (d, priority));
        }
    }
}
/// Convenience API; callers computing a document should reuse `Stylesheet`.
pub fn compute(doc: &Document, id: NodeId, parent: &Style, rules: &[Rule]) -> Style {
    Stylesheet::new(rules.to_vec(), 0.).compute(doc, id, parent, 16.)
}

fn inherited(property: Property) -> bool {
    matches!(
        property,
        Property::FontSize
            | Property::Color
            | Property::Bold
            | Property::Italic
            | Property::Mono
            | Property::WhiteSpace
            | Property::TextAlign
            | Property::LineHeight
            | Property::BreakWords
    )
}
fn user_agent_style(doc: &Document, id: NodeId, parent: &Style) -> Style {
    let mut style = Style::default();
    for property in PROPERTIES {
        if inherited(property) {
            copy_property(&mut style, parent, property, property);
        }
    }
    style.rtl = doc
        .attr(id, "dir")
        .filter(|_| doc.namespace(id) == crate::dom::Namespace::Html)
        .and_then(|direction| match direction {
            value if value.eq_ignore_ascii_case("rtl") => Some(true),
            value if value.eq_ignore_ascii_case("ltr") => Some(false),
            _ => None,
        })
        .unwrap_or(parent.rtl);
    style.custom = parent.custom.clone();
    // Decoration propagates to descendant inline text in this renderer.
    style.underline = parent.underline;
    let tag = doc.tag(id);
    // HTML widget defaults are overridden by the ordinary author cascade.
    if doc.namespace(id) == crate::dom::Namespace::Html
        && matches!(tag, "input" | "button" | "select" | "textarea")
    {
        style.display = Display::InlineBlock;
        style.font_size = 13.333333;
        style.line_height = 1.2;
        style.line_height_absolute = None;
        style.padding = [Length::Px(2.); 4];
        style.border_width = [1.; 4];
        style.border_visible = [true; 4];
        style.border_color = Color(118, 118, 118, 255);
        style.border_colors = [Some(Color(118, 118, 118, 255)); 4];
        style.background = Some(Color::WHITE);
        style.color = Color(20, 24, 28, 255);
        if tag == "button"
            || tag == "input"
                && doc.attr(id, "type").is_some_and(|kind| {
                    matches!(
                        kind.to_ascii_lowercase().as_str(),
                        "button" | "submit" | "reset"
                    )
                })
        {
            style.background = Some(Color(239, 239, 239, 255));
            style.padding[1] = Length::Px(6.);
            style.padding[3] = Length::Px(6.);
        }
        if tag == "input"
            && doc
                .attr(id, "type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden"))
        {
            style.display = Display::None;
        }
        if tag == "input"
            && doc.attr(id, "type").is_some_and(|kind| {
                matches!(kind.to_ascii_lowercase().as_str(), "checkbox" | "radio")
            })
        {
            style.padding = [Length::Px(0.); 4];
            style.margin = [Length::Px(3.); 4];
        }
    }
    if matches!(
        tag,
        "html"
            | "body"
            | "div"
            | "p"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "ul"
            | "ol"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "blockquote"
            | "table"
            | "tr"
            | "figure"
            | "figcaption"
            | "hr"
            | "dl"
            | "dt"
            | "dd"
            | "form"
            | "fieldset"
            | "details"
            | "summary"
            | "address"
    ) {
        style.display = Display::Block;
    }
    if matches!(
        tag,
        "head" | "title" | "style" | "script" | "meta" | "link" | "base" | "template"
    ) || doc.attr(id, "hidden").is_some()
    {
        style.display = Display::None;
    }
    match tag {
        "body" => style.margin = [Length::Px(8.); 4],
        "p" | "pre" | "blockquote" | "ul" | "ol" | "dl" | "figure" => {
            style.margin[0] = Length::Em(1.);
            style.margin[2] = Length::Em(1.);
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            style.font_size = parent.font_size
                * match tag {
                    "h1" => 2.,
                    "h2" => 1.5,
                    "h3" => 1.17,
                    "h5" => 0.83,
                    "h6" => 0.67,
                    _ => 1.,
                };
            style.bold = true;
            style.margin[0] = Length::Em(0.67);
            style.margin[2] = Length::Em(0.67);
        }
        "a" if doc.attr(id, "href").is_some() => {
            style.color = Color(0, 0, 238, 255);
            style.underline = true;
        }
        "b" | "strong" | "th" => style.bold = true,
        "i" | "em" | "cite" | "address" => style.italic = true,
        "small" => style.font_size *= 0.83,
        "code" | "kbd" | "samp" => style.mono = true,
        "hr" => {
            style.border_width = [1.; 4];
            style.border_visible = [true; 4];
            style.margin[0] = Length::Em(0.5);
            style.margin[2] = Length::Em(0.5);
        }
        _ => {}
    }
    if tag == "pre" {
        style.pre = true;
        style.wrap = false;
        style.mono = true;
    }
    if matches!(tag, "ul" | "ol") {
        style.padding[3] = Length::Px(40.);
    }
    if matches!(tag, "blockquote" | "figure") {
        style.margin[1] = Length::Px(40.);
        style.margin[3] = Length::Px(40.);
    }
    if matches!(
        tag,
        "b" | "strong" | "th" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    ) {
        style.font_weight = 700;
    }
    if matches!(tag, "i" | "em" | "cite" | "address") {
        style.font_style = FontStyle::Italic;
    }
    if matches!(tag, "pre" | "code" | "kbd" | "samp") {
        style.font_family = vec![FontFamily::Generic(GenericFamily::Monospace)].into();
    }
    style
}
fn copy_property(s: &mut Style, from: &Style, p: Property, source: Property) {
    use Property::*;
    match p {
        FontSize => s.font_size = from.font_size,
        Color => s.color = from.color,
        Display => s.display = from.display,
        Float => s.float = from.float,
        Clear => s.clear = from.clear,
        Position => s.position = from.position,
        Top | Right | Bottom | Left => {
            s.inset[p as usize - Top as usize] = from.inset[source as usize - Top as usize]
        }
        ZIndex => s.z_index = from.z_index,
        OverflowX | OverflowY => {
            s.overflow[p as usize - OverflowX as usize] =
                from.overflow[p as usize - OverflowX as usize]
        }
        Transform => s.transform = from.transform.clone(),
        TransformOrigin => s.transform_origin = from.transform_origin,
        Background => s.background = from.background,
        Bold => {
            s.bold = from.bold;
            s.font_weight = from.font_weight;
        }
        Italic => {
            s.italic = from.italic;
            s.font_style = from.font_style;
        }
        Mono => {
            s.mono = from.mono;
            s.font_family = from.font_family.clone();
        }
        Underline => s.underline = from.underline,
        WhiteSpace => {
            s.pre = from.pre;
            s.wrap = from.wrap
        }
        TextAlign => s.align = from.align.clone(),
        LineHeight => {
            s.line_height = from.line_height;
            s.line_height_absolute = from.line_height_absolute
        }
        MarginTop | MarginRight | MarginBottom | MarginLeft => {
            s.margin[p as usize - MarginTop as usize] =
                from.margin[source as usize - MarginTop as usize]
        }
        PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => {
            s.padding[p as usize - PaddingTop as usize] =
                from.padding[source as usize - PaddingTop as usize]
        }
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth => {
            let i = p as usize - BorderTopWidth as usize;
            s.border_width[i] = from.border_width[i]
        }
        BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle => {
            let i = p as usize - BorderTopStyle as usize;
            s.border_visible[i] = from.border_visible[i]
        }
        BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => {
            let i = p as usize - BorderTopColor as usize;
            s.border_colors[i] = from.border_colors[i]
        }
        Width => s.width = from.width,
        Height => s.height = from.height,
        MinWidth => s.min_width = from.min_width,
        MaxWidth => s.max_width = from.max_width,
        MinHeight => s.min_height = from.min_height,
        MaxHeight => s.max_height = from.max_height,
        BoxSizing => s.box_sizing = from.box_sizing,
        FlexDirection => s.flex_direction = from.flex_direction,
        FlexWrap => s.flex_wrap = from.flex_wrap,
        FlexGrow => s.flex_grow = from.flex_grow,
        FlexShrink => s.flex_shrink = from.flex_shrink,
        FlexBasis => s.flex_basis = from.flex_basis,
        JustifyContent => s.justify_content = from.justify_content,
        AlignItems => s.align_items = from.align_items,
        AlignSelf => s.align_self = from.align_self,
        RowGap => s.gap[0] = from.gap[0],
        ColumnGap => s.gap[1] = from.gap[1],
        Order => s.order = from.order,
        BreakWords => s.break_words = from.break_words,
        GridTemplateColumns => s.grid_template_columns = from.grid_template_columns.clone(),
        GridTemplateRows => s.grid_template_rows = from.grid_template_rows.clone(),
        GridTemplateAreas => s.grid_template_areas = from.grid_template_areas.clone(),
        GridAutoColumns => s.grid_auto_columns = from.grid_auto_columns.clone(),
        GridAutoRows => s.grid_auto_rows = from.grid_auto_rows.clone(),
        GridColumnStart => s.grid_column[0] = from.grid_column[0].clone(),
        GridColumnEnd => s.grid_column[1] = from.grid_column[1].clone(),
        GridRowStart => s.grid_row[0] = from.grid_row[0].clone(),
        GridRowEnd => s.grid_row[1] = from.grid_row[1].clone(),
        GridAutoFlow => {
            s.grid_auto_flow = from.grid_auto_flow;
            s.grid_dense = from.grid_dense;
        }
        JustifyItems => s.justify_items = from.justify_items,
        JustifySelf => s.justify_self = from.justify_self,
        AlignContent => s.align_content = from.align_content,
        Direction | MarginInlineStart | MarginInlineEnd | PaddingInlineStart | PaddingInlineEnd
        | InsetInlineStart | InsetInlineEnd => unreachable!("only physical properties are copied"),
    }
}
fn apply_value(
    s: &mut Style,
    parent: &Style,
    p: Property,
    value: &Value,
    root_font: f32,
    viewport: [f32; 2],
) {
    use Property::*;
    let font = if p == FontSize {
        parent.font_size
    } else {
        s.font_size
    };
    let l = |length: &SpecifiedLength| length.computed(root_font, viewport, font);
    match (p, value) {
        (FontSize, Value::Length(length)) => {
            s.font_size = l(length)
                .resolve(parent.font_size, parent.font_size)
                .unwrap_or(parent.font_size)
                .clamp(0., 512.)
        }
        (Color, Value::Color(color)) => s.color = color.unwrap_or(parent.color),
        (Background, Value::Color(color)) => s.background = Some(color.unwrap_or(s.color)),
        (Display, Value::Display(v)) => s.display = *v,
        (Float, Value::Float(v)) => s.float = *v,
        (Clear, Value::Clear(v)) => s.clear = *v,
        (Position, Value::Position(v)) => s.position = *v,
        (Top | Right | Bottom | Left, Value::Length(v)) => {
            s.inset[p as usize - Top as usize] = l(v)
        }
        (ZIndex, Value::ZIndex(v)) => s.z_index = *v,
        (OverflowX | OverflowY, Value::Overflow(v)) => {
            s.overflow[p as usize - OverflowX as usize] = *v
        }
        (Transform, Value::Transforms(v)) => {
            s.transform = Arc::new(
                v.iter()
                    .copied()
                    .map(|op| op.map_lengths(|v| l(&v)))
                    .collect(),
            )
        }
        (TransformOrigin, Value::Origin(v)) => s.transform_origin = v.map(|v| l(&v)),
        (Bold, Value::FontWeight(v)) => {
            s.font_weight = *v;
            s.bold = *v >= 600;
        }
        (Bold, Value::RelativeWeight(bolder)) => {
            s.font_weight = match (*bolder, parent.font_weight) {
                (true, 1..350) => 400,
                (true, 350..550) => 700,
                (true, 550..900) => 900,
                (false, 100..550) => 100,
                (false, 550..750) => 400,
                (false, 750..) => 700,
                (_, weight) => weight,
            };
            s.bold = s.font_weight >= 600;
        }
        (Italic, Value::FontStyle(v)) => {
            s.font_style = *v;
            s.italic = *v != FontStyle::Normal;
        }
        (Mono, Value::Families(v)) => {
            s.font_family = v.clone();
            s.mono = matches!(
                v.first(),
                Some(FontFamily::Generic(
                    GenericFamily::Monospace | GenericFamily::UiMonospace
                ))
            );
        }
        (Underline, Value::Bool(v)) => s.underline = *v,
        (WhiteSpace, Value::WhiteSpace { pre, wrap }) => {
            s.pre = *pre;
            s.wrap = *wrap
        }
        (TextAlign, Value::Text(v)) => s.align = v.clone(),
        (LineHeight, Value::Number(v)) => {
            s.line_height = *v;
            s.line_height_absolute = None
        }
        (LineHeight, Value::Length(v)) => {
            s.line_height_absolute = l(v).resolve(s.font_size, s.font_size)
        }
        (MarginTop | MarginRight | MarginBottom | MarginLeft, Value::Length(v)) => {
            s.margin[p as usize - MarginTop as usize] = l(v)
        }
        (PaddingTop | PaddingRight | PaddingBottom | PaddingLeft, Value::Length(v)) => {
            s.padding[p as usize - PaddingTop as usize] = l(v)
        }
        (
            BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth,
            Value::Length(v),
        ) => {
            s.border_width[p as usize - BorderTopWidth as usize] =
                l(v).resolve(0., s.font_size).unwrap_or(0.).max(0.)
        }
        (
            BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle,
            Value::Bool(v),
        ) => s.border_visible[p as usize - BorderTopStyle as usize] = *v,
        (
            BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor,
            Value::Color(v),
        ) => s.border_colors[p as usize - BorderTopColor as usize] = *v,
        (Width, Value::Length(v)) => s.width = l(v),
        (Height, Value::Length(v)) => s.height = l(v),
        (MinWidth, Value::Length(v)) => s.min_width = l(v),
        (MaxWidth, Value::Length(v)) => s.max_width = l(v),
        (MinHeight, Value::Length(v)) => s.min_height = l(v),
        (MaxHeight, Value::Length(v)) => s.max_height = l(v),
        (BoxSizing, Value::Sizing(v)) => s.box_sizing = *v,
        (FlexDirection, Value::Direction(v)) => s.flex_direction = *v,
        (FlexWrap, Value::Bool(v)) => s.flex_wrap = *v,
        (FlexGrow, Value::Number(v)) => s.flex_grow = *v,
        (FlexShrink, Value::Number(v)) => s.flex_shrink = *v,
        (FlexBasis, Value::Length(v)) => s.flex_basis = l(v),
        (JustifyContent, Value::Align(v)) => s.justify_content = *v,
        (AlignItems, Value::Align(v)) => s.align_items = *v,
        (AlignSelf, Value::Align(v)) => s.align_self = *v,
        (RowGap, Value::Length(v)) => s.gap[0] = l(v),
        (ColumnGap, Value::Length(v)) => s.gap[1] = l(v),
        (Order, Value::Number(v)) => s.order = *v as i32,
        (BreakWords, Value::Bool(v)) => s.break_words = *v,
        (GridTemplateColumns | GridTemplateRows, Value::GridTemplate(template)) => {
            let template = template.computed(root_font, viewport, font);
            if p == GridTemplateColumns {
                s.grid_template_columns = template;
            } else {
                s.grid_template_rows = template;
            }
        }
        (GridTemplateAreas, Value::GridAreas(areas)) => s.grid_template_areas = areas.clone(),
        (GridAutoColumns | GridAutoRows, Value::Tracks(tracks)) => {
            let tracks = tracks.computed(root_font, viewport, font);
            if p == GridAutoColumns {
                s.grid_auto_columns = tracks;
            } else {
                s.grid_auto_rows = tracks;
            }
        }
        (GridColumnStart, Value::GridLine(line)) => s.grid_column[0] = line.clone(),
        (GridColumnEnd, Value::GridLine(line)) => s.grid_column[1] = line.clone(),
        (GridRowStart, Value::GridLine(line)) => s.grid_row[0] = line.clone(),
        (GridRowEnd, Value::GridLine(line)) => s.grid_row[1] = line.clone(),
        (GridAutoFlow, Value::GridFlow { axis, dense }) => {
            s.grid_auto_flow = *axis;
            s.grid_dense = *dense;
        }
        (JustifyItems, Value::Align(value)) => s.justify_items = *value,
        (JustifySelf, Value::Align(value)) => s.justify_self = *value,
        (AlignContent, Value::Align(value)) => s.align_content = *value,
        _ => {}
    }
}

fn targets(name: &str) -> Vec<Property> {
    use Property::*;
    match name {
        "all" => PROPERTIES.to_vec(),
        "color" => vec![Color],
        "background" | "background-color" => vec![Background],
        "display" => vec![Display],
        "float" => vec![Float],
        "clear" => vec![Clear],
        "font-size" => vec![FontSize],
        "font-weight" => vec![Bold],
        "font-style" => vec![Italic],
        "font-family" => vec![Mono],
        "font" => vec![FontSize, Bold, Italic, Mono, LineHeight],
        "text-decoration" | "text-decoration-line" => vec![Underline],
        "white-space" => vec![WhiteSpace],
        "text-align" => vec![TextAlign],
        "line-height" => vec![LineHeight],
        "direction" => vec![Direction],
        "margin" => vec![MarginTop, MarginRight, MarginBottom, MarginLeft],
        "padding" => vec![PaddingTop, PaddingRight, PaddingBottom, PaddingLeft],
        "margin-block" => vec![MarginTop, MarginBottom],
        "margin-inline" => vec![MarginInlineStart, MarginInlineEnd],
        "margin-top" | "margin-block-start" => vec![MarginTop],
        "margin-right" => vec![MarginRight],
        "margin-bottom" | "margin-block-end" => vec![MarginBottom],
        "margin-left" => vec![MarginLeft],
        "margin-inline-start" => vec![MarginInlineStart],
        "margin-inline-end" => vec![MarginInlineEnd],
        "padding-block" => vec![PaddingTop, PaddingBottom],
        "padding-inline" => vec![PaddingInlineStart, PaddingInlineEnd],
        "padding-top" | "padding-block-start" => vec![PaddingTop],
        "padding-right" => vec![PaddingRight],
        "padding-bottom" | "padding-block-end" => vec![PaddingBottom],
        "padding-left" => vec![PaddingLeft],
        "padding-inline-start" => vec![PaddingInlineStart],
        "padding-inline-end" => vec![PaddingInlineEnd],
        "border-width" => vec![
            BorderTopWidth,
            BorderRightWidth,
            BorderBottomWidth,
            BorderLeftWidth,
        ],
        "border-style" => vec![
            BorderTopStyle,
            BorderRightStyle,
            BorderBottomStyle,
            BorderLeftStyle,
        ],
        "border-color" => vec![
            BorderTopColor,
            BorderRightColor,
            BorderBottomColor,
            BorderLeftColor,
        ],
        "border" => vec![
            BorderTopWidth,
            BorderRightWidth,
            BorderBottomWidth,
            BorderLeftWidth,
            BorderTopStyle,
            BorderRightStyle,
            BorderBottomStyle,
            BorderLeftStyle,
            BorderTopColor,
            BorderRightColor,
            BorderBottomColor,
            BorderLeftColor,
        ],
        "border-top" => vec![BorderTopWidth, BorderTopStyle, BorderTopColor],
        "border-right" => vec![BorderRightWidth, BorderRightStyle, BorderRightColor],
        "border-bottom" => vec![BorderBottomWidth, BorderBottomStyle, BorderBottomColor],
        "border-left" => vec![BorderLeftWidth, BorderLeftStyle, BorderLeftColor],
        "border-top-width" => vec![BorderTopWidth],
        "border-right-width" => vec![BorderRightWidth],
        "border-bottom-width" => vec![BorderBottomWidth],
        "border-left-width" => vec![BorderLeftWidth],
        "border-top-style" => vec![BorderTopStyle],
        "border-right-style" => vec![BorderRightStyle],
        "border-bottom-style" => vec![BorderBottomStyle],
        "border-left-style" => vec![BorderLeftStyle],
        "border-top-color" => vec![BorderTopColor],
        "border-right-color" => vec![BorderRightColor],
        "border-bottom-color" => vec![BorderBottomColor],
        "border-left-color" => vec![BorderLeftColor],
        "position" => vec![Position],
        "top" | "inset-block-start" => vec![Top],
        "right" => vec![Right],
        "bottom" | "inset-block-end" => vec![Bottom],
        "left" => vec![Left],
        "inset" => vec![Top, Right, Bottom, Left],
        "inset-block" => vec![Top, Bottom],
        "inset-inline" => vec![InsetInlineStart, InsetInlineEnd],
        "inset-inline-start" => vec![InsetInlineStart],
        "inset-inline-end" => vec![InsetInlineEnd],
        "z-index" => vec![ZIndex],
        "overflow" => vec![OverflowX, OverflowY],
        "overflow-x" => vec![OverflowX],
        "overflow-y" => vec![OverflowY],
        "transform" => vec![Transform],
        "transform-origin" => vec![TransformOrigin],
        "width" | "inline-size" => vec![Width],
        "height" | "block-size" => vec![Height],
        "min-width" | "min-inline-size" => vec![MinWidth],
        "max-width" | "max-inline-size" => vec![MaxWidth],
        "min-height" | "min-block-size" => vec![MinHeight],
        "max-height" | "max-block-size" => vec![MaxHeight],
        "box-sizing" => vec![BoxSizing],
        "flex-direction" => vec![FlexDirection],
        "flex-wrap" => vec![FlexWrap],
        "flex-flow" => vec![FlexDirection, FlexWrap],
        "flex-grow" => vec![FlexGrow],
        "flex-shrink" => vec![FlexShrink],
        "flex-basis" => vec![FlexBasis],
        "flex" => vec![FlexGrow, FlexShrink, FlexBasis],
        "justify-content" => vec![JustifyContent],
        "align-items" => vec![AlignItems],
        "align-self" => vec![AlignSelf],
        "gap" => vec![RowGap, ColumnGap],
        "row-gap" => vec![RowGap],
        "column-gap" => vec![ColumnGap],
        "order" => vec![Order],
        "overflow-wrap" | "word-wrap" | "word-break" => vec![BreakWords],
        "grid-template-columns" => vec![GridTemplateColumns],
        "grid-template-rows" => vec![GridTemplateRows],
        "grid-template-areas" => vec![GridTemplateAreas],
        "grid-template" => vec![GridTemplateRows, GridTemplateColumns, GridTemplateAreas],
        "grid-auto-columns" => vec![GridAutoColumns],
        "grid-auto-rows" => vec![GridAutoRows],
        "grid-column-start" => vec![GridColumnStart],
        "grid-column-end" => vec![GridColumnEnd],
        "grid-row-start" => vec![GridRowStart],
        "grid-row-end" => vec![GridRowEnd],
        "grid-column" => vec![GridColumnStart, GridColumnEnd],
        "grid-row" => vec![GridRowStart, GridRowEnd],
        "grid-area" => vec![GridRowStart, GridColumnStart, GridRowEnd, GridColumnEnd],
        "grid-auto-flow" => vec![GridAutoFlow],
        "justify-items" => vec![JustifyItems],
        "justify-self" => vec![JustifySelf],
        "align-content" => vec![AlignContent],
        "place-items" => vec![AlignItems, JustifyItems],
        "place-self" => vec![AlignSelf, JustifySelf],
        "place-content" => vec![AlignContent, JustifyContent],
        _ => vec![],
    }
}
fn compile_declarations(declarations: &[Declaration]) -> Vec<CompiledDeclaration> {
    let mut result = vec![];
    for (order, decl) in declarations.iter().enumerate() {
        for (property, value) in parse_assignment(&decl.name, &decl.value) {
            result.push(CompiledDeclaration {
                property,
                value,
                important: decl.important,
                order,
            });
        }
    }
    result
}
fn whitespace_tokens(source: &str) -> Vec<&str> {
    let mut result = vec![];
    let mut start = 0;
    let (mut quote, mut escaped, mut depth) = (0, false, 0u32);
    for (i, &b) in source.as_bytes().iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
            continue;
        }
        if matches!(b, b'\'' | b'"') {
            quote = b;
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && (b.is_ascii_whitespace() || b == 0) {
            if start < i {
                result.push(&source[start..i]);
            }
            start = i + 1;
        }
    }
    if start < source.len() {
        result.push(&source[start..]);
    }
    result
}

fn contains_calc_function(source: &str) -> bool {
    if source
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"calc("))
    {
        return true;
    }
    if !source.contains('\\') {
        return false;
    }
    // Escaped identifiers can spell the same function name. Consume a whole
    // identifier at a time so long invalid names cannot cause repeated scans.
    let mut offset = 0;
    while offset < source.len() {
        let mut end = offset;
        if let Some(name) = identifier(source, &mut end) {
            if name.eq_ignore_ascii_case("calc") && source.as_bytes().get(end) == Some(&b'(') {
                return true;
            }
            offset = end;
        } else {
            offset += source[offset..].chars().next().unwrap().len_utf8();
        }
    }
    false
}

fn declaration_tokens(source: &str) -> String {
    let clean = without_comments(source);
    // A comment between calc tokens cannot supply the whitespace required by + or -.
    if clean.contains('\0') && !contains_calc_function(&clean) {
        clean.replace('\0', " ")
    } else {
        clean
    }
}

fn parse_assignment(name: &str, source: &str) -> Vec<(Property, Value)> {
    let properties = targets(name);
    if properties.is_empty() {
        return vec![];
    }
    let clean = declaration_tokens(source);
    let value = trim_boundary(&clean);
    let lower = value.to_ascii_lowercase();
    let wide = match lower.as_str() {
        "initial" => Some(Value::Initial),
        "inherit" => Some(Value::Inherit),
        "unset" => Some(Value::Unset),
        _ => None,
    };
    if let Some(wide) = wide {
        return properties.into_iter().map(|p| (p, wide.clone())).collect();
    }
    if contains_variable(value) {
        let source: Arc<str> = Arc::from(serialized_tokens(value));
        let name: Arc<str> = Arc::from(name);
        return properties
            .into_iter()
            .map(|p| {
                (
                    p,
                    Value::Deferred {
                        name: name.clone(),
                        source: source.clone(),
                    },
                )
            })
            .collect();
    }
    if matches!(name, "grid-row" | "grid-column" | "grid-area") {
        let components = split_top(value, b'/');
        if components.len() > properties.len() {
            return vec![];
        }
        let Some(mut lines) = components
            .iter()
            .map(|value| grid::parse_line(value))
            .collect::<Option<Vec<_>>>()
        else {
            return vec![];
        };
        while lines.len() < properties.len() {
            let start = if lines.len() == 3 { 1 } else { 0 };
            lines.push(lines[start].shorthand_end());
        }
        return properties
            .into_iter()
            .zip(lines.into_iter().map(Value::GridLine))
            .collect();
    }
    if name == "grid-template" {
        let Some((rows, columns, areas)) = grid::parse_template(value) else {
            return vec![];
        };
        return properties
            .into_iter()
            .zip([
                Value::GridTemplate(rows),
                Value::GridTemplate(columns),
                Value::GridAreas(Arc::new(areas)),
            ])
            .collect();
    }
    if name == "overflow" {
        let tokens = whitespace_tokens(value);
        if tokens.is_empty() || tokens.len() > 2 {
            return vec![];
        }
        let Some(x) = parse_value(Property::OverflowX, tokens[0]) else {
            return vec![];
        };
        let Some(y) = parse_value(
            Property::OverflowY,
            tokens.get(1).copied().unwrap_or(tokens[0]),
        ) else {
            return vec![];
        };
        return vec![(Property::OverflowX, x), (Property::OverflowY, y)];
    }
    if matches!(
        name,
        "place-items"
            | "place-self"
            | "place-content"
            | "margin-block"
            | "margin-inline"
            | "padding-block"
            | "padding-inline"
            | "inset-block"
            | "inset-inline"
    ) {
        let tokens = whitespace_tokens(value);
        if tokens.is_empty() || tokens.len() > 2 {
            return vec![];
        }
        let Some(first) = parse_value(properties[0], tokens[0]) else {
            return vec![];
        };
        let Some(second) = parse_value(properties[1], tokens.get(1).copied().unwrap_or(tokens[0]))
        else {
            return vec![];
        };
        return vec![(properties[0], first), (properties[1], second)];
    }
    let tokens = whitespace_tokens(value);
    if matches!(
        name,
        "margin" | "padding" | "border-width" | "border-style" | "border-color" | "inset"
    ) {
        let Some(values) = tokens
            .iter()
            .map(|v| parse_value(properties[0], v))
            .collect::<Option<Vec<_>>>()
        else {
            return vec![];
        };
        let values = match values.as_slice() {
            [a] => vec![a.clone(); 4],
            [a, b] => vec![a.clone(), b.clone(), a.clone(), b.clone()],
            [a, b, c] => vec![a.clone(), b.clone(), c.clone(), b.clone()],
            [a, b, c, d] => vec![a.clone(), b.clone(), c.clone(), d.clone()],
            _ => return vec![],
        };
        return properties.into_iter().zip(values).collect();
    }
    if matches!(
        name,
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left"
    ) {
        let (mut width, mut style, mut color) = (None, None, None);
        for token in tokens {
            if let Some(v) = parse_value(Property::BorderTopWidth, token) {
                if width.replace(v).is_some() {
                    return vec![];
                }
            } else if let Some(v) = parse_value(Property::BorderTopStyle, token) {
                if style.replace(v).is_some() {
                    return vec![];
                }
            } else if let Some(v) = parse_value(Property::BorderTopColor, token) {
                if color.replace(v).is_some() {
                    return vec![];
                }
            } else {
                return vec![];
            }
        }
        if value.is_empty() {
            return vec![];
        }
        return properties
            .into_iter()
            .map(|p| {
                let v = if matches!(
                    p,
                    Property::BorderTopWidth
                        | Property::BorderRightWidth
                        | Property::BorderBottomWidth
                        | Property::BorderLeftWidth
                ) {
                    width
                        .clone()
                        .unwrap_or(Value::Length(Length::Px(3.).into()))
                } else if matches!(
                    p,
                    Property::BorderTopStyle
                        | Property::BorderRightStyle
                        | Property::BorderBottomStyle
                        | Property::BorderLeftStyle
                ) {
                    style.clone().unwrap_or(Value::Bool(false))
                } else {
                    color.clone().unwrap_or(Value::Color(None))
                };
                (p, v)
            })
            .collect();
    }
    if name == "gap" {
        if !(1..=2).contains(&tokens.len()) {
            return vec![];
        }
        let Some(first) = parse_value(Property::RowGap, tokens[0]) else {
            return vec![];
        };
        let Some(second) = parse_value(Property::ColumnGap, tokens.get(1).unwrap_or(&tokens[0]))
        else {
            return vec![];
        };
        return vec![(Property::RowGap, first), (Property::ColumnGap, second)];
    }
    if name == "flex-flow" {
        let (mut dir, mut wrap) = (None, None);
        for token in tokens {
            if let Some(v) = parse_value(Property::FlexDirection, token) {
                if dir.replace(v).is_some() {
                    return vec![];
                }
            } else if let Some(v) = parse_value(Property::FlexWrap, token) {
                if wrap.replace(v).is_some() {
                    return vec![];
                }
            } else {
                return vec![];
            }
        }
        return vec![
            (
                Property::FlexDirection,
                dir.unwrap_or(Value::Direction(FlexDirection::Row)),
            ),
            (Property::FlexWrap, wrap.unwrap_or(Value::Bool(false))),
        ];
    }
    if name == "flex" {
        let values = match lower.as_str() {
            "none" => Some((0., 0., Length::Auto.into())),
            "auto" => Some((1., 1., Length::Auto.into())),
            _ => {
                let mut numbers = vec![];
                let mut basis = None;
                let mut valid = !tokens.is_empty() && tokens.len() <= 3;
                for token in tokens {
                    if let Some(n) = number(token).filter(|n| *n >= 0.) {
                        if numbers.len() == 2 && n == 0. && basis.is_none() {
                            basis = Some(Length::Px(0.).into());
                        } else {
                            if basis.is_some() {
                                valid = false;
                            }
                            numbers.push(n);
                        }
                    } else if let Some(l) = nonnegative_length(token, true) {
                        if basis.replace(l).is_some() {
                            valid = false;
                        }
                    } else {
                        valid = false;
                    }
                }
                if !valid || numbers.len() > 2 {
                    None
                } else {
                    Some((
                        *numbers.first().unwrap_or(&1.),
                        *numbers.get(1).unwrap_or(&1.),
                        basis.unwrap_or(Length::Percent(0.).into()),
                    ))
                }
            }
        };
        return values
            .map(|(g, s, b)| {
                vec![
                    (Property::FlexGrow, Value::Number(g)),
                    (Property::FlexShrink, Value::Number(s)),
                    (Property::FlexBasis, Value::Length(b)),
                ]
            })
            .unwrap_or_default();
    }
    if name == "font" {
        return parse_font_shorthand(value);
    }
    if name == "background-color"
        && color(value).is_none()
        && !value.eq_ignore_ascii_case("currentcolor")
    {
        return vec![];
    }
    if name == "all" {
        return vec![];
    }
    if properties.len() != 1 {
        return vec![];
    }
    parse_value(properties[0], value)
        .map(|v| vec![(properties[0], v)])
        .unwrap_or_default()
}
fn parse_font_shorthand(source: &str) -> Vec<(Property, Value)> {
    let tokens = whitespace_tokens(source);
    let mut bold = Value::FontWeight(400);
    let mut italic = Value::FontStyle(FontStyle::Normal);
    let mut size_index = None;
    for (index, token) in tokens.iter().enumerate() {
        let sizes = split_top(token, b'/');
        if parse_value(Property::FontSize, sizes[0]).is_some() {
            size_index = Some(index);
            break;
        }
        if let Some(v) = parse_value(Property::Italic, token) {
            italic = v;
        } else if let Some(v) = parse_value(Property::Bold, token) {
            bold = v;
        } else if *token != "small-caps" {
            return vec![];
        }
    }
    let Some(index) = size_index else {
        return vec![];
    };
    let sizes = split_top(tokens[index], b'/');
    if sizes.len() > 2 {
        return vec![];
    }
    let Some(size) = parse_value(Property::FontSize, sizes[0]) else {
        return vec![];
    };
    let mut line = Value::Number(1.2);
    let mut family_index = index + 1;
    if let Some(height) = sizes.get(1) {
        if height.is_empty() {
            return vec![];
        }
        let Some(v) = parse_value(Property::LineHeight, height) else {
            return vec![];
        };
        line = v;
    } else if tokens.get(family_index) == Some(&"/") {
        let Some(height) = tokens.get(family_index + 1) else {
            return vec![];
        };
        let Some(v) = parse_value(Property::LineHeight, height) else {
            return vec![];
        };
        line = v;
        family_index += 2;
    }
    let family = tokens.get(family_index..).unwrap_or(&[]).join(" ");
    let Some(mono) = parse_value(Property::Mono, &family) else {
        return vec![];
    };
    vec![
        (Property::FontSize, size),
        (Property::Bold, bold),
        (Property::Italic, italic),
        (Property::Mono, mono),
        (Property::LineHeight, line),
    ]
}
fn parse_value(p: Property, value: &str) -> Option<Value> {
    use Property::*;
    let p = p.physical(false);
    let lower = trim_boundary(value).to_ascii_lowercase();
    let v = lower.as_str();
    match p {
        Direction => Some(Value::Bool(match v {
            "ltr" => false,
            "rtl" => true,
            _ => return None,
        })),
        MarginInlineStart | MarginInlineEnd | PaddingInlineStart | PaddingInlineEnd
        | InsetInlineStart | InsetInlineEnd => unreachable!("logical properties are mapped first"),
        Position => Some(Value::Position(match v {
            "static" => crate::css::Position::Static,
            "relative" => crate::css::Position::Relative,
            "sticky" => crate::css::Position::Sticky,
            "absolute" => crate::css::Position::Absolute,
            "fixed" => crate::css::Position::Fixed,
            _ => return None,
        })),
        Top | Right | Bottom | Left => specified_length(v).map(Value::Length),
        ZIndex => {
            if v == "auto" {
                Some(Value::ZIndex(None))
            } else {
                v.parse::<i32>().ok().map(|v| Value::ZIndex(Some(v)))
            }
        }
        OverflowX | OverflowY => Some(Value::Overflow(match v {
            "visible" => crate::css::Overflow::Visible,
            "hidden" => crate::css::Overflow::Hidden,
            "clip" => crate::css::Overflow::Clip,
            "auto" => crate::css::Overflow::Auto,
            "scroll" => crate::css::Overflow::Scroll,
            _ => return None,
        })),
        Transform => parse_transform(v).map(|v| Value::Transforms(Arc::new(v))),
        TransformOrigin => parse_transform_origin(v).map(Value::Origin),
        Color | Background | BorderTopColor | BorderRightColor | BorderBottomColor
        | BorderLeftColor => {
            if v == "currentcolor" {
                Some(Value::Color(None))
            } else if let Some(c) = color(value) {
                Some(Value::Color(Some(c)))
            } else if p == Background {
                let tokens = whitespace_tokens(value);
                let found = tokens.iter().rev().find_map(|t| {
                    if t.eq_ignore_ascii_case("currentcolor") {
                        Some(Value::Color(None))
                    } else {
                        color(t).map(|c| Value::Color(Some(c)))
                    }
                });
                found.or_else(|| {
                    tokens
                        .iter()
                        .any(|t| {
                            t.starts_with("url(")
                                || t.starts_with("linear-gradient(")
                                || t.eq_ignore_ascii_case("none")
                        })
                        .then_some(Value::Color(Some(crate::css::Color(0, 0, 0, 0))))
                })
            } else {
                None
            }
        }
        Float => Some(Value::Float(match v {
            "none" => crate::css::Float::None,
            "left" => crate::css::Float::Left,
            "right" => crate::css::Float::Right,
            "inline-start" => crate::css::Float::InlineStart,
            "inline-end" => crate::css::Float::InlineEnd,
            _ => return None,
        })),
        Clear => Some(Value::Clear(match v {
            "none" => crate::css::Clear::None,
            "left" => crate::css::Clear::Left,
            "right" => crate::css::Clear::Right,
            "both" => crate::css::Clear::Both,
            "inline-start" => crate::css::Clear::InlineStart,
            "inline-end" => crate::css::Clear::InlineEnd,
            _ => return None,
        })),
        Display => Some(Value::Display(match v {
            "none" => crate::css::Display::None,
            "block" | "list-item" | "table" => crate::css::Display::Block,
            "inline" => crate::css::Display::Inline,
            "inline-block" => crate::css::Display::InlineBlock,
            "flex" => crate::css::Display::Flex,
            "inline-flex" => crate::css::Display::InlineFlex,
            "grid" => crate::css::Display::Grid,
            "inline-grid" => crate::css::Display::InlineGrid,
            _ => return None,
        })),
        FontSize => Some(Value::Length(match v {
            "xx-small" => Length::Px(9.).into(),
            "x-small" => Length::Px(10.).into(),
            "small" => Length::Px(13.).into(),
            "medium" => Length::Px(16.).into(),
            "large" => Length::Px(18.).into(),
            "x-large" => Length::Px(24.).into(),
            "xx-large" => Length::Px(32.).into(),
            "xxx-large" => Length::Px(48.).into(),
            "larger" => Length::Em(1.2).into(),
            "smaller" => Length::Em(1. / 1.2).into(),
            _ => nonnegative_length(v, false)?,
        })),
        Bold => {
            if matches!(v, "bolder" | "lighter") {
                Some(Value::RelativeWeight(v == "bolder"))
            } else {
                absolute_weight(v).map(Value::FontWeight)
            }
        }
        Italic => parse_font_style(v).map(Value::FontStyle),
        Mono => font_families(value).map(|v| Value::Families(v.into())),
        Underline => {
            let tokens: Vec<_> = v.split_ascii_whitespace().collect();
            if tokens.is_empty()
                || !tokens.iter().all(|t| {
                    matches!(
                        *t,
                        "none"
                            | "underline"
                            | "overline"
                            | "line-through"
                            | "solid"
                            | "double"
                            | "dotted"
                            | "dashed"
                            | "wavy"
                    )
                })
            {
                None
            } else {
                Some(Value::Bool(tokens.contains(&"underline")))
            }
        }
        WhiteSpace => Some(match v {
            "normal" => Value::WhiteSpace {
                pre: false,
                wrap: true,
            },
            "nowrap" => Value::WhiteSpace {
                pre: false,
                wrap: false,
            },
            "pre" => Value::WhiteSpace {
                pre: true,
                wrap: false,
            },
            "pre-wrap" | "break-spaces" => Value::WhiteSpace {
                pre: true,
                wrap: true,
            },
            _ => return None,
        }),
        TextAlign => Some(Value::Text(
            match v {
                "left" | "start" => "left",
                "right" | "end" => "right",
                "center" => "center",
                _ => return None,
            }
            .into(),
        )),
        LineHeight => {
            if v == "normal" {
                Some(Value::Number(1.2))
            } else if let Some(n) = number(v).filter(|n| *n >= 0.) {
                Some(Value::Number(n.min(100.)))
            } else {
                Some(Value::Length(nonnegative_length(v, false)?))
            }
        }
        MarginTop | MarginRight | MarginBottom | MarginLeft => {
            specified_length(v).map(Value::Length)
        }
        PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => {
            nonnegative_length(v, false).map(Value::Length)
        }
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth => {
            let l = match v {
                "thin" => Length::Px(1.).into(),
                "medium" => Length::Px(3.).into(),
                "thick" => Length::Px(5.).into(),
                _ => nonnegative_length(v, false)?,
            };
            if l.has_percent() {
                None
            } else {
                Some(Value::Length(l))
            }
        }
        BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle => {
            Some(Value::Bool(match v {
                "none" | "hidden" => false,
                "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset"
                | "outset" => true,
                _ => return None,
            }))
        }
        Width | Height | MinWidth | MinHeight | MaxWidth | MaxHeight | FlexBasis => {
            if matches!(p, MaxWidth | MaxHeight) {
                if v == "none" {
                    Some(Value::Length(Length::Auto.into()))
                } else {
                    nonnegative_length(v, false).map(Value::Length)
                }
            } else {
                nonnegative_length(v, true).map(Value::Length)
            }
        }
        BoxSizing => Some(Value::Sizing(match v {
            "content-box" => crate::css::BoxSizing::ContentBox,
            "border-box" => crate::css::BoxSizing::BorderBox,
            _ => return None,
        })),
        FlexDirection => Some(Value::Direction(match v {
            "row" => crate::css::FlexDirection::Row,
            "row-reverse" => crate::css::FlexDirection::RowReverse,
            "column" => crate::css::FlexDirection::Column,
            "column-reverse" => crate::css::FlexDirection::ColumnReverse,
            _ => return None,
        })),
        FlexWrap => Some(Value::Bool(match v {
            "wrap" => true,
            "nowrap" => false,
            _ => return None,
        })),
        FlexGrow | FlexShrink => number(v)
            .filter(|n| *n >= 0.)
            .map(|n| Value::Number(n.min(1e6))),
        JustifyContent | AlignItems | AlignSelf | JustifyItems | JustifySelf | AlignContent => {
            Some(Value::Align(match v {
                "normal" | "stretch" => Align::Stretch,
                "start" | "flex-start" => Align::Start,
                "end" | "flex-end" => Align::End,
                "center" => Align::Center,
                "auto" if matches!(p, AlignSelf | JustifySelf) => Align::Auto,
                "space-between" if matches!(p, JustifyContent | AlignContent) => {
                    Align::SpaceBetween
                }
                "space-around" if matches!(p, JustifyContent | AlignContent) => Align::SpaceAround,
                "space-evenly" if matches!(p, JustifyContent | AlignContent) => Align::SpaceEvenly,
                _ => return None,
            }))
        }
        RowGap | ColumnGap => {
            if v == "normal" {
                Some(Value::Length(Length::Px(0.).into()))
            } else {
                nonnegative_length(v, false).map(Value::Length)
            }
        }
        Order => v.parse::<i32>().ok().map(|n| Value::Number(n as f32)),
        BreakWords => Some(Value::Bool(match v {
            "normal" | "keep-all" => false,
            "break-word" | "anywhere" | "break-all" => true,
            _ => return None,
        })),
        GridTemplateColumns | GridTemplateRows => {
            grid::parse_tracks(value).map(Value::GridTemplate)
        }
        GridTemplateAreas => grid::parse_areas(value).map(Arc::new).map(Value::GridAreas),
        GridAutoColumns | GridAutoRows => grid::parse_auto_tracks(value).map(Value::Tracks),
        GridColumnStart | GridColumnEnd | GridRowStart | GridRowEnd => {
            grid::parse_line(value).map(Value::GridLine)
        }
        GridAutoFlow => {
            let tokens = whitespace_tokens(v);
            if tokens.is_empty() || tokens.len() > 2 {
                return None;
            }
            let mut axis = None;
            let mut dense = false;
            for token in tokens {
                match token {
                    "row" if axis.is_none() => axis = Some(crate::css::GridAutoFlow::Row),
                    "column" if axis.is_none() => axis = Some(crate::css::GridAutoFlow::Column),
                    "dense" if !dense => dense = true,
                    _ => return None,
                }
            }
            Some(Value::GridFlow {
                axis: axis.unwrap_or_default(),
                dense,
            })
        }
    }
}

fn parse_angle(source: &str) -> Option<f32> {
    for (unit, factor) in [
        ("deg", std::f32::consts::PI / 180.),
        ("grad", std::f32::consts::PI / 200.),
        ("turn", std::f32::consts::TAU),
        ("rad", 1.),
    ] {
        if let Some(value) = source.strip_suffix(unit) {
            return number(value).map(|n| n * factor).filter(|n| n.is_finite());
        }
    }
    (source == "0").then_some(0.)
}
fn transform_length(value: &str) -> Option<SpecifiedLength> {
    specified_length(value).filter(|length| !matches!(length, SpecifiedLength::Plain(Length::Auto)))
}
fn parse_transform(source: &str) -> Option<Vec<TransformOp<SpecifiedLength>>> {
    if source == "none" {
        return Some(vec![]);
    }
    if source.len() > 16384 {
        return None;
    }
    let mut result = vec![];
    let mut rest = source.trim();
    while !rest.is_empty() {
        if result.len() >= 32 {
            return None;
        }
        let open = rest.find('(')?;
        let name = rest[..open].trim();
        if name.is_empty() {
            return None;
        }
        let close = matching_close(rest, open, b'(', b')')?;
        let args = &rest[open + 1..close];
        let values = split_top(args, b',');
        if values.iter().any(|v| trim_boundary(v).is_empty()) {
            return None;
        }
        let n = |index: usize| values.get(index).and_then(|v| number(v.trim()));
        let len = |index: usize| values.get(index).and_then(|v| transform_length(v));
        let angle = |index: usize| values.get(index).and_then(|v| parse_angle(v.trim()));
        let op = match (name, values.len()) {
            ("matrix", 6) => TransformOp::Matrix([n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?]),
            ("translate", 1 | 2) => TransformOp::Translate([
                len(0)?,
                if values.len() == 2 {
                    len(1)?
                } else {
                    Length::Px(0.).into()
                },
            ]),
            ("translatex", 1) => TransformOp::Translate([len(0)?, Length::Px(0.).into()]),
            ("translatey", 1) => TransformOp::Translate([Length::Px(0.).into(), len(0)?]),
            ("scale", 1 | 2) => {
                TransformOp::Scale([n(0)?, if values.len() == 2 { n(1)? } else { n(0)? }])
            }
            ("scalex", 1) => TransformOp::Scale([n(0)?, 1.]),
            ("scaley", 1) => TransformOp::Scale([1., n(0)?]),
            ("rotate", 1) => TransformOp::Rotate(angle(0)?),
            ("skew", 1 | 2) => {
                TransformOp::Skew([angle(0)?, if values.len() == 2 { angle(1)? } else { 0. }])
            }
            ("skewx", 1) => TransformOp::Skew([angle(0)?, 0.]),
            ("skewy", 1) => TransformOp::Skew([0., angle(0)?]),
            _ => return None,
        };
        result.push(op);
        rest = trim_boundary(&rest[close + 1..]);
    }
    (!result.is_empty()).then_some(result)
}
fn parse_transform_origin(source: &str) -> Option<[SpecifiedLength; 2]> {
    let tokens = whitespace_tokens(source);
    if tokens.is_empty() || tokens.len() > 2 {
        return None;
    }
    let horizontal = |v: &str| match v {
        "left" => Some(Length::Percent(0.).into()),
        "center" => Some(Length::Percent(50.).into()),
        "right" => Some(Length::Percent(100.).into()),
        _ => transform_length(v),
    };
    let vertical = |v: &str| match v {
        "top" => Some(Length::Percent(0.).into()),
        "center" => Some(Length::Percent(50.).into()),
        "bottom" => Some(Length::Percent(100.).into()),
        _ => transform_length(v),
    };
    if tokens.len() == 1 {
        return if matches!(tokens[0], "top" | "bottom") {
            Some([Length::Percent(50.).into(), vertical(tokens[0])?])
        } else {
            Some([horizontal(tokens[0])?, Length::Percent(50.).into()])
        };
    }
    if matches!(tokens[0], "top" | "bottom") || matches!(tokens[1], "left" | "right") {
        Some([horizontal(tokens[1])?, vertical(tokens[0])?])
    } else {
        Some([horizontal(tokens[0])?, vertical(tokens[1])?])
    }
}
fn number(value: &str) -> Option<f32> {
    let n = value.parse::<f32>().ok()?;
    n.is_finite().then_some(n)
}
fn nonnegative_length(value: &str, auto: bool) -> Option<SpecifiedLength> {
    let l = specified_length(value)?;
    match l {
        SpecifiedLength::Plain(Length::Auto) if !auto => None,
        SpecifiedLength::Plain(
            Length::Px(n)
            | Length::Em(n)
            | Length::Rem(n)
            | Length::Vw(n)
            | Length::Vh(n)
            | Length::Percent(n),
        ) if n < 0. => None,
        SpecifiedLength::Calc(mut calc) => {
            calc.nonnegative = true;
            Some(SpecifiedLength::Calc(calc))
        }
        _ => Some(l),
    }
}

fn specified_length(value: &str) -> Option<SpecifiedLength> {
    let value = trim_boundary(value);
    if value.starts_with(['c', 'C', '\\']) {
        calc::parse(value).map(SpecifiedLength::Calc)
    } else {
        length(value).map(SpecifiedLength::from)
    }
}

fn absolute_length_scale(unit: &str) -> Option<f64> {
    Some(match unit {
        "px" => 1.,
        "pt" => 96. / 72.,
        "pc" => 16.,
        "in" => 96.,
        "cm" => 96. / 2.54,
        "mm" => 96. / 25.4,
        "q" => 96. / 101.6,
        _ => return None,
    })
}

pub fn length(value: &str) -> Option<Length> {
    let lower = trim_boundary(value).to_ascii_lowercase();
    let v = lower.as_str();
    if v == "auto" {
        return Some(Length::Auto);
    }
    let units = [
        "rem", "px", "em", "vw", "vh", "pt", "pc", "in", "cm", "mm", "q", "%",
    ];
    for unit in units {
        if let Some(raw) = v.strip_suffix(unit) {
            let n = number(raw)?;
            return Some(match unit {
                "em" => Length::Em(n),
                "rem" => Length::Rem(n),
                "vw" => Length::Vw(n),
                "vh" => Length::Vh(n),
                "%" => Length::Percent(n),
                _ => {
                    let px = (f64::from(n) * absolute_length_scale(unit)?) as f32;
                    Length::Px(px.is_finite().then_some(px)?)
                }
            });
        }
    }
    let n = number(v)?;
    if n == 0. { Some(Length::Px(0.)) } else { None }
}

fn functional_color_components(inner: &str, rgb: bool) -> Option<([&str; 3], Option<&str>)> {
    if inner.contains(',') {
        // Legacy syntax uses commas exclusively, including before alpha.
        if inner.contains('/') {
            return None;
        }
        let mut values = inner.split(',').map(str::trim_ascii);
        let channels = [values.next()?, values.next()?, values.next()?];
        let alpha = values.next();
        if values.next().is_some() {
            return None;
        }
        if rgb {
            let percent = channels[0].ends_with('%');
            if channels[1..].iter().any(|s| s.ends_with('%') != percent) {
                return None;
            }
        }
        Some((channels, alpha))
    } else {
        // Modern syntax uses a slash before the optional alpha component.
        let (channels, alpha) = inner.split_once('/').map_or((inner, None), |(head, tail)| {
            (head, Some(tail.trim_ascii()))
        });
        let mut values = channels.split_ascii_whitespace();
        let channels = [values.next()?, values.next()?, values.next()?];
        values.next().is_none().then_some((channels, alpha))
    }
}

pub fn color(value: &str) -> Option<Color> {
    let v = value.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        return match hex.len() {
            3 | 4 => {
                let mut n = hex.chars().map(|c| c.to_digit(16).map(|n| n as u8 * 17));
                Some(Color(
                    n.next()??,
                    n.next()??,
                    n.next()??,
                    if hex.len() == 4 { n.next()?? } else { 255 },
                ))
            }
            6 | 8 => {
                // The radix parser accepts a leading '+', which is not a hex digit.
                if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                let n = u32::from_str_radix(hex, 16).ok()?;
                Some(if hex.len() == 6 {
                    Color((n >> 16) as u8, (n >> 8) as u8, n as u8, 255)
                } else {
                    Color((n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8)
                })
            }
            _ => None,
        };
    }
    let alpha = |s: &str| -> Option<u8> {
        let n = if let Some(n) = s.strip_suffix('%') {
            number(n)? / 100.
        } else {
            number(s)?
        };
        Some((n.clamp(0., 1.) * 255.).round() as u8)
    };
    if v.starts_with("rgb(")
        || v.starts_with("rgba(")
        || v.starts_with("hsl(")
        || v.starts_with("hsla(")
    {
        let inner = v.split_once('(')?.1.strip_suffix(')')?;
        let rgb = v.starts_with("rgb");
        let (values, opacity) = functional_color_components(inner, rgb)?;
        let a = opacity.map_or(Some(255), alpha)?;
        if !rgb {
            let hue = values[0];
            let h = if let Some(n) = hue.strip_suffix("turn") {
                number(n)? * 360.
            } else if let Some(n) = hue.strip_suffix("grad") {
                number(n)? * 0.9
            } else if let Some(n) = hue.strip_suffix("rad") {
                number(n)? * 180. / std::f32::consts::PI
            } else {
                number(hue.strip_suffix("deg").unwrap_or(hue))?
            };
            let s = (number(values[1].strip_suffix('%')?)? / 100.).clamp(0., 1.);
            let l = (number(values[2].strip_suffix('%')?)? / 100.).clamp(0., 1.);
            let c = (1. - (2. * l - 1.).abs()) * s;
            let h = h.rem_euclid(360.) / 60.;
            let x = c * (1. - (h.rem_euclid(2.) - 1.).abs());
            let m = l - c / 2.;
            let (r, g, b) = match h as u8 {
                0 => (c, x, 0.),
                1 => (x, c, 0.),
                2 => (0., c, x),
                3 => (0., x, c),
                4 => (x, 0., c),
                _ => (c, 0., x),
            };
            return Some(Color(
                ((r + m) * 255.).round() as u8,
                ((g + m) * 255.).round() as u8,
                ((b + m) * 255.).round() as u8,
                a,
            ));
        }
        let channel = |s: &str| -> Option<u8> {
            let n = if let Some(n) = s.strip_suffix('%') {
                number(n)? * 255. / 100.
            } else {
                number(s)?
            };
            Some(n.clamp(0., 255.).round() as u8)
        };
        return Some(Color(
            channel(values[0])?,
            channel(values[1])?,
            channel(values[2])?,
            a,
        ));
    }
    Some(match v.as_str() {
        "transparent" => Color(0, 0, 0, 0),
        "black" => Color::BLACK,
        "white" => Color::WHITE,
        "red" => Color::rgb(255, 0, 0),
        "green" => Color::rgb(0, 128, 0),
        "blue" => Color::rgb(0, 0, 255),
        "gray" | "grey" => Color::rgb(128, 128, 128),
        "silver" => Color::rgb(192, 192, 192),
        "navy" => Color::rgb(0, 0, 128),
        "teal" => Color::rgb(0, 128, 128),
        "aqua" | "cyan" => Color::rgb(0, 255, 255),
        "yellow" => Color::rgb(255, 255, 0),
        "orange" => Color::rgb(255, 165, 0),
        "purple" => Color::rgb(128, 0, 128),
        "rebeccapurple" => Color::rgb(102, 51, 153),
        "maroon" => Color::rgb(128, 0, 0),
        "fuchsia" | "magenta" => Color::rgb(255, 0, 255),
        "lime" => Color::rgb(0, 255, 0),
        "olive" => Color::rgb(128, 128, 0),
        "lightgray" | "lightgrey" => Color::rgb(211, 211, 211),
        "darkgray" | "darkgrey" => Color::rgb(169, 169, 169),
        "whitesmoke" => Color::rgb(245, 245, 245),
        "aliceblue" => Color::rgb(240, 248, 255),
        "pink" => Color::rgb(255, 192, 203),
        "brown" => Color::rgb(165, 42, 42),
        _ => return None,
    })
}

fn contains_variable(source: &str) -> bool {
    variable_offset(source, 0).is_some()
}
fn variable_offset(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let (mut quote, mut escaped) = (0, false);
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
            continue;
        }
        if matches!(b, b'\'' | b'"') {
            quote = b;
            continue;
        }
        // Function names are ASCII case-insensitive, but a suffix of a
        // longer identifier (or a dimension, hash or at-keyword) is not var().
        if bytes
            .get(i..i + 4)
            .is_some_and(|name| name.eq_ignore_ascii_case(b"var("))
            && (i == 0
                || !(bytes[i - 1].is_ascii_alphanumeric()
                    || matches!(bytes[i - 1], b'-' | b'_' | b'#' | b'@')
                    || bytes[i - 1] >= 128))
        {
            return Some(i);
        }
    }
    None
}
fn substitute_variables(
    source: &str,
    depth: usize,
    lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    if depth > 32 || source.len() > 65536 {
        return None;
    }
    let mut result = String::new();
    let mut offset = 0;
    while let Some(start) = variable_offset(source, offset) {
        result.push_str(&source[offset..start]);
        let end = matching_close(source, start + 3, b'(', b')')?;
        let arguments = &source[start + 4..end];
        let comma = separators(arguments, b',').into_iter().next();
        let name = without_comments(&arguments[..comma.unwrap_or(arguments.len())]);
        let name = trim_boundary(&name);
        if !name.starts_with("--") {
            return None;
        }
        let replacement = lookup(name).or_else(|| {
            comma.and_then(|i| substitute_variables(&arguments[i + 1..], depth + 1, lookup))
        })?;
        result.push_str("/**/");
        result.push_str(&replacement);
        result.push_str("/**/");
        if result.len() > 65536 {
            return None;
        }
        offset = end + 1;
    }
    result.push_str(&source[offset..]);
    (result.len() <= 65536).then_some(result)
}
fn resolve_variables(
    source: &str,
    custom: &HashMap<Arc<str>, Arc<str>>,
    depth: usize,
) -> Option<String> {
    substitute_variables(source, depth, &mut |name| {
        custom.get(name).map(|value| value.to_string())
    })
}
fn compute_custom(raw: HashMap<Arc<str>, Arc<str>>) -> HashMap<Arc<str>, Arc<str>> {
    use std::collections::HashSet;
    // Dependencies inside a fallback participate in cycles even when the first
    // argument currently resolves. Resolve the graph before doing substitution.
    fn dependencies(value: &str, depth: usize) -> Vec<Arc<str>> {
        if depth > 32 {
            return vec![];
        }
        let mut names = vec![];
        let mut offset = 0;
        while let Some(start) = variable_offset(value, offset) {
            let Some(end) = matching_close(value, start + 3, b'(', b')') else {
                break;
            };
            let args = &value[start + 4..end];
            let comma = separators(args, b',').into_iter().next();
            let name = without_comments(&args[..comma.unwrap_or(args.len())]);
            let name = trim_boundary(&name);
            if name.starts_with("--") {
                names.push(Arc::from(name));
            }
            if let Some(comma) = comma {
                names.extend(dependencies(&args[comma + 1..], depth + 1));
            }
            offset = end + 1;
        }
        names
    }
    fn cycles(
        name: &Arc<str>,
        graph: &HashMap<Arc<str>, Vec<Arc<str>>>,
        visited: &mut HashSet<Arc<str>>,
        stack: &mut Vec<Arc<str>>,
        invalid: &mut HashSet<Arc<str>>,
    ) {
        if let Some(index) = stack.iter().position(|n| n == name) {
            invalid.extend(stack[index..].iter().cloned());
            return;
        }
        if visited.contains(name) {
            return;
        }
        if stack.len() >= 32 {
            invalid.insert(name.clone());
            return;
        }
        stack.push(name.clone());
        if let Some(edges) = graph.get(name) {
            for edge in edges {
                cycles(edge, graph, visited, stack, invalid);
            }
        }
        stack.pop();
        visited.insert(name.clone());
    }
    fn resolve_name(
        name: &str,
        raw: &HashMap<Arc<str>, Arc<str>>,
        cache: &mut HashMap<Arc<str>, Arc<str>>,
        invalid: &mut HashSet<Arc<str>>,
        stack: &mut Vec<Arc<str>>,
    ) -> Option<Arc<str>> {
        if invalid.contains(name) {
            return None;
        }
        if let Some(v) = cache.get(name) {
            return Some(v.clone());
        }
        if let Some(index) = stack.iter().position(|n| n.as_ref() == name) {
            invalid.extend(stack[index..].iter().cloned());
            return None;
        }
        if stack.len() > 32 {
            return None;
        }
        let value = raw.get(name)?;
        if !contains_variable(value) {
            cache.insert(name.into(), value.clone());
            return Some(value.clone());
        }
        stack.push(name.into());
        let resolved = substitute_variables(value, stack.len(), &mut |n| {
            resolve_name(n, raw, cache, invalid, stack).map(|value| value.to_string())
        });
        stack.pop();
        if invalid.contains(name) {
            return None;
        }
        let resolved = resolved?;
        // Substitution to the sole keyword `initial` yields the guaranteed-invalid
        // value for an unregistered custom property, not a token to pass to callers.
        if trim_boundary(&without_comments(&resolved)).eq_ignore_ascii_case("initial") {
            invalid.insert(name.into());
            return None;
        }
        let resolved = Arc::<str>::from(resolved);
        cache.insert(name.into(), resolved.clone());
        Some(resolved)
    }
    let mut cache = HashMap::new();
    let mut invalid = HashSet::new();
    let graph: HashMap<_, _> = raw
        .iter()
        .map(|(name, value)| (name.clone(), dependencies(value, 0)))
        .collect();
    let mut visited = HashSet::new();
    for name in graph.keys() {
        cycles(name, &graph, &mut visited, &mut vec![], &mut invalid);
    }
    let mut names: Vec<_> = raw.keys().collect();
    names.sort();
    for name in names {
        resolve_name(name, &raw, &mut cache, &mut invalid, &mut vec![]);
    }
    cache.retain(|name, _| !invalid.contains(name));
    cache
}
