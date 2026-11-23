use super::*;
use unicode_segmentation::UnicodeSegmentation;

const SHAPE_SEGMENT_LIMIT: usize = 4096;
const PLAN_CACHE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PlanKey {
    face: u64,
    coords: Coordinates,
    direction: rustybuzz::Direction,
    script: rustybuzz::Script,
    language: Option<rustybuzz::Language>,
}

struct CachedPlan {
    face: Weak<FaceData>,
    plan: rustybuzz::ShapePlan,
    cost: usize,
}

#[derive(Default)]
pub(super) struct PlanCache {
    plans: HashMap<PlanKey, CachedPlan>,
    fifo: VecDeque<PlanKey>,
    bytes: usize,
}

impl PlanCache {
    fn shape(
        &mut self,
        face: &rustybuzz::Face<'_>,
        instance: &RunFace,
        mut buffer: rustybuzz::UnicodeBuffer,
        features: &[rustybuzz::Feature],
    ) -> rustybuzz::GlyphBuffer {
        buffer.guess_segment_properties();
        if !features.is_empty() {
            return rustybuzz::shape(face, features, buffer);
        }
        let key = PlanKey {
            face: instance.face.id(),
            coords: instance.coords,
            direction: buffer.direction(),
            script: buffer.script(),
            language: buffer.language(),
        };
        if let Some(entry) = self.plans.get(&key) {
            return rustybuzz::shape_with_plan(face, &entry.plan, buffer);
        }
        let plan = rustybuzz::ShapePlan::new(
            face,
            key.direction,
            (key.script != rustybuzz::script::UNKNOWN).then_some(key.script),
            key.language.as_ref(),
            &[],
        );
        let shaped = rustybuzz::shape_with_plan(face, &plan, buffer);
        let cost = instance.face.data.plan_cost;
        if cost > PLAN_CACHE_BYTES {
            return shaped;
        }
        self.fifo.retain(|key| {
            if self
                .plans
                .get(key)
                .is_some_and(|entry| entry.face.strong_count() != 0)
            {
                return true;
            }
            if let Some(entry) = self.plans.remove(key) {
                self.bytes -= entry.cost;
            }
            false
        });
        while self.plans.len() >= 64 || self.bytes + cost > PLAN_CACHE_BYTES {
            let key = self.fifo.pop_front().expect("bounded shape plans");
            self.bytes -= self.plans.remove(&key).expect("shape plan entry").cost;
        }
        self.bytes += cost;
        self.fifo.push_back(key.clone());
        self.plans.insert(
            key,
            CachedPlan {
                face: Arc::downgrade(&instance.face.data),
                plan,
                cost,
            },
        );
        shaped
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct CanvasShaping<'a> {
    pub language: Option<&'a str>,
    pub features: &'a [rustybuzz::Feature],
    pub letter_spacing: f32,
    pub word_spacing: f32,
}

impl CanvasShaping<'_> {
    fn is_default(self) -> bool {
        self.language.is_none()
            && self.features.is_empty()
            && self.letter_spacing == 0.
            && self.word_spacing == 0.
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapeKey {
    request: FontRequest,
    font_set: u64,
    size: u32,
}

struct CachedRun {
    glyphs: Vec<Glyph>,
    faces: Vec<(Weak<FaceData>, Coordinates)>,
    advance: f32,
    size: f32,
    ascent: f32,
    descent: f32,
}

impl CachedRun {
    fn from_run(run: &Run) -> Self {
        Self {
            glyphs: run.glyphs.clone(),
            faces: run
                .faces
                .iter()
                .map(|instance| (Arc::downgrade(&instance.face.data), instance.coords))
                .collect(),
            advance: run.advance,
            size: run.size,
            ascent: run.ascent,
            descent: run.descent,
        }
    }

    fn get(&self) -> Option<Run> {
        let faces: Option<Vec<_>> = self
            .faces
            .iter()
            .map(|(weak, coords)| {
                Some(RunFace {
                    face: FontFace {
                        data: weak.upgrade()?,
                    },
                    coords: *coords,
                })
            })
            .collect();
        Some(Run {
            glyphs: self.glyphs.clone(),
            faces: faces?.into(),
            advance: self.advance,
            size: self.size,
            ascent: self.ascent,
            descent: self.descent,
        })
    }
}

#[derive(Default)]
pub(super) struct ShapeCache {
    runs: HashMap<ShapeKey, HashMap<Arc<str>, CachedRun>>,
    fifo: VecDeque<(ShapeKey, Arc<str>, usize)>,
    pub stats: ShapeCacheStats,
}

impl ShapeCache {
    fn get(&mut self, key: &ShapeKey, text: &str) -> Option<Run> {
        let run = self
            .runs
            .get(key)
            .and_then(|runs| runs.get(text))
            .and_then(CachedRun::get);
        if run.is_some() {
            self.stats.hits += 1;
        } else {
            self.stats.misses += 1;
        }
        run
    }

    fn insert(&mut self, key: ShapeKey, text: &str, run: &Run) {
        if let Some(old) = self.runs.get_mut(&key).and_then(|runs| runs.get_mut(text)) {
            // A stale entry has released its face. Remove its FIFO slot so a
            // replacement with different fallback coverage can be recharged.
            if old.faces.iter().any(|(face, _)| face.strong_count() == 0) {
                if let Some(index) = self
                    .fifo
                    .iter()
                    .position(|(k, t, _)| *k == key && t.as_ref() == text)
                {
                    let (_, _, cost) = self.fifo.remove(index).expect("stale cache entry");
                    self.stats.payload_bytes -= cost;
                    self.stats.entries -= 1;
                }
                if let Some(runs) = self.runs.get_mut(&key) {
                    runs.remove(text);
                }
            } else {
                return;
            }
        }
        let name_bytes: usize = key
            .request
            .families
            .iter()
            .map(|name| match name {
                FontFamily::Named(name) => name.len(),
                FontFamily::Generic(_) => 0,
            })
            .sum();
        let cost = text.len()
            + name_bytes
            + run.glyphs.len() * std::mem::size_of::<Glyph>()
            + run.faces.len() * std::mem::size_of::<(Weak<FaceData>, Coordinates)>()
            + std::mem::size_of::<ShapeKey>()
            + std::mem::size_of::<CachedRun>()
            + 64;
        if cost > SHAPE_CACHE_BYTES {
            return;
        }
        while self.stats.payload_bytes + cost > SHAPE_CACHE_BYTES
            || self.stats.entries >= SHAPE_CACHE_ENTRIES
        {
            let (key, text, cost) = self.fifo.pop_front().expect("bounded shape cache");
            if let Some(runs) = self.runs.get_mut(&key) {
                runs.remove(text.as_ref());
                if runs.is_empty() {
                    self.runs.remove(&key);
                }
            }
            self.stats.payload_bytes -= cost;
            self.stats.entries -= 1;
        }
        let text: Arc<str> = Arc::from(text);
        self.runs
            .entry(key.clone())
            .or_default()
            .insert(text.clone(), CachedRun::from_run(run));
        self.fifo.push_back((key, text, cost));
        self.stats.entries += 1;
        self.stats.payload_bytes += cost;
    }
}

enum Candidate<'a> {
    Web(&'a FontFaceEntry),
    System(fontdb::ID),
    Default,
}

fn ignored_for_coverage(ch: char) -> bool {
    matches!(ch, '\u{200c}' | '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}')
}

fn covers(
    face: &ttf_parser::Face<'_>,
    ranges: &[UnicodeRange],
    chars: impl Iterator<Item = char>,
) -> bool {
    chars.filter(|&ch| !ignored_for_coverage(ch)).all(|ch| {
        let code = u32::from(ch);
        (ranges.is_empty()
            || ranges
                .get(ranges.partition_point(|range| range.end < code))
                .is_some_and(|range| code >= range.start))
            && face.glyph_index(ch).is_some_and(|glyph| glyph.0 != 0)
    })
}

fn families_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
        || (!a.is_ascii() || !b.is_ascii()) && a.to_lowercase() == b.to_lowercase()
}

impl Fonts {
    pub fn shape_text(&self, text: &str, size: f32, request: &FontRequest, set: &FontSet) -> Run {
        let size = if size.is_finite() {
            size.clamp(0., 512.)
        } else {
            16.
        };
        let key = ShapeKey {
            request: request.clone(),
            font_set: set.revision(),
            size: size.to_bits(),
        };
        if text.len() <= 512
            && let Some(run) = self.shapes.borrow_mut().get(&key, text)
        {
            return run;
        }
        let run = self
            .shape_uncached(text, size, request, set, false, CanvasShaping::default())
            .0;
        if text.len() <= 512 && run.glyphs.len() <= 256 {
            self.shapes.borrow_mut().insert(key, text, &run);
        }
        run
    }

