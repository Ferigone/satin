use super::{
    GridBreadth, GridTrack, Length, SpecifiedLength, identifier, matching_close,
    nonnegative_length, number, split_top, unescape_string,
};
use std::{
    collections::BTreeMap,
    ops::{Deref, Range},
    sync::Arc,
};

pub(crate) const GRID_TRACK_LIMIT: usize = 128;
const DECLARATION_LIMIT: usize = 64 * 1024;
const NAME_LIMIT: usize = 256;
const LINE_NAME_LIMIT: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum GridLine {
    #[default]
    Auto,
    Line(i16),
    Span(u16),
    Named {
        name: Arc<str>,
        occurrence: Option<i16>,
    },
    NamedSpan {
        name: Arc<str>,
        count: u16,
    },
}

impl GridLine {
    pub(super) fn shorthand_end(&self) -> Self {
        if matches!(
            self,
            Self::Named {
                occurrence: None,
                ..
            }
        ) {
            self.clone()
        } else {
            Self::Auto
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GridAutoRepeat {
    pub(crate) range: Range<usize>,
    pub(crate) fit: bool,
    pub(crate) first_names: Vec<Arc<str>>,
    pub(crate) last_names: Vec<Arc<str>>,
}

/// A template keeps one copy of its automatic repeat fragment. At layout time
/// the copy count is derived from the grid container's size.
#[derive(Clone, Debug)]
pub struct GridTemplate<L = Length> {
    pub(crate) tracks: Vec<GridTrack<L>>,
    pub(crate) names: Arc<BTreeMap<usize, Vec<Arc<str>>>>,
    pub(crate) repeat: Option<Arc<GridAutoRepeat>>,
}

impl<L> Default for GridTemplate<L> {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            names: Arc::default(),
            repeat: None,
        }
    }
}

impl<L> Deref for GridTemplate<L> {
    type Target = [GridTrack<L>];
    fn deref(&self) -> &Self::Target {
        &self.tracks
    }
}

#[derive(Clone, Debug)]
pub(super) enum SpecifiedTracks {
    Shared(Arc<Vec<GridTrack>>),
    Contextual(Arc<[GridTrack<SpecifiedLength>]>),
}

impl SpecifiedTracks {
    pub(super) fn computed(
        &self,
        root_font: f32,
        viewport: [f32; 2],
        font: f32,
    ) -> Arc<Vec<GridTrack>> {
        match self {
            Self::Shared(tracks) => tracks.clone(),
            Self::Contextual(tracks) => Arc::new(compute_tracks(tracks, root_font, viewport, font)),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum SpecifiedTemplate {
    Shared(Arc<GridTemplate>),
    Contextual(Arc<GridTemplate<SpecifiedLength>>),
}

impl SpecifiedTemplate {
    fn new(template: GridTemplate<SpecifiedLength>) -> Self {
        if tracks_need_context(&template.tracks) {
            Self::Contextual(Arc::new(template))
        } else {
            Self::Shared(compute_template(&template, 16., [0.; 2], 16.))
        }
    }

    pub(super) fn computed(
        &self,
        root_font: f32,
        viewport: [f32; 2],
        font: f32,
    ) -> Arc<GridTemplate> {
        match self {
            Self::Shared(template) => template.clone(),
            Self::Contextual(template) => compute_template(template, root_font, viewport, font),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridArea {
    pub(crate) rows: [usize; 2],
    pub(crate) columns: [usize; 2],
}

#[derive(Clone, Debug, Default)]
pub struct GridAreas {
    dimensions: [usize; 2],
    pub(crate) names: BTreeMap<Arc<str>, GridArea>,
}

impl GridAreas {
    /// Explicit row and column counts created by the area strings.
    pub fn dimensions(&self) -> [usize; 2] {
        self.dimensions
    }
}

fn css_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{c}' | ' ' | '\0')
}

fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn starts_ident(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let valid_escape =
        |next: Option<char>| next.is_some_and(|c| !matches!(c, '\n' | '\r' | '\u{c}'));
    match first {
        '-' => match chars.next() {
            Some('-') => true,
            Some('\\') => valid_escape(chars.next()),
            Some(c) => ident_start(c),
            None => false,
        },
        '\\' => valid_escape(chars.next()),
        c => ident_start(c),
    }
}

fn custom_ident(name: &str) -> bool {
    name.len() <= NAME_LIMIT
        && ![
            "auto",
            "span",
            "initial",
            "inherit",
            "unset",
            "revert",
            "revert-layer",
            "default",
        ]
        .iter()
        .any(|keyword| name.eq_ignore_ascii_case(keyword))
}

pub(super) fn parse_line(source: &str) -> Option<GridLine> {
    if source.len() > NAME_LIMIT * 4 {
        return None;
    }
    let mut source = source.trim_matches(css_space);
    let mut count = None;
    let mut name = None;
    let mut span = false;
    while !source.is_empty() {
        if starts_ident(source) {
            let mut offset = 0;
            let ident = identifier(source, &mut offset)?;
            source = source.get(offset..)?;
            if ident.eq_ignore_ascii_case("auto") {
                return (source.is_empty() && count.is_none() && name.is_none() && !span)
                    .then_some(GridLine::Auto);
            }
            if ident.eq_ignore_ascii_case("span") && !span {
                span = true;
            } else if name.is_none() && custom_ident(&ident) {
                name = Some(Arc::from(ident));
            } else {
                return None;
            }
        } else {
            let end = source.find(css_space).unwrap_or(source.len());
            if count.is_some() {
                return None;
            }
            count = Some(source[..end].parse::<i32>().ok().filter(|n| *n != 0)?);
            source = &source[end..];
        }
        if !source.is_empty() && !source.starts_with(css_space) {
            return None;
        }
        source = source.trim_start_matches(css_space);
    }
    match (span, count, name) {
        (false, Some(count), None) => Some(GridLine::Line(i16::try_from(count).ok()?)),
        (false, count, Some(name)) => Some(GridLine::Named {
            name,
            occurrence: count.map(i16::try_from).transpose().ok()?,
        }),
        (true, Some(count), None) if count > 0 => Some(GridLine::Span(u16::try_from(count).ok()?)),
        (true, count, Some(name)) => Some(GridLine::NamedSpan {
            name,
            count: u16::try_from(count.unwrap_or(1)).ok()?,
        }),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Token<'a> {
    Word(&'a str),
    Names(&'a str),
    String(&'a str),
}

fn tokens(mut source: &str) -> Option<Vec<Token<'_>>> {
    if source.len() > DECLARATION_LIMIT {
        return None;
    }
    let mut tokens = Vec::new();
    loop {
        source = source.trim_start_matches(css_space);
        let Some(first) = source.chars().next() else {
            return Some(tokens);
        };
        let (token, end) = match first {
            '[' => {
                let end = matching_close(source, 0, b'[', b']')?;
                (Token::Names(&source[1..end]), end + 1)
            }
            '\'' | '"' => {
                let mut chars = source.char_indices().skip(1).peekable();
                let end = loop {
                    let (index, c) = chars.next()?;
                    match c {
                        c if c == first => break index,
                        '\n' | '\r' | '\u{c}' => return None,
                        '\\' => {
                            let (_, escaped) = chars.next()?;
                            if escaped == '\r' && chars.peek().is_some_and(|&(_, c)| c == '\n') {
                                chars.next();
                            }
                        }
                        _ => {}
                    }
                };
                (Token::String(&source[1..end]), end + 1)
            }
            ']' | ')' | '/' => return None,
            _ => {
                let mut end = 0;
                while end < source.len() {
                    let c = source[end..].chars().next()?;
                    if css_space(c) || matches!(c, '[' | ']' | ')' | '/' | '\'' | '"') {
                        break;
                    }
                    if c == '(' {
                        end = matching_close(source, end, b'(', b')')? + 1;
                    } else {
                        end += c.len_utf8();
                    }
                }
                (Token::Word(&source[..end]), end)
            }
        };
        if tokens.len() >= GRID_TRACK_LIMIT * 4 + 2 {
            return None;
        }
        tokens.push(token);
        source = &source[end..];
    }
}

fn parse_names(mut source: &str) -> Option<Vec<Arc<str>>> {
    let mut names = Vec::new();
    loop {
        source = source.trim_start_matches(css_space);
        if source.is_empty() {
            break;
        }
        if !starts_ident(source) || names.len() == LINE_NAME_LIMIT {
            return None;
        }
        let mut end = 0;
        let name = identifier(source, &mut end)?;
        if !custom_ident(&name) {
            return None;
        }
        names.push(Arc::from(name));
        source = &source[end..];
        if !source.is_empty() && !source.starts_with(css_space) {
            return None;
        }
    }
    names.sort_unstable();
    names.dedup();
    Some(names)
}

fn function<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let (prefix, tail) = source.split_once('(')?;
    if prefix.eq_ignore_ascii_case(name) {
        tail.strip_suffix(')')
    } else {
        None
    }
}

fn breadth(source: &str) -> Option<GridBreadth<SpecifiedLength>> {
    let lower = source.trim_matches(css_space).to_ascii_lowercase();
    Some(match lower.as_str() {
        "auto" => GridBreadth::Auto,
        "min-content" => GridBreadth::MinContent,
        "max-content" => GridBreadth::MaxContent,
        value if value.ends_with("fr") => {
            let n = number(value.strip_suffix("fr")?)?;
            if n < 0. {
                return None;
            }
            GridBreadth::Flex(n.min(1e6))
        }
        value => GridBreadth::Length(nonnegative_length(value, false)?),
    })
}

fn track(source: &str) -> Option<GridTrack<SpecifiedLength>> {
    if let Some(args) = function(source, "minmax") {
        let parts = split_top(args, b',');
        if let [min, max] = parts.as_slice() {
            let min = breadth(min)?;
            if !matches!(min, GridBreadth::Flex(_)) {
                return Some(GridTrack {
                    min,
                    max: breadth(max)?,
                });
            }
        }
        return None;
    }
    let size = breadth(source)?;
    Some(GridTrack {
        min: if matches!(size, GridBreadth::Flex(_)) {
            GridBreadth::Auto
        } else {
            size
        },
        max: size,
    })
}

fn add_names(template: &mut GridTemplate<SpecifiedLength>, line: usize, incoming: Vec<Arc<str>>) {
    if incoming.is_empty() {
        return;
    }
    let names = Arc::make_mut(&mut template.names).entry(line).or_default();
    names.extend(incoming);
    names.sort_unstable();
    names.dedup();
}

fn name_count(template: &GridTemplate<SpecifiedLength>) -> usize {
    let edges = template.repeat.as_ref().map_or(0, |repeat| {
        repeat.first_names.len() + repeat.last_names.len()
    });
    template.names.values().map(Vec::len).sum::<usize>() + edges
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackList {
    Template,
    Explicit,
    Auto,
}

fn list(source: &str, kind: TrackList) -> Option<GridTemplate<SpecifiedLength>> {
    let mut result = GridTemplate::default();
    if kind == TrackList::Template && source.trim().eq_ignore_ascii_case("none") {
        return Some(result);
    }
    let mut names_pending = false;
    for token in tokens(source)? {
        match token {
            Token::Names(value) if kind != TrackList::Auto && !names_pending => {
                let names = parse_names(value)?;
                if name_count(&result) + names.len() > LINE_NAME_LIMIT {
                    return None;
                }
                let line = result.tracks.len();
                add_names(&mut result, line, names);
                names_pending = true;
            }
            Token::Word(value) => {
                names_pending = false;
                if let Some(repeat) = function(value, "repeat") {
                    if kind != TrackList::Template {
                        return None;
                    }
                    let parts = split_top(repeat, b',');
                    let [count, pattern] = parts.as_slice() else {
                        return None;
                    };
                    let mut pattern = list(pattern, TrackList::Explicit)?;
                    let auto = count.eq_ignore_ascii_case("auto-fill")
                        || count.eq_ignore_ascii_case("auto-fit");
                    let count = if auto {
                        if result.repeat.is_some() {
                            return None;
                        }
                        1
                    } else {
                        count
                            .parse::<usize>()
                            .ok()
                            .filter(|n| *n > 0 && *n <= GRID_TRACK_LIMIT)?
                    };
                    let start = result.tracks.len();
                    let length = pattern.tracks.len();
                    if start + length * count > GRID_TRACK_LIMIT
                        || name_count(&result) + name_count(&pattern) * count > LINE_NAME_LIMIT
                    {
                        return None;
                    }
                    if auto {
                        let names = Arc::make_mut(&mut pattern.names);
                        result.repeat = Some(Arc::new(GridAutoRepeat {
                            range: start..start + length,
                            fit: parts[0].eq_ignore_ascii_case("auto-fit"),
                            first_names: names.remove(&0).unwrap_or_default(),
                            last_names: names.remove(&length).unwrap_or_default(),
                        }));
                    }
                    for _ in 0..count {
                        let offset = result.tracks.len();
                        result.tracks.extend_from_slice(&pattern.tracks);
                        for (&line, names) in pattern.names.iter() {
                            add_names(&mut result, line + offset, names.clone());
                        }
                    }
                } else {
                    if result.tracks.len() >= GRID_TRACK_LIMIT {
                        return None;
                    }
                    result.tracks.push(track(value)?);
                }
            }
            _ => return None,
        }
    }
    if result.repeat.is_some()
        && result.tracks.iter().any(|track| {
            !matches!(track.min, GridBreadth::Length(_))
                && !matches!(track.max, GridBreadth::Length(_))
        })
    {
        return None;
    }
    (!result.tracks.is_empty()).then_some(result)
}

pub(super) fn parse_tracks(source: &str) -> Option<SpecifiedTemplate> {
    list(source, TrackList::Template).map(SpecifiedTemplate::new)
}

pub(super) fn parse_auto_tracks(source: &str) -> Option<SpecifiedTracks> {
    let tracks = list(source, TrackList::Auto)?.tracks;
    Some(if tracks_need_context(&tracks) {
        SpecifiedTracks::Contextual(tracks.into())
    } else {
        SpecifiedTracks::Shared(Arc::new(compute_tracks(&tracks, 16., [0.; 2], 16.)))
    })
}

fn area_row(raw: &str) -> Option<String> {
    if raw.contains(['\r', '\u{c}']) {
        unescape_string(&raw.replace("\r\n", "\n").replace(['\r', '\u{c}'], "\n"))
    } else {
        unescape_string(raw)
    }
}

fn tracks_need_context(tracks: &[GridTrack<SpecifiedLength>]) -> bool {
    tracks.iter().any(|track| {
        [track.min, track.max]
            .into_iter()
            .any(|breadth| matches!(breadth, GridBreadth::Length(length) if length.needs_context()))
    })
}

fn compute_template(
    template: &GridTemplate<SpecifiedLength>,
    root_font: f32,
    viewport: [f32; 2],
    font: f32,
) -> Arc<GridTemplate> {
    Arc::new(GridTemplate {
        tracks: compute_tracks(&template.tracks, root_font, viewport, font),
        names: template.names.clone(),
        repeat: template.repeat.clone(),
    })
}

fn compute_tracks(
    tracks: &[GridTrack<SpecifiedLength>],
    root_font: f32,
    viewport: [f32; 2],
    font: f32,
) -> Vec<GridTrack> {
    let compute = |breadth: GridBreadth<SpecifiedLength>| match breadth {
        GridBreadth::Length(length) => {
            GridBreadth::Length(length.computed(root_font, viewport, font))
        }
        GridBreadth::Flex(n) => GridBreadth::Flex(n),
        GridBreadth::Auto => GridBreadth::Auto,
        GridBreadth::MinContent => GridBreadth::MinContent,
        GridBreadth::MaxContent => GridBreadth::MaxContent,
    };
    tracks
        .iter()
        .map(|track| GridTrack {
            min: compute(track.min),
            max: compute(track.max),
        })
        .collect()
}

fn areas(rows: &[String]) -> Option<GridAreas> {
    if rows.is_empty() || rows.len() > GRID_TRACK_LIMIT {
        return None;
    }
    let mut by_name = BTreeMap::<Arc<str>, (GridArea, usize)>::new();
    let mut columns = 0;
    let name_character = |c: char| ident_start(c) || c.is_ascii_digit() || c == '-';
    for (row, text) in rows.iter().enumerate() {
        let mut remaining = text.as_str();
        let mut column = 0;
        loop {
            remaining = remaining.trim_start_matches(css_space);
            let Some(first) = remaining.chars().next() else {
                break;
            };
            if column == GRID_TRACK_LIMIT {
                return None;
            }
            if first == '.' {
                remaining = remaining.trim_start_matches('.');
            } else if name_character(first) {
                let end = remaining
                    .find(|c| !name_character(c))
                    .unwrap_or(remaining.len());
                let name = &remaining[..end];
                if name.len() > NAME_LIMIT {
                    return None;
                }
                if let Some((rect, count)) = by_name.get_mut(name) {
                    rect.rows[1] = row + 1;
                    rect.columns[0] = rect.columns[0].min(column);
                    rect.columns[1] = rect.columns[1].max(column + 1);
                    *count += 1;
                } else {
                    by_name.insert(
                        Arc::from(name),
                        (
                            GridArea {
                                rows: [row, row + 1],
                                columns: [column, column + 1],
                            },
                            1,
                        ),
                    );
                }
                remaining = &remaining[end..];
            } else {
                return None;
            }
            column += 1;
        }
        if column == 0 || (row != 0 && column != columns) {
            return None;
        }
        columns = column;
    }
    let names = by_name
        .into_iter()
        .map(|(name, (rect, count))| {
            let size = (rect.rows[1] - rect.rows[0]) * (rect.columns[1] - rect.columns[0]);
            (count == size).then_some((name, rect))
        })
        .collect::<Option<BTreeMap<_, _>>>()?;
    Some(GridAreas {
        dimensions: [rows.len(), columns],
        names,
    })
}

pub(super) fn parse_areas(source: &str) -> Option<GridAreas> {
    if source.trim().eq_ignore_ascii_case("none") {
        return Some(GridAreas::default());
    }
    let rows = tokens(source)?
        .into_iter()
        .map(|token| match token {
            Token::String(value) => area_row(value),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    areas(&rows)
}

pub(super) fn parse_template(
    source: &str,
) -> Option<(SpecifiedTemplate, SpecifiedTemplate, GridAreas)> {
    if source.len() > DECLARATION_LIMIT {
        return None;
    }
    if source.trim().eq_ignore_ascii_case("none") {
        let empty = SpecifiedTemplate::Shared(Arc::new(GridTemplate::default()));
        return Some((empty.clone(), empty, GridAreas::default()));
    }
    let parts = split_top(source, b'/');
    let (rows, columns) = match parts.as_slice() {
        [rows] => (*rows, None),
        [rows, columns] => (*rows, Some(*columns)),
        _ => return None,
    };
    let tokens = tokens(rows)?;
    if !tokens.iter().any(|token| matches!(token, Token::String(_))) {
        return Some((
            parse_tracks(rows)?,
            parse_tracks(columns?)?,
            GridAreas::default(),
        ));
    }
    let mut tokens = tokens.into_iter().peekable();
    let mut rows = GridTemplate::default();
    let mut matrix = Vec::new();
    while tokens.peek().is_some() {
        let line = rows.tracks.len();
        if line >= GRID_TRACK_LIMIT {
            return None;
        }
        if let Some(Token::Names(value)) = tokens.peek() {
            add_names(&mut rows, line, parse_names(value)?);
            tokens.next();
        }
        let Some(Token::String(raw)) = tokens.next() else {
            return None;
        };
        matrix.push(area_row(raw)?);
        let size = if let Some(Token::Word(value)) = tokens.peek() {
            let size = track(value)?;
            tokens.next();
            size
        } else {
            GridTrack::default()
        };
        rows.tracks.push(size);
        if let Some(Token::Names(value)) = tokens.peek() {
            add_names(&mut rows, line + 1, parse_names(value)?);
            tokens.next();
        }
        if name_count(&rows) > LINE_NAME_LIMIT {
            return None;
        }
    }
    let columns = if let Some(columns) = columns {
        list(columns, TrackList::Explicit)?
    } else {
        GridTemplate::default()
    };
    Some((
        SpecifiedTemplate::new(rows),
        SpecifiedTemplate::new(columns),
        areas(&matrix)?,
    ))
}
