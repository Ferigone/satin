use super::*;
use std::path::PathBuf;

struct Catalog {
    database: fontdb::Database,
    families: HashMap<String, Vec<fontdb::ID>>,
    names: HashMap<String, fontdb::ID>,
    loaded: Mutex<LiveFaces>,
}

#[derive(Default)]
struct LiveFaces {
    entries: HashMap<fontdb::ID, Weak<FaceData>>,
    files: HashMap<PathBuf, Weak<[u8]>>,
    invalid: HashSet<fontdb::ID>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let mut families: HashMap<String, Vec<fontdb::ID>> = HashMap::new();
        let mut names = HashMap::new();
        for info in database.faces() {
            for (family, _) in &info.families {
                let ids = families.entry(family.to_lowercase()).or_default();
                if ids.last() != Some(&info.id) {
                    ids.push(info.id);
                }
            }
            names
                .entry(info.post_script_name.to_lowercase())
                .or_insert(info.id);
        }
        Catalog {
            database,
            families,
            names,
            loaded: Mutex::default(),
        }
    })
}

fn face_style(style: fontdb::Style) -> FontStyle {
    match style {
        fontdb::Style::Normal => FontStyle::Normal,
        fontdb::Style::Italic => FontStyle::Italic,
        fontdb::Style::Oblique => FontStyle::Oblique,
    }
}

pub(super) fn style_score(wanted: FontStyle, actual: FontStyle) -> u8 {
    match (wanted, actual) {
        (a, b) if a == b => 0,
        (FontStyle::Italic, FontStyle::Oblique)
        | (FontStyle::Oblique, FontStyle::Italic)
        | (FontStyle::Normal, FontStyle::Oblique) => 1,
        _ => 2,
    }
}

/// CSS Fonts matching checks a preferred side of the requested weight before
/// the other side. Requests from 400 through 500 try up to 500 first.
pub(super) fn weight_score(wanted: u16, range: [u16; 2]) -> (u8, u16) {
    let actual = wanted.clamp(range[0], range[1].max(range[0]));
    if wanted == actual {
        return (0, 0);
    }
    match wanted {
        0..400 => (u8::from(actual > wanted), actual.abs_diff(wanted)),
        400..=500 if actual > wanted && actual <= 500 => (0, actual - wanted),
        400..=500 if actual < wanted => (1, wanted - actual),
        400..=500 => (2, actual - wanted),
        _ => (u8::from(actual < wanted), actual.abs_diff(wanted)),
    }
}

fn named_id(name: &str, wanted: u16, style: FontStyle) -> Option<fontdb::ID> {
    let c = catalog();
    c.families
        .get(&name.to_lowercase())?
        .iter()
        .copied()
        .min_by_key(|id| {
            let face = c.database.face(*id).expect("catalog ID");
            (
                style_score(style, face_style(face.style)),
                weight_score(wanted, [face.weight.0; 2]),
            )
        })
}

pub(super) fn named(name: &str, weight: u16, style: FontStyle) -> Option<fontdb::ID> {
    named_id(name, weight, style)
}

pub(super) fn generic(family: GenericFamily, weight: u16, style: FontStyle) -> Option<fontdb::ID> {
    let names: &[&str] = match family {
        GenericFamily::Serif | GenericFamily::UiSerif => &[
            "Times New Roman",
            "DejaVu Serif",
            "Noto Serif",
            "Liberation Serif",
        ],
        GenericFamily::SansSerif => &["Arial", "DejaVu Sans", "Noto Sans", "Liberation Sans"],
        GenericFamily::SystemUi | GenericFamily::UiSansSerif | GenericFamily::UiRounded => {
            &["Segoe UI", "DejaVu Sans", "Noto Sans", "Arial"]
        }
        GenericFamily::Monospace | GenericFamily::UiMonospace => &[
            "Consolas",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Courier New",
        ],
        GenericFamily::Cursive => &["Comic Sans MS", "URW Chancery L", "Segoe Script"],
        GenericFamily::Fantasy => &["Impact", "Cooper Black", "DejaVu Sans"],
        GenericFamily::Emoji => &["Segoe UI Emoji", "Noto Color Emoji", "Segoe UI Symbol"],
        GenericFamily::Math => &["Cambria Math", "STIX Two Math", "DejaVu Math TeX Gyre"],
        GenericFamily::FangSong => &["FangSong", "STFangsong", "Noto Serif CJK SC"],
    };
    names.iter().find_map(|name| named_id(name, weight, style))
}