    pub(crate) fn shape_canvas(
        &self,
        text: &str,
        size: f32,
        request: &FontRequest,
        set: &FontSet,
        options: CanvasShaping<'_>,
    ) -> Run {
        if options.is_default() || text.is_empty() {
            self.shape_text(text, size, request, set)
        } else {
            self.shape_uncached(text, size, request, set, false, options)
                .0
        }
    }

    fn candidates<'a>(&self, request: &FontRequest, set: &'a FontSet) -> Vec<Candidate<'a>> {
        let mut candidates = Vec::new();
        let mut checked = Vec::new();
        for family in request.families.iter().take(16) {
            if checked.contains(&family) {
                continue;
            }
            checked.push(family);
            if let FontFamily::Named(name) = family {
                let matching = set
                    .entries()
                    .iter()
                    .rev()
                    .filter(|entry| families_equal(&entry.rule.family, name));
                let score = |entry: &FontFaceEntry| {
                    (
                        catalog::style_score(request.style, entry.rule.style),
                        catalog::weight_score(request.weight, entry.rule.weight),
                    )
                };
                if let Some(best) = matching.clone().map(score).min() {
                    candidates.extend(
                        matching
                            .filter(|entry| score(entry) == best)
                            .map(Candidate::Web),
                    );
                    continue;
                }
            }
            if let Some(id) = self.system_match(family, request) {
                candidates.push(Candidate::System(id));
            }
        }
        candidates.push(Candidate::Default);
        candidates
    }

    fn resolve_candidate<'a>(
        &self,
        candidate: Candidate<'a>,
        request: &FontRequest,
    ) -> Option<(RunFace, &'a [UnicodeRange])> {
        let (face, ranges, weight) = match candidate {
            Candidate::Web(entry) => {
                let weight = request
                    .weight
                    .clamp(entry.rule.weight[0], entry.rule.weight[1]);
                (
                    entry.face.clone(),
                    entry.rule.unicode_ranges.as_slice(),
                    weight,
                )
            }
            Candidate::System(id) => (self.system_face(id)?, &[][..], request.weight),
            Candidate::Default => (self.default.clone(), &[][..], request.weight),
        };
        Some((RunFace::new(face, request, weight), ranges))
    }

    fn shape_uncached(
        &self,
        text: &str,
        size: f32,
        request: &FontRequest,
        set: &FontSet,
        want_clusters: bool,
        options: CanvasShaping<'_>,
    ) -> (Run, Vec<(usize, f32)>) {
        let mut end = text.len().min(MAX_SHAPE_INPUT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let text = &text[..end];
        let mut candidates = self.candidates(request, set).into_iter();
        let (primary, ranges) = candidates
            .by_ref()
            .find_map(|candidate| self.resolve_candidate(candidate, request))
            .expect("default face");
        let mut out = Output {
            size,
            advance: 0.,
            ascent: 0.,
            descent: 0.,
            glyphs: vec![],
            faces: vec![],
            clusters: vec![],
            want_clusters,
            plans: &self.plans,
            options,
        };
        if text.is_empty() {
            (out.ascent, out.descent) = line_metrics(&primary, size);
            out.faces.push(primary);
            return out.finish();
        }
        if text.len() <= SHAPE_SEGMENT_LIMIT && covers(&parsed_face(&primary), ranges, text.chars())
        {
            out.segment(text, 0, &primary);
            return out.finish();
        }

        // Try each CSS face only for graphemes the preceding faces could not
        // represent. Installed fonts later in the list are opened lazily.
        let mut clusters: Vec<_> = text
            .grapheme_indices(true)
            .map(|(start, cluster)| (start, cluster, None))
            .collect();
        let mut remaining = clusters.len();
        let mut faces = Vec::new();
        for (instance, ranges) in std::iter::once((primary.clone(), ranges))
            .chain(candidates.filter_map(|candidate| self.resolve_candidate(candidate, request)))
        {
            let face = parsed_face(&instance);
            let slot = faces.len();
            let before = remaining;
            for (_, cluster, choice) in &mut clusters {
                if choice.is_none() && covers(&face, ranges, cluster.chars().take(64)) {
                    *choice = Some(slot);
                    remaining -= 1;
                }
            }
            if before != remaining {
                faces.push(instance);
            }
            if remaining == 0 {
                break;
            }
        }

        let mut fallback = Vec::new();
        let mut previous: Option<(RunFace, usize)> = None;
        for (start, cluster, choice) in clusters {
            let selected = choice
                .map(|slot| faces[slot].clone())
                .or_else(|| self.fallback(cluster, request, &mut fallback))
                .unwrap_or_else(|| primary.clone());
            if let Some((instance, offset)) = &mut previous {
                if instance.key() == selected.key()
                    && start + cluster.len() - *offset <= SHAPE_SEGMENT_LIMIT
                {
                    continue;
                }
                out.segment(&text[*offset..start], *offset, instance);
            }
            previous = Some((selected, start));
        }
        if let Some((face, start)) = previous {
            out.segment(&text[start..], start, &face);
        }
        out.finish()
    }

    fn fallback(
        &self,
        cluster: &str,
        request: &FontRequest,
        loaded: &mut Vec<RunFace>,
    ) -> Option<RunFace> {
        let sample = cluster.chars().find(|&ch| !ignored_for_coverage(ch))?;
        let is_cjk = matches!(sample, '\u{3000}'..='\u{9fff}' | '\u{ac00}'..='\u{d7ff}' | '\u{20000}'..='\u{3ffff}');
        let is_symbol = matches!(sample, '\u{2100}'..='\u{2bff}' | '\u{1f000}'..='\u{1faff}');
        let families: &[&str] = if is_cjk {
            &[
                "Microsoft YaHei",
                "Yu Gothic",
                "Malgun Gothic",
                "Noto Sans CJK SC",
                "SimSun",
            ]
        } else if is_symbol {
            &[
                "Segoe UI Symbol",
                "Segoe UI Emoji",
                "DejaVu Sans",
                "Noto Sans Symbols 2",
            ]
        } else {
            &[
                "Arial",
                "Segoe UI Symbol",
                "Arial Unicode MS",
                "DejaVu Sans",
                "Noto Sans",
            ]
        };
        for name in families {
            let family = FontFamily::Named(Arc::from(*name));
            let Some(id) = self.system_match(&family, request) else {
                continue;
            };
            let Some(face) = self.system_face(id) else {
                continue;
            };
            let instance = loaded
                .iter()
                .find(|instance| instance.face.id() == face.id())
                .cloned()
                .unwrap_or_else(|| RunFace::new(face, request, request.weight));
            let data = &instance.face.data;
            if ttf_parser::Face::parse(&data.bytes, data.index)
                .is_ok_and(|face| covers(&face, &[], cluster.chars().take(64)))
            {
                if loaded.len() < 16 && !loaded.iter().any(|old| old.key() == instance.key()) {
                    loaded.push(instance.clone());
                }
                return Some(instance);
            }
        }
        None
    }

    /// Shape bounded chunks for cluster advances, then reshape each final
    /// segment. No prefix loops or splitting within normal shaping clusters.
    pub fn break_word_text(
        &self,
        text: &str,
        size: f32,
        request: &FontRequest,
        set: &FontSet,
        width: f32,
    ) -> Vec<Run> {
        let mut result = Vec::new();
        let mut offset = 0;
        let width = width.max(1.);
        while offset < text.len() {
            let mut end = (offset + SHAPE_SEGMENT_LIMIT + 128).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let extended = &text[offset..end];
            let (_, mut clusters) =
                self.shape_uncached(extended, size, request, set, true, CanvasShaping::default());
            if end - offset > SHAPE_SEGMENT_LIMIT {
                let boundary = clusters
                    .iter()
                    .map(|c| c.0)
                    .filter(|&n| n > 0 && n <= SHAPE_SEGMENT_LIMIT)
                    .max();
                let cut = boundary.unwrap_or_else(|| {
                    let mut n = SHAPE_SEGMENT_LIMIT;
                    while !extended.is_char_boundary(n) {
                        n -= 1;
                    }
                    n
                });
                end = offset + cut;
                clusters.retain(|c| c.0 < cut);
            }
            let chunk = &text[offset..end];
            clusters.sort_unstable_by_key(|c| c.0);
            let mut grouped: Vec<(usize, f32)> = Vec::new();
            for (index, advance) in clusters {
                if let Some(last) = grouped.last_mut().filter(|last| last.0 == index) {
                    last.1 += advance;
                } else {
                    grouped.push((index, advance));
                }
            }
            let mut start = 0;
            let mut advance = 0.;
            for (index, cluster_width) in grouped {
                if index > start && advance + cluster_width > width {
                    result.push(self.shape_text(&chunk[start..index], size, request, set));
                    start = index;
                    advance = 0.;
                }
                advance += cluster_width;
            }
            if start < chunk.len() {
                result.push(self.shape_text(&chunk[start..], size, request, set));
            }
            offset = end;
        }
        result
    }
}

