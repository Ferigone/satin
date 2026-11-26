//! Resolves explicit Grid tracks and line names before item placement. Track
//! collapse follows placement so even a spanning item keeps its tracks open.
use super::{BoxMetrics, GridAxisPosition, GridPlacement, to_content};
use crate::css::{
    GRID_TRACK_LIMIT, GridAreas, GridBreadth, GridLine, GridTemplate, GridTrack, Length, Style,
};
use std::{ops::Deref, sync::Arc};

#[derive(Clone, Copy)]
pub(super) struct AxisSize {
    pub(super) preferred: Option<f32>,
    pub(super) minimum: f32,
    pub(super) maximum: f32,
}

impl AxisSize {
    pub(super) fn fixed(size: f32) -> Self {
        Self {
            preferred: Some(size),
            minimum: 0.,
            maximum: f32::INFINITY,
        }
    }

    pub(super) fn from_style(
        style: &Style,
        row: bool,
        containing: Option<f32>,
        metrics: BoxMetrics,
    ) -> Self {
        let (preferred, min, max, extra) = if row {
            (
                style.height,
                style.min_height,
                style.max_height,
                metrics.extra_y(),
            )
        } else {
            (
                style.width,
                style.min_width,
                style.max_width,
                metrics.extra_x(),
            )
        };
        let resolve = |length: Length, basis| {
            length
                .resolve_with_basis(basis, style.font_size)
                .map(|value| to_content(value, extra, style.box_sizing))
        };
        let minimum = resolve(min, Some(containing.unwrap_or(0.))).unwrap_or(0.);
        let maximum = resolve(max, containing)
            .unwrap_or(f32::INFINITY)
            .max(minimum);
        Self {
            preferred: resolve(preferred, containing).map(|value| value.clamp(minimum, maximum)),
            minimum,
            maximum,
        }
    }
}

pub(super) struct Axis<'a> {
    template: &'a GridTemplate,
    areas: &'a GridAreas,
    row: bool,
    count: usize,
    repeat_end: usize,
    pub(super) explicit: usize,
    pub(super) limited: bool,
}

impl<'a> Axis<'a> {
    pub(super) fn new(style: &'a Style, row: bool, size: AxisSize) -> Self {
        let axis = usize::from(!row);
        let template = if row {
            &style.grid_template_rows
        } else {
            &style.grid_template_columns
        };
        let areas = &style.grid_template_areas;
        let (count, repeat_end, limited) = if let Some(repeat) = &template.repeat {
            let (copies, limited) = repeat_count(template, size, style.gap[axis], style.font_size);
            let pattern = repeat.range.len();
            (
                template.tracks.len() + pattern * (copies - 1),
                repeat.range.start + pattern * copies,
                limited,
            )
        } else {
            (template.tracks.len(), 0, false)
        };
        Self {
            template,
            areas,
            row,
            count,
            repeat_end,
            explicit: count.max(areas.dimensions()[axis]),
            limited,
        }
    }

    pub(super) fn inherited_columns(mut self, count: usize) -> Self {
        self.count = count;
        self.explicit = count;
        self
    }

    // Within the same node and axis the template, areas and line names
    // are immutable. Expansion is the only size-dependent placement input.
    pub(super) fn placement_key(&self) -> [usize; 3] {
        [self.count, self.repeat_end, self.explicit]
    }

    fn track(&self, mut index: usize) -> Option<GridTrack> {
        if let Some(repeat) = &self.template.repeat {
            let start = repeat.range.start;
            if index >= self.repeat_end {
                index -= self.repeat_end - repeat.range.end;
            } else if index >= start {
                index = start + (index - start) % repeat.range.len();
            }
        }
        self.template.tracks.get(index).copied()
    }

    fn has_name(&self, line: usize, name: &str) -> bool {
        let at = |index| {
            self.template
                .names
                .get(&index)
                .is_some_and(|names| includes(names, name))
        };
        let Some(repeat) = &self.template.repeat else {
            return at(line);
        };
        let start = repeat.range.start;
        if line < start {
            at(line)
        } else if line == start {
            at(line) || includes(&repeat.first_names, name)
        } else if line > self.repeat_end {
            at(line - (self.repeat_end - repeat.range.end))
        } else if line == self.repeat_end {
            at(repeat.range.end) || includes(&repeat.last_names, name)
        } else {
            let position = (line - start) % repeat.range.len();
            if position == 0 {
                includes(&repeat.last_names, name) || includes(&repeat.first_names, name)
            } else {
                at(start + position)
            }
        }
    }