pub(super) fn first_faces() -> impl Iterator<Item = fontdb::ID> {
    catalog().database.faces().take(64).map(|face| face.id)
}

pub(super) fn load(id: fontdb::ID) -> Result<FontFace, FontError> {
    let c = catalog();
    let mut cache = c.loaded.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(data) = cache.entries.get(&id).and_then(Weak::upgrade) {
        return Ok(FontFace { data });
    }
    if cache.invalid.contains(&id) {
        return Err(FontError("Invalid system font".into()));
    }
    cache.entries.retain(|_, face| face.strong_count() != 0);
    cache.files.retain(|_, bytes| bytes.strong_count() != 0);
    if cache.entries.len() >= 128 {
        return Err(FontError("Live system face count limit exceeded".into()));
    }
    let mut existing_files = HashSet::new();
    let live_bytes: usize = cache
        .entries
        .values()
        .filter_map(Weak::upgrade)
        .map(|face| {
            let bytes = if existing_files.insert(face.bytes.as_ptr()) {
                face.bytes.len()
            } else {
                0
            };
            bytes
                + face.costs.len() * std::mem::size_of::<u32>()
                + face.bounds.len() * std::mem::size_of::<[i16; 4]>()
        })
        .sum();
    let info = c
        .database
        .face(id)
        .ok_or_else(|| FontError("Unknown system font".into()))?;
    let path = match &info.source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => Some(path),
        _ => None,
    };
    let mut quota_exceeded = false;
    let loaded = if let Some(bytes) = path
        .and_then(|path| cache.files.get(path))
        .and_then(Weak::upgrade)
    {
        FontFace::from_sfnt(bytes, info.index, false)
    } else {
        c.database
            .with_face_data(id, |bytes, index| {
                if bytes.len() > MAX_SYSTEM_FONT_BYTES {
                    return Err(FontError("System font byte limit exceeded".into()));
                }
                if live_bytes.saturating_add(bytes.len()) > 64 * 1024 * 1024 {
                    quota_exceeded = true;
                    return Err(FontError("Live system font byte limit exceeded".into()));
                }
                FontFace::from_sfnt(Arc::from(bytes), index, false)
            })
            .unwrap_or_else(|| Err(FontError("Cannot read system font".into())))
    };
    let face = match loaded {
        Ok(face) => face,
        Err(error) => {
            if !quota_exceeded && cache.invalid.len() < 256 {
                cache.invalid.insert(id);
            }
            return Err(error);
        }
    };
    let profile = face.data.costs.len() * std::mem::size_of::<u32>()
        + face.data.bounds.len() * std::mem::size_of::<[i16; 4]>();
    let new_bytes = if existing_files.contains(&face.data.bytes.as_ptr()) {
        0
    } else {
        face.decoded_bytes()
    };
    if live_bytes.saturating_add(new_bytes).saturating_add(profile) > 64 * 1024 * 1024 {
        return Err(FontError("Live system font byte limit exceeded".into()));
    }
    if let Some(path) = path {
        cache
            .files
            .insert(path.clone(), Arc::downgrade(&face.data.bytes));
    }
    cache.entries.insert(id, Arc::downgrade(&face.data));
    Ok(face)
}

pub(super) fn local(name: &str) -> Result<Option<FontFace>, FontError> {
    if name.is_empty() || name.len() > 256 {
        return Ok(None);
    }
    let c = catalog();
    let normalized = name.to_lowercase();
    if let Some(id) = c.names.get(&normalized) {
        return load(*id).map(Some);
    }
    // Full face names often append a subfamily to the family. Only probe
    // matching family prefixes so local() cannot force a full disk scan.
    let mut candidates = Vec::new();
    for (index, _) in normalized
        .match_indices(' ')
        .chain(std::iter::once((normalized.len(), "")))
    {
        if let Some(ids) = c.families.get(&normalized[..index]) {
            for &id in ids {
                if candidates.len() < 32 && !candidates.contains(&id) {
                    candidates.push(id);
                }
            }
        }
    }
    for id in candidates {
        let matches = c
            .database
            .with_face_data(id, |data, index| {
                ttf_parser::Face::parse(data, index)
                    .ok()
                    .is_some_and(|face| {
                        face.names()
                            .into_iter()
                            .filter(|name| name.name_id == 4)
                            .filter_map(|name| name.to_string())
                            .any(|name| name.to_lowercase() == normalized)
                    })
            })
            .unwrap_or(false);
        if matches {
            return load(id).map(Some);
        }
    }
    Ok(None)
}
