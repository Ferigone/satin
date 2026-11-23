//! CSS font shorthand parsing for Canvas, with line-height discarded.
use super::*;
use crate::fonts::FontRequest;

#[derive(Clone, Copy)]
pub(crate) struct Basis {
    pub size: f32,
    pub weight: u16,
    pub root_size: f32,
    pub viewport: [f32; 2],
}

impl Default for Basis {
    fn default() -> Self {
        Self {
            size: 10.,
            weight: 400,
            root_size: 16.,
            viewport: [0.; 2],
        }
    }
}

#[derive(Clone)]
pub(crate) struct Font {
    pub request: FontRequest,
    pub size: f32,
    pub serialized: Arc<str>,
    pub small_caps: bool,
    pub stretch: f64,
}

impl Default for Font {
    fn default() -> Self {
        static FONT: OnceLock<Font> = OnceLock::new();
        FONT.get_or_init(|| Font {
            request: FontRequest {
                families: [FontFamily::Generic(GenericFamily::SansSerif)].into(),
                weight: 400,
                style: FontStyle::Normal,
            },
            size: 10.,
            serialized: "10px sans-serif".into(),
            small_caps: false,
            stretch: 1.,
        })
        .clone()
    }
}

pub(crate) struct Shorthand {
    size: SpecifiedLength,
    weight: Value,
    style: FontStyle,
    families: Arc<[FontFamily]>,
    small_caps: bool,
    stretch: f64,
}

impl Shorthand {
    pub(crate) fn uses_font_basis(&self) -> bool {
        !matches!(self.size, SpecifiedLength::Plain(Length::Px(_)))
            || matches!(self.weight, Value::RelativeWeight(_))
    }

    pub(crate) fn resolve(self, basis: Basis) -> Option<Font> {
        let size = self
            .size
            .computed(basis.root_size, basis.viewport, basis.size)
            .resolve(basis.size, basis.size)?;
        if !size.is_finite() {
            return None;
        }
        let size = size.max(0.);
        let inherited = Style {
            font_weight: basis.weight,
            ..Style::default()
        };
        let mut computed = Style {
            font_family: self.families,
            font_style: self.style,
            ..Style::default()
        };
        apply_value(
            &mut computed,
            &inherited,
            Property::Bold,
            &self.weight,
            basis.root_size,
            basis.viewport,
        );
        let mut serialized = String::new();
        if self.style != FontStyle::Normal {
            serialized.push_str(self.style.as_css());
            serialized.push(' ');
        }
        match computed.font_weight {
            400 => {}
            700 => serialized.push_str("bold "),
            weight => serialized.push_str(&format!("{weight} ")),
        }
        if self.small_caps {
            serialized.push_str("small-caps ");
        }
        serialized.push_str(&format!("{size}px {}", computed.font_family_css()));
        Some(Font {
            request: computed.font_request(),
            size,
            serialized: serialized.into(),
            small_caps: self.small_caps,
            stretch: self.stretch,
        })
    }
}

pub(crate) fn stretch_factor(value: &str) -> Option<f64> {
    Some(match value {
        "ultra-condensed" => 0.5,
        "extra-condensed" => 0.625,
        "condensed" => 0.75,
        "semi-condensed" => 0.875,
        "normal" => 1.,
        "semi-expanded" => 1.125,
        "expanded" => 1.25,
        "extra-expanded" => 1.5,
        "ultra-expanded" => 2.,
        _ => return None,
    })
}

pub(crate) fn parse(source: &str) -> Option<Shorthand> {
    let source = declaration_tokens(source);
    let system = trim_boundary(&source).to_ascii_lowercase();
    if matches!(
        system.as_str(),
        "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
    ) {
        // These are the browser's UI font defaults. Ordinary named family
        // fallback still applies if a Windows face is unavailable.
        let (size, family) = if cfg!(target_os = "windows") {
            if matches!(system.as_str(), "menu" | "small-caption" | "status-bar") {
                (12., FontFamily::Named("Segoe UI".into()))
            } else {
                (16., FontFamily::Named("Arial".into()))
            }
        } else {
            (13., FontFamily::Generic(GenericFamily::SystemUi))
        };
        return Some(Shorthand {
            size: Length::Px(size).into(),
            weight: Value::FontWeight(400),
            style: FontStyle::Normal,
            families: [family].into(),
            small_caps: false,
            stretch: 1.,
        });
    }
    let parts = split_top(&source, b'/');
    let (prefix, line_height_and_family) = match parts.as_slice() {
        [prefix] => (*prefix, None),
        [prefix, rest] => (*prefix, Some(whitespace_tokens(rest))),
        _ => return None,
    };
    let mut style = None;
    let mut weight = None;
    let mut small_caps = false;
    let mut stretch = None;
    let mut normal_count = 0;
    let mut tokens = whitespace_tokens(prefix).into_iter();
    let size = loop {
        let token = tokens.next()?;
        let lower = token.to_ascii_lowercase();
        if let Some(Value::Length(length)) = parse_value(Property::FontSize, &lower) {
            break length;
        }
        if let Some(ex) = lower
            .strip_suffix("ex")
            .and_then(number)
            .filter(|v| *v >= 0.)
        {
            // The CSS length parser has no x-height unit yet.
            break Length::Em(ex * 0.5).into();
        }
        if lower == "normal" {
            normal_count += 1;
        } else if let Some(value) = parse_font_style(&lower).filter(|_| style.is_none()) {
            style = Some(value);
        } else if let Some(value) = parse_value(Property::Bold, &lower).filter(|_| weight.is_none())
        {
            weight = Some(value);
        } else if lower == "small-caps" && !small_caps {
            small_caps = true;
        } else {
            stretch = Some(stretch_factor(&lower).filter(|_| stretch.is_none())?);
        }
        if normal_count
            + usize::from(style.is_some())
            + usize::from(weight.is_some())
            + usize::from(small_caps)
            + usize::from(stretch.is_some())
            > 4
        {
            return None;
        }
    };
    let family = match line_height_and_family {
        None => tokens.collect::<Vec<_>>().join(" "),
        Some(rest) => {
            if tokens.next().is_some() {
                return None;
            }
            let (line_height, family) = rest.split_first()?;
            parse_value(Property::LineHeight, line_height)?;
            family.join(" ")
        }
    };
    Some(Shorthand {
        size,
        weight: weight.unwrap_or(Value::FontWeight(400)),
        style: style.unwrap_or_default(),
        families: font_families(&family)?.into(),
        small_caps,
        stretch: stretch.unwrap_or(1.),
    })
}