    fn area_line(&self, name: &str) -> Option<usize> {
        let (name, end) = if let Some(name) = name.strip_suffix("-start") {
            (name, 0)
        } else {
            (name.strip_suffix("-end")?, 1)
        };
        let area = self.areas.names.get(name)?;
        Some(if self.row {
            area.rows[end]
        } else {
            area.columns[end]
        })
    }

    // The implicit lines on the search side count as matches only after the
    // explicit lines have been searched in that direction.
    fn named_from(
        &self,
        name: &str,
        mut matches: i32,
        edge: i32,
        forward: bool,
        work: &mut usize,
    ) -> Option<i32> {
        let end = self.explicit as i32;
        let area = self.area_line(name);
        let mut line = if forward {
            (edge + 1).max(0)
        } else {
            (edge - 1).min(end)
        };
        let step = if forward { 1 } else { -1 };
        let cost = 1 + name.len().div_ceil(32);
        while line >= 0 && line <= end {
            *work = work.checked_sub(cost)?;
            let index = line as usize;
            if area == Some(index) || self.has_name(index, name) {
                matches -= 1;
                if matches == 0 {
                    return Some(line);
                }
            }
            line += step;
        }
        Some(if forward {
            edge.max(end) + matches
        } else {
            edge.min(0) - matches
        })
    }

    fn line(&self, line: &GridLine, end: bool, work: &mut usize) -> Option<Option<i32>> {
        let last = self.explicit as i32;
        Some(match line {
            GridLine::Line(n) => Some(if *n > 0 {
                i32::from(*n) - 1
            } else {
                last + 1 + i32::from(*n)
            }),
            GridLine::Named { name, occurrence } => {
                if occurrence.is_none() {
                    let suffix = if end { "-end" } else { "-start" };
                    let candidate =
                        self.named_from(&format!("{name}{suffix}"), 1, -1, true, work)?;
                    if candidate <= last {
                        return Some(Some(candidate));
                    }
                }
                let occurrence = i32::from(occurrence.unwrap_or(1));
                let forward = occurrence > 0;
                let edge = if forward { -1 } else { last + 1 };
                Some(self.named_from(name, occurrence.abs(), edge, forward, work)?)
            }
            _ => None,
        })
    }

    fn anchored_span(
        &self,
        line: &GridLine,
        edge: i32,
        forward: bool,
        work: &mut usize,
    ) -> Option<usize> {
        Some(match line {
            GridLine::Span(n) => usize::from(*n),
            GridLine::NamedSpan { name, count } => {
                let bound = self.named_from(name, i32::from(*count), edge, forward, work)?;
                bound.abs_diff(edge) as usize
            }
            _ => 1,
        })
    }

    pub(super) fn position(
        &self,
        lines: &[GridLine; 2],
        work: &mut usize,
    ) -> Option<GridAxisPosition> {
        let start = self.line(&lines[0], false, work)?;
        let end = self.line(&lines[1], true, work)?;
        Some(match (start, end) {
            (Some(a), Some(b)) => GridAxisPosition {
                start: Some(a.min(b)),
                span: a.abs_diff(b).max(1) as usize,
            },
            (Some(start), None) => GridAxisPosition {
                start: Some(start),
                span: self.anchored_span(&lines[1], start, true, work)?,
            },
            (None, Some(end)) => {
                let span = self.anchored_span(&lines[0], end, false, work)?;
                GridAxisPosition {
                    start: Some(end - span as i32),
                    span,
                }
            }
            (None, None) => GridAxisPosition {
                start: None,
                // With two spans the end contribution is discarded. A named
                // span with no definite opposite edge contributes one track.
                span: match (&lines[0], &lines[1]) {
                    (GridLine::Span(n), _) | (GridLine::Auto, GridLine::Span(n)) => usize::from(*n),
                    _ => 1,
                },
            },
        })
    }