pub(super) fn face(instance: &RunFace) -> rustybuzz::Face<'_> {
    let data = &instance.face.data;
    let mut face = rustybuzz::Face::from_slice(&data.bytes, data.index).expect("validated font");
    let axes: Vec<_> = instance
        .coords
        .axes()
        .map(|(tag, value)| rustybuzz::Variation {
            tag: ttf_parser::Tag::from_bytes(&tag),
            value,
        })
        .collect();
    if !axes.is_empty() {
        face.set_variations(&axes);
    }
    face
}
fn parsed_face(instance: &RunFace) -> ttf_parser::Face<'_> {
    let data = &instance.face.data;
    ttf_parser::Face::parse(&data.bytes, data.index).expect("validated font")
}

fn line_metrics(instance: &RunFace, size: f32) -> (f32, f32) {
    let face = face(instance);
    let scale = size / face.units_per_em() as f32;
    (
        (f32::from(face.ascender()) * scale).max(0.),
        (-f32::from(face.descender()) * scale).max(0.),
    )
}

struct Output<'a> {
    size: f32,
    advance: f32,
    ascent: f32,
    descent: f32,
    glyphs: Vec<Glyph>,
    faces: Vec<RunFace>,
    clusters: Vec<(usize, f32)>,
    want_clusters: bool,
    plans: &'a RefCell<PlanCache>,
    options: CanvasShaping<'a>,
}

