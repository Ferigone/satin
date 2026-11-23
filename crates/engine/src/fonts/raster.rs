use super::*;
use ab_glyph::{Font, VariableFont};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GlyphMetrics {
    pub xmin: i32,
    pub ymin: i32,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RasterFailure {
    Invalid,
    Limit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: u64,
    coords: Coordinates,
    id: u16,
    scale: u16,
}

struct CachedGlyph {
    face: Weak<FaceData>,
    metrics: GlyphMetrics,
    pixels: Vec<u8>,
}

#[derive(Default)]
pub(super) struct RasterCache {
    entries: HashMap<GlyphKey, CachedGlyph>,
    fifo: VecDeque<GlyphKey>,
    bytes: usize,
    next_prune: u8,
}

impl RasterCache {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn insert(&mut self, key: GlyphKey, glyph: CachedGlyph) {
        let cost = glyph.pixels.len();
        while self.bytes + cost > GLYPH_CACHE_BYTES || self.entries.len() >= GLYPH_CACHE_ENTRIES {
            let old = self.fifo.pop_front().expect("bounded glyph cache");
            self.bytes -= self
                .entries
                .remove(&old)
                .expect("cached glyph")
                .pixels
                .len();
        }
        self.bytes += cost;
        self.entries.insert(key, glyph);
        self.fifo.push_back(key);
    }

    fn prune(&mut self) {
        if self.next_prune != 0 {
            self.next_prune -= 1;
            return;
        }
        self.next_prune = 31;
        let entries = &mut self.entries;
        let bytes = &mut self.bytes;
        self.fifo.retain(|key| {
            if entries
                .get(key)
                .is_some_and(|glyph| glyph.face.strong_count() != 0)
            {
                true
            } else {
                if let Some(glyph) = entries.remove(key) {
                    *bytes -= glyph.pixels.len();
                }
                false
            }
        });
    }
}

fn charge(budget: &mut usize, cost: usize) -> Result<(), RasterFailure> {
    if cost > *budget {
        *budget = 0;
        Err(RasterFailure::Limit)
    } else {
        *budget -= cost;
        Ok(())
    }
}

fn quantized(size: f32) -> u16 {
    (size * 16.).round().clamp(1., 8192.) as u16
}

impl Fonts {
    /// Drop stale web font raster entries between frames. Cached pixels never
    /// own the font bytes. This also releases their bounded bitmap storage.
    pub(crate) fn prune_raster_cache(&mut self) {
        self.raster.prune();
    }

    pub(crate) fn raster_glyph(
        &mut self,
        instance: &RunFace,
        id: u16,
        size: f32,
        budget: &mut usize,
    ) -> Result<(GlyphMetrics, &[u8]), RasterFailure> {
        if !size.is_finite() || size <= 0. {
            return Err(RasterFailure::Invalid);
        }
        let key = GlyphKey {
            face: instance.face.id(),
            coords: instance.coords,
            id,
            scale: quantized(size),
        };
        if !self.raster.entries.contains_key(&key) {
            let data = &instance.face.data;
            let cost = *data
                .costs
                .get(usize::from(id))
                .ok_or(RasterFailure::Invalid)? as usize;
            charge(budget, cost.saturating_mul(4).saturating_add(64))?;
            let mut font = ab_glyph::FontRef::try_from_slice_and_index(&data.bytes, data.index)
                .map_err(|_| RasterFailure::Invalid)?;
            for (tag, value) in instance.coords.axes() {
                font.set_variation(&tag, value);
            }
            // ab_glyph scales by ascender minus descender. Convert CSS em size
            // to that scale, matching the units-per-em used by the shaper.
            let em = font.units_per_em().ok_or(RasterFailure::Invalid)?;
            let height = font.height_unscaled();
            if !em.is_finite() || !height.is_finite() || em <= 0. || height <= 0. {
                return Err(RasterFailure::Invalid);
            }
            let glyph = ab_glyph::GlyphId(id).with_scale(f32::from(key.scale) / 16. * height / em);
            let (metrics, pixels) = if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                let width = bounds.width();
                let height = bounds.height();
                if ![bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]
                    .iter()
                    .all(|value| value.is_finite() && value.abs() <= 32768.)
                {
                    return Err(RasterFailure::Limit);
                }
                let width = width.max(0.) as usize;
                let height = height.max(0.) as usize;
                let area = width.saturating_mul(height);
                if area > 1024 * 1024 || width > 2048 || height > 2048 {
                    return Err(RasterFailure::Limit);
                }
                let edge_work = cost.saturating_mul(width.max(height).max(1));
                charge(budget, area.saturating_mul(2).saturating_add(edge_work))?;
                let metrics = GlyphMetrics {
                    xmin: bounds.min.x as i32,
                    ymin: -bounds.max.y as i32,
                    width,
                    height,
                };
                let mut pixels = vec![0u8; area];
                if area != 0 {
                    outline.draw(|x, y, coverage| {
                        if let Some(pixel) = pixels.get_mut(y as usize * width + x as usize) {
                            *pixel = (coverage.clamp(0., 1.) * 255.).round() as u8;
                        }
                    });
                }
                (metrics, pixels)
            } else {
                (GlyphMetrics::default(), Vec::new())
            };
            self.raster.insert(
                key,
                CachedGlyph {
                    face: Arc::downgrade(data),
                    metrics,
                    pixels,
                },
            );
        }
        let glyph = self.raster.entries.get(&key).expect("raster cache entry");
        charge(budget, 1)?;
        Ok((glyph.metrics, &glyph.pixels))
    }
}