    pub(super) fn tracks(&self, automatic: &[GridTrack], placement: &GridPlacement) -> Tracks {
        let (count, origin) = if self.row {
            (placement.rows, placement.row_origin)
        } else {
            (placement.columns, placement.column_origin)
        };
        let collapsed =
            if let Some(repeat) = self.template.repeat.as_ref().filter(|repeat| repeat.fit) {
                let mut coverage = vec![0i32; count + 1];
                for item in &placement.items {
                    let (start, span) = if self.row {
                        (item.row, item.rows)
                    } else {
                        (item.column, item.columns)
                    };
                    coverage[start] += 1;
                    coverage[start + span] -= 1;
                }
                let mut occupied = 0;
                let range = repeat.range.start as i32..self.repeat_end as i32;
                coverage[..count]
                    .iter()
                    .enumerate()
                    .map(|(index, delta)| {
                        occupied += delta;
                        occupied == 0 && range.contains(&(index as i32 + origin))
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
        let tracks = (0..count)
            .map(|index| {
                if collapsed.get(index) == Some(&true) {
                    let zero = GridBreadth::Length(Length::Px(0.));
                    return GridTrack {
                        min: zero,
                        max: zero,
                    };
                }
                let grid_index = index as i32 + origin;
                if grid_index >= 0
                    && let Some(track) = self.track(grid_index as usize)
                {
                    return track;
                }
                let pattern_index = if grid_index < 0 {
                    grid_index
                } else {
                    grid_index - self.count as i32
                };
                automatic
                    .get(pattern_index.rem_euclid(automatic.len().max(1) as i32) as usize)
                    .copied()
                    .unwrap_or_default()
            })
            .collect();
        let active = count - collapsed.iter().filter(|&&collapsed| collapsed).count();
        Tracks {
            tracks,
            collapsed,
            active,
        }
    }
}

fn includes(names: &[Arc<str>], name: &str) -> bool {
    names
        .binary_search_by(|entry| entry.as_ref().cmp(name))
        .is_ok()
}

fn repeat_count(template: &GridTemplate, size: AxisSize, gap: Length, font: f32) -> (usize, bool) {
    let repeat = template.repeat.as_ref().expect("automatic repetition");
    let (available, minimum) = if let Some(size) = size.preferred {
        (size, false)
    } else if size.maximum.is_finite() {
        (size.maximum, false)
    } else if size.minimum > 0. {
        (size.minimum, true)
    } else {
        return (1, false);
    };
    // A maximum constrains how many repetitions fit without establishing a
    // preferred percentage basis. Use the definite size, or the minimum size
    // during intrinsic measurement.
    let basis = size.preferred.unwrap_or(size.minimum);
    let breadth = |breadth: GridBreadth| match breadth {
        GridBreadth::Length(length) => length
            .resolve(basis, font)
            .map(|n| f64::from(n.clamp(0., 1e8))),
        _ => None,
    };
    let mut outside = 0.;
    let mut pattern = 0.;
    for (index, track) in template.tracks.iter().enumerate() {
        let min = breadth(track.min);
        let Some(used) = breadth(track.max)
            .map(|max| max.max(min.unwrap_or(0.)))
            .or(min)
        else {
            return (1, false);
        };
        if repeat.range.contains(&index) {
            pattern += used.max(1.);
        } else {
            outside += used;
        }
    }
    let gap = f64::from(
        gap.resolve(size.preferred.unwrap_or(0.), font)
            .unwrap_or(0.)
            .clamp(0., 1e8),
    );
    let pattern_len = repeat.range.len();
    let outside_len = template.tracks.len() - pattern_len;
    let budget = f64::from(available) + gap - outside - gap * outside_len as f64;
    let per_repeat = pattern + gap * pattern_len as f64;
    let copies = budget / per_repeat;
    let copies = if minimum {
        copies.ceil()
    } else {
        copies.floor()
    }
    .max(1.) as usize;
    let max = (GRID_TRACK_LIMIT - outside_len) / pattern_len;
    (copies.min(max), copies > max)
}

pub(super) struct Tracks {
    tracks: Vec<GridTrack>,
    collapsed: Vec<bool>,
    pub(super) active: usize,
}

impl Tracks {
    pub(super) fn needs_contributions(&self, basis: Option<f32>, font: f32) -> bool {
        self.iter().any(|track| [track.min, track.max].iter().any(|breadth| {
            !matches!(breadth, GridBreadth::Length(length) if length.resolve_with_basis(basis, font).is_some())
        }))
    }
    pub(super) fn gaps(&self, gap: f32) -> f32 {
        gap * self.active.saturating_sub(1) as f32
    }
    pub(super) fn collapsed(&self, index: usize) -> bool {
        self.collapsed.get(index) == Some(&true)
    }
}

impl Deref for Tracks {
    type Target = [GridTrack];
    fn deref(&self) -> &Self::Target {
        &self.tracks
    }
}