impl Output<'_> {
    fn segment(&mut self, text: &str, offset: usize, instance: &RunFace) {
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + SHAPE_SEGMENT_LIMIT).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.segment_piece(&text[start..end], offset + start, instance);
            start = end;
        }
    }
    fn segment_piece(&mut self, text: &str, offset: usize, instance: &RunFace) {
        let slot = self
            .faces
            .iter()
            .position(|existing| existing.key() == instance.key())
            .unwrap_or_else(|| {
                self.faces.push(instance.clone());
                self.faces.len() - 1
            });
        let face = face(instance);
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        if let Some(language) = self.options.language.and_then(|s| s.parse().ok()) {
            buffer.set_language(language);
        }
        let shaped = self
            .plans
            .borrow_mut()
            .shape(&face, instance, buffer, self.options.features);
        let scale = self.size / face.units_per_em() as f32;
        self.ascent = self.ascent.max(f32::from(face.ascender()) * scale);
        self.descent = self.descent.max(-f32::from(face.descender()) * scale);
        let mut x = 0.;
        let limit = text.len().saturating_mul(4).clamp(1, 8192);
        let mut glyphs = shaped
            .glyph_infos()
            .iter()
            .zip(shaped.glyph_positions())
            .take(limit)
            .peekable();
        while let Some((info, position)) = glyphs.next() {
            self.glyphs.push(Glyph {
                id: info.glyph_id as u16,
                face_slot: slot as u16,
                x: self.advance + x + position.x_offset as f32 * scale,
                y: position.y_offset as f32 * scale,
            });
            let mut advance = position.x_advance as f32 * scale;
            if glyphs
                .peek()
                .is_none_or(|(next, _)| next.cluster != info.cluster)
            {
                advance += self.options.letter_spacing;
                if self.options.word_spacing != 0.
                    && text
                        .get(info.cluster as usize..)
                        .is_some_and(|rest| rest.starts_with([' ', '\u{a0}']))
                {
                    advance += self.options.word_spacing;
                }
            }
            x += advance;
            if self.want_clusters {
                self.clusters
                    .push((offset + info.cluster as usize, advance.abs()));
            }
        }
        self.advance += if self.options.is_default() {
            x.abs()
        } else {
            x
        };
    }

    fn finish(self) -> (Run, Vec<(usize, f32)>) {
        (
            Run {
                glyphs: self.glyphs,
                faces: self.faces.into(),
                advance: self.advance,
                size: self.size,
                ascent: self.ascent,
                descent: self.descent,
            },
            self.clusters,
        )
    }
}
