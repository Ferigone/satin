mod catalog;
mod codec;
mod raster;
mod shaping;

pub(crate) use raster::RasterFailure;
use rustybuzz::ttf_parser;
pub(crate) use shaping::CanvasShaping;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

pub const MAX_FONT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DECODED_FONT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FONT_SET_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_FONT_FACES: usize = 32;
pub(crate) const MAX_SHAPE_INPUT_BYTES: usize = 32 * 1024;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
    Oblique,
}
impl FontStyle {
    pub fn as_css(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Italic => "italic",
            Self::Oblique => "oblique",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GenericFamily {
    Serif,
    SansSerif,
    Monospace,
    Cursive,
    Fantasy,
    SystemUi,
    UiSerif,
    UiSansSerif,
    UiMonospace,
    UiRounded,
    Emoji,
    Math,
    FangSong,
}
impl GenericFamily {
    pub fn as_css(self) -> &'static str {
        match self {
            Self::Serif => "serif",
            Self::SansSerif => "sans-serif",
            Self::Monospace => "monospace",
            Self::Cursive => "cursive",
            Self::Fantasy => "fantasy",
            Self::SystemUi => "system-ui",
            Self::UiSerif => "ui-serif",
            Self::UiSansSerif => "ui-sans-serif",
            Self::UiMonospace => "ui-monospace",
            Self::UiRounded => "ui-rounded",
            Self::Emoji => "emoji",
            Self::Math => "math",
            Self::FangSong => "fangsong",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Named(Arc<str>),
    Generic(GenericFamily),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontFormat {
    TrueType,
    OpenType,
    Woff,
    Woff2,
    Unsupported,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontSource {
    Local(Arc<str>),
    Url {
        url: Arc<str>,
        format: Option<FontFormat>,
    },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontDisplay {
    #[default]
    Auto,
    Block,
    Swap,
    Fallback,
    Optional,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnicodeRange {
    pub start: u32,
    pub end: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFaceRule {
    pub family: Arc<str>,
    pub weight: [u16; 2],
    pub style: FontStyle,
    pub sources: Vec<FontSource>,
    pub unicode_ranges: Vec<UnicodeRange>,
    pub display: FontDisplay,
}
#[derive(Clone, Debug)]
pub struct FontError(pub String);
impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FontError {}
#[derive(Clone)]
pub struct FontFace {
    data: Arc<FaceData>,
}
struct FaceData {
    id: u64,
    bytes: Arc<[u8]>,
    index: u32,
    family: Arc<str>,
    weight: u16,
    style: FontStyle,
    costs: Arc<[u32]>,
    bounds: Arc<[[i16; 4]]>,
    units_per_em: u16,
    plan_cost: usize,
    web: bool,
    axes: u8,
}
impl fmt::Debug for FontFace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontFace")
            .field("id", &self.data.id)
            .field("family", &self.data.family)
            .field("bytes", &self.data.bytes.len())
            .finish()
    }
}
impl FontFace {
    pub fn id(&self) -> u64 {
        self.data.id
    }
    pub fn decoded_bytes(&self) -> usize {
        self.data.bytes.len()
    }
    pub fn family(&self) -> &str {
        &self.data.family
    }
    pub fn weight(&self) -> u16 {
        self.data.weight
    }
    pub fn style(&self) -> FontStyle {
        self.data.style
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, FontError> {
        let decoded = codec::decode(bytes)?;
        Self::from_sfnt(decoded.into(), 0, true)
    }
    pub fn local(name: &str) -> Result<Option<Self>, FontError> {
        catalog::local(name)
    }
    fn from_sfnt(bytes: Arc<[u8]>, index: u32, web: bool) -> Result<Self, FontError> {
        let profile = codec::outline_profile(&bytes, index, web)?;
        let face = rustybuzz::ttf_parser::Face::parse(&bytes, index)
            .map_err(|_| FontError("Invalid font face".into()))?;
        let family = face
            .names()
            .into_iter()
            .filter(|n| matches!(n.name_id, 1 | 16))
            .find_map(|n| n.to_string())
            .unwrap_or_else(|| "Unnamed".into());
        let weight = face.weight().to_number();
        let style = if face.is_italic() {
            FontStyle::Italic
        } else if face.is_oblique() {
            FontStyle::Oblique
        } else {
            FontStyle::Normal
        };
        let axes = face.variation_axes().into_iter().fold(0, |mask, axis| {
            mask | match &axis.tag.to_bytes() {
                b"wght" => 1,
                b"ital" => 2,
                b"slnt" => 4,
                _ => 0,
            }
        });
        let units_per_em = face.units_per_em();
        let plan_cost = 2048usize.saturating_add(
            [b"GSUB", b"GPOS", b"mort", b"morx"]
                .into_iter()
                .map(|tag| {
                    face.raw_face()
                        .table(ttf_parser::Tag::from_bytes(tag))
                        .map_or(0, |table| table.len() * 4)
                })
                .sum(),
        );
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            data: Arc::new(FaceData {
                id: NEXT.fetch_add(1, Ordering::Relaxed),
                bytes,
                index,
                family: family.into(),
                weight,
                style,
                costs: profile.costs.into(),
                bounds: profile.bounds.into(),
                units_per_em,
                plan_cost,
                web,
                axes,
            }),
        })
    }
    pub fn downgrade(&self) -> WeakFontFace {
        WeakFontFace(Arc::downgrade(&self.data))
    }
    pub fn is_web(&self) -> bool {
        self.data.web
    }
}
pub struct WeakFontFace(std::sync::Weak<FaceData>);
impl WeakFontFace {
    pub fn upgrade(&self) -> Option<FontFace> {
        self.0.upgrade().map(|data| FontFace { data })
    }
}
#[derive(Clone, Debug)]
pub struct FontFaceEntry {
    pub rule: FontFaceRule,
    pub face: FontFace,
}
#[derive(Clone, Debug, Default)]
pub struct FontSet {
    entries: Arc<[FontFaceEntry]>,
    revision: u64,
    bytes: usize,
}
impl FontSet {
    pub fn new(mut entries: Vec<FontFaceEntry>) -> Result<Self, FontError> {
        if entries.len() > MAX_FONT_FACES {
            return Err(FontError("Font face count limit exceeded".into()));
        }
        for entry in &mut entries {
            let [lo, hi] = entry.rule.weight;
            if entry.rule.unicode_ranges.len() > 256
                || entry.rule.family.is_empty()
                || entry.rule.family.len() > 256
                || entry.rule.sources.len() > 16
                || lo < 1
                || hi > 1000
                || lo > hi
            {
                return Err(FontError("Font descriptor limit exceeded".into()));
            }
            entry
                .rule
                .unicode_ranges
                .sort_unstable_by_key(|range| (range.start, range.end));
            let mut merged: Vec<UnicodeRange> = Vec::new();
            for range in &entry.rule.unicode_ranges {
                if range.start > range.end || range.end > 0x10ffff {
                    continue;
                }
                if let Some(last) = merged.last_mut().filter(|last| range.start <= last.end + 1) {
                    last.end = last.end.max(range.end);
                } else {
                    merged.push(*range);
                }
            }
            entry.rule.unicode_ranges = merged;
        }
        let mut seen = std::collections::HashSet::new();
        let bytes = entries
            .iter()
            .filter(|entry| seen.insert(entry.face.id()))
            .map(|entry| entry.face.decoded_bytes())
            .try_fold(0usize, |sum, n| sum.checked_add(n))
            .filter(|n| *n <= MAX_FONT_SET_BYTES)
            .ok_or_else(|| FontError("Decoded font set byte limit exceeded".into()))?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            entries: entries.into(),
            revision: NEXT.fetch_add(1, Ordering::Relaxed),
            bytes,
        })
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn decoded_bytes(&self) -> usize {
        self.bytes
    }
    pub fn entries(&self) -> &[FontFaceEntry] {
        &self.entries
    }
}
const MAX_SYSTEM_FONT_BYTES: usize = 24 * 1024 * 1024;
const SHAPE_CACHE_BYTES: usize = 4 * 1024 * 1024;
const SHAPE_CACHE_ENTRIES: usize = 4096;
const GLYPH_CACHE_BYTES: usize = 16 * 1024 * 1024;
const GLYPH_CACHE_ENTRIES: usize = 16384;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FontRequest {
    pub families: Arc<[FontFamily]>,
    pub weight: u16,
    pub style: FontStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Coordinates {
    weight: u16,
    italic: bool,
    slant: i16,
}

impl Coordinates {
    fn axes(self) -> impl Iterator<Item = ([u8; 4], f32)> {
        [
            (self.weight != 0).then_some((*b"wght", f32::from(self.weight))),
            self.italic.then_some((*b"ital", 1.)),
            (self.slant != 0).then_some((*b"slnt", f32::from(self.slant))),
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Clone, Debug)]
pub struct RunFace {
    face: FontFace,
    coords: Coordinates,
}

impl RunFace {
    pub fn face(&self) -> &FontFace {
        &self.face
    }
    pub fn variation_weight(&self) -> Option<u16> {
        (self.coords.weight != 0).then_some(self.coords.weight)
    }
    pub(crate) fn outline_face(&self) -> rustybuzz::Face<'_> {
        shaping::face(self)
    }
    pub(crate) fn outline_cost(&self, id: u16) -> usize {
        self.face
            .data
            .costs
            .get(usize::from(id))
            .copied()
            .unwrap_or(0) as usize
    }
    pub(crate) fn ink_bounds(&self, id: u16, size: f32) -> Option<[f32; 4]> {
        if self.coords != Coordinates::default() {
            return None;
        }
        let &[left, bottom, right, top] = self.face.data.bounds.get(usize::from(id))?;
        let size = (size * 16.).round().clamp(1., 8192.) / 16.;
        let scale = size / f32::from(self.face.data.units_per_em);
        Some([
            f32::from(left) * scale - 1.,
            -f32::from(top) * scale - 1.,
            f32::from(right) * scale + 1.,
            -f32::from(bottom) * scale + 1.,
        ])
    }
    fn key(&self) -> (u64, Coordinates) {
        (self.face.id(), self.coords)
    }
    fn new(face: FontFace, request: &FontRequest, weight: u16) -> Self {
        let axes = face.data.axes;
        let coords = Coordinates {
            weight: if axes & 1 != 0 { weight } else { 0 },
            italic: axes & 2 != 0 && request.style == FontStyle::Italic,
            slant: if axes & 4 != 0
                && request.style != FontStyle::Normal
                && (axes & 2 == 0 || request.style == FontStyle::Oblique)
            {
                -14
            } else {
                0
            },
        };
        Self { face, coords }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub id: u16,
    pub face_slot: u16,
    pub x: f32,
    pub y: f32,
}
#[derive(Clone, Debug)]
pub struct Run {
    pub glyphs: Vec<Glyph>,
    pub advance: f32,
    pub size: f32,
    pub ascent: f32,
    pub descent: f32,
    faces: Arc<[RunFace]>,
}
impl Run {
    pub fn faces(&self) -> &[RunFace] {
        &self.faces
    }
    pub fn face_for(&self, glyph: &Glyph) -> &RunFace {
        &self.faces[usize::from(glyph.face_slot)]
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ShapeCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub payload_bytes: usize,
}

pub struct Fonts {
    default: FontFace,
    system_hot: RefCell<VecDeque<(fontdb::ID, FontFace)>>,
    system_matches: RefCell<HashMap<(FontFamily, u16, FontStyle), Option<fontdb::ID>>>,
    shapes: RefCell<shaping::ShapeCache>,
    plans: RefCell<shaping::PlanCache>,
    raster: raster::RasterCache,
}

impl Fonts {
    pub fn new() -> Result<Self, String> {
        let default = catalog::generic(GenericFamily::SystemUi, 400, FontStyle::Normal)
            .into_iter()
            .chain(catalog::first_faces())
            .find_map(|id| catalog::load(id).ok())
            .ok_or_else(|| "No usable system font was found".to_owned())?;
        Ok(Self {
            default,
            system_hot: RefCell::default(),
            system_matches: RefCell::default(),
            shapes: RefCell::default(),
            plans: RefCell::default(),
            raster: raster::RasterCache::default(),
        })
    }

    fn system_match(&self, family: &FontFamily, request: &FontRequest) -> Option<fontdb::ID> {
        let key = (family.clone(), request.weight, request.style);
        let mut cache = self.system_matches.borrow_mut();
        if let Some(found) = cache.get(&key) {
            return *found;
        }
        let found = match family {
            FontFamily::Named(name) => catalog::named(name, request.weight, request.style),
            FontFamily::Generic(generic) => {
                catalog::generic(*generic, request.weight, request.style)
            }
        };
        if cache.len() >= 1024 {
            cache.clear();
        }
        cache.insert(key, found);
        found
    }

    fn system_face(&self, id: fontdb::ID) -> Option<FontFace> {
        let mut hot = self.system_hot.borrow_mut();
        if let Some(index) = hot.iter().position(|(candidate, _)| *candidate == id) {
            let entry = hot.remove(index).expect("hot font index");
            let face = entry.1.clone();
            hot.push_back(entry);
            return Some(face);
        }
        let face = catalog::load(id).ok()?;
        let mut bytes: usize = hot.iter().map(|(_, face)| face.decoded_bytes()).sum();
        while hot.len() >= 16 || bytes.saturating_add(face.decoded_bytes()) > 32 * 1024 * 1024 {
            let Some((_, old)) = hot.pop_front() else {
                break;
            };
            bytes -= old.decoded_bytes();
        }
        hot.push_back((id, face.clone()));
        Some(face)
    }

    pub fn shape(&self, text: &str, size: f32, bold: bool, italic: bool, mono: bool) -> Run {
        self.shape_text(text, size, &ui_request(bold, italic, mono), empty_set())
    }

    pub fn break_word(
        &self,
        text: &str,
        size: f32,
        bold: bool,
        italic: bool,
        mono: bool,
        width: f32,
    ) -> Vec<Run> {
        self.break_word_text(
            text,
            size,
            &ui_request(bold, italic, mono),
            empty_set(),
            width,
        )
    }

    pub fn shape_cache_stats(&self) -> ShapeCacheStats {
        self.shapes.borrow().stats
    }

    pub fn cache_bytes(&self) -> usize {
        self.raster.bytes()
    }
}

fn empty_set() -> &'static FontSet {
    static EMPTY: OnceLock<FontSet> = OnceLock::new();
    EMPTY.get_or_init(FontSet::default)
}

fn ui_request(bold: bool, italic: bool, mono: bool) -> FontRequest {
    static SYSTEM: OnceLock<Arc<[FontFamily]>> = OnceLock::new();
    static MONO: OnceLock<Arc<[FontFamily]>> = OnceLock::new();
    let families = if mono {
        MONO.get_or_init(|| [FontFamily::Generic(GenericFamily::Monospace)].into())
    } else {
        SYSTEM.get_or_init(|| [FontFamily::Generic(GenericFamily::SystemUi)].into())
    };
    FontRequest {
        families: families.clone(),
        weight: if bold { 700 } else { 400 },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
    }
}
