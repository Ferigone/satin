//! Four subscanlines with analytic horizontal coverage. Group opacity uses
//! bounded transparent scanline layers; no SVG-sized surface is allocated.
use super::{
    geometry::{FlatPath, Point},
    scene::{Clip, Command, Paint, Spread, Stop},
    *,
};
use std::{collections::HashMap, sync::Arc};

const SAMPLES: usize = 4;
const TILE_WIDTH: usize = 4096;
type Pixel = [f32; 4];
#[derive(Clone, Copy)]
struct Edge {
    low: f64,
    high: f64,
    x: f64,
    slope: f64,
    winding: i32,
}
pub(crate) struct DevicePath {
    edges: Vec<Edge>,
    rule: FillRule,
}
struct DeviceClip {
    paths: Vec<DevicePath>,
}
enum Prepared {
    Push(f32),
    Pop,
    Paint {
        path: DevicePath,
        paint: Paint,
        clips: Vec<Arc<DeviceClip>>,
        bounds: SvgRect,
    },
}

pub(super) fn paint(
    scene: &SvgScene,
    pixels: &mut [u32],
    width: u32,
    height: u32,
    target: SvgRect,
    clip: SvgRect,
) -> SvgRenderReport {
    if !target.valid() || scene.width <= 0. || scene.height <= 0. {
        return SvgRenderReport::default();
    }
    paint_transformed(
        scene,
        pixels,
        width,
        height,
        [
            target.width / scene.width,
            0.,
            0.,
            target.height / scene.height,
            target.x,
            target.y,
        ],
        clip,
        |x, y| clip.contains(x, y),
    )
}
pub(super) fn paint_transformed<F: Fn(f32, f32) -> bool>(
    scene: &SvgScene,
    pixels: &mut [u32],
    width: u32,
    height: u32,
    m: [f32; 6],
    clip: SvgRect,
    inside: F,
) -> SvgRenderReport {
    let mut report = SvgRenderReport::default();
    if (width as usize)
        .checked_mul(height as usize)
        .is_none_or(|size| size > pixels.len())
        || !clip.valid()
        || m.iter().any(|v| !v.is_finite())
    {
        report.invalid_surface = true;
        return report;
    }
    if width == 0 || height == 0 || scene.width <= 0. || scene.height <= 0. {
        return report;
    }
    let transform = SvgTransform {
        a: m[0] as f64,
        b: m[1] as f64,
        c: m[2] as f64,
        d: m[3] as f64,
        e: m[4] as f64,
        f: m[5] as f64,
    };
    let Some(inverse) = transform.inverse() else {
        return report;
    };
    let clip = clip
        .intersect(SvgRect {
            x: 0.,
            y: 0.,
            width: width as f32,
            height: height as f32,
        })
        .intersect(transform.rect(SvgRect {
            x: 0.,
            y: 0.,
            width: scene.width,
            height: scene.height,
        }));
    if clip.width <= 0. || clip.height <= 0. {
        return report;
    }
    let mut prepared = vec![];
    let mut cached = HashMap::new();
    for command in &scene.commands {
        if !work(&mut report, 1) {
            return report;
        }
        match command {
            Command::PushOpacity(alpha) => prepared.push(Prepared::Push(*alpha)),
            Command::PopOpacity => prepared.push(Prepared::Pop),
            Command::Paint {
                path,
                paint,
                clips,
                bounds,
                ..
            } => {
                let bounds = transform.rect(*bounds);
                if bounds.intersect(clip).width <= 0. || bounds.intersect(clip).height <= 0. {
                    continue;
                }
                let Some(path) = device_path(path, transform, FillRule::NonZero, &mut report)
                else {
                    return report;
                };
                let mut cp = vec![];
                for c in clips {
                    let key = Arc::as_ptr(c) as usize;
                    let clip = if let Some(c) = cached.get(&key) {
                        Arc::clone(c)
                    } else {
                        let mut paths = vec![];
                        for (p, r) in &c.paths {
                            let Some(p) = device_path(p, transform, *r, &mut report) else {
                                return report;
                            };
                            paths.push(p);
                        }
                        let c = Arc::new(DeviceClip { paths });
                        cached.insert(key, Arc::clone(&c));
                        c
                    };
                    cp.push(clip);
                }
                let rule = match command {
                    Command::Paint { rule, .. } => *rule,
                    _ => unreachable!(),
                };
                prepared.push(Prepared::Paint {
                    path: DevicePath { rule, ..path },
                    paint: shader(paint, inverse),
                    clips: cp,
                    bounds,
                });
            }
        }
    }
    let left = clip.x.floor().max(0.) as usize;
    let right = (clip.x + clip.width).ceil().min(width as f32) as usize;
    let top = clip.y.floor().max(0.) as usize;
    let bottom = (clip.y + clip.height).ceil().min(height as f32) as usize;
    let mut coverage = vec![0f32; (right - left).min(TILE_WIDTH)];
    let mut pool: Vec<Vec<Pixel>> = vec![];
    'rows: for y in top..bottom {
        for first in (left..right).step_by(TILE_WIDTH) {
            let last = (first + TILE_WIDTH).min(right);
            let size = last - first;
            if !work(&mut report, size.saturating_mul(2)) {
                break 'rows;
            }
            let root = (first..last)
                .map(|x| {
                    let p = pixels[y * width as usize + x];
                    [
                        ((p >> 16) & 255) as f32 / 255.,
                        ((p >> 8) & 255) as f32 / 255.,
                        (p & 255) as f32 / 255.,
                        1.,
                    ]
                })
                .collect::<Vec<_>>();
            let mut layers = vec![(root, 1f32)];
            for command in &prepared {
                if !work(&mut report, 1) {
                    break;
                }
                match command {
                    Prepared::Push(opacity) => {
                        if layers.len() > MAX_SVG_DEPTH {
                            report.truncated = true;
                            break;
                        }
                        let mut row = pool.pop().unwrap_or_default();
                        row.resize(size, [0.; 4]);
                        row.fill([0.; 4]);
                        layers.push((row, *opacity));
                    }
                    Prepared::Pop => {
                        if layers.len() <= 1 {
                            report.invalid_surface = true;
                            break;
                        }
                        if !work(&mut report, size) {
                            break;
                        }
                        let (row, opacity) = layers.pop().unwrap();
                        let parent = &mut layers.last_mut().unwrap().0;
                        for (dst, src) in parent.iter_mut().zip(&row) {
                            blend(
                                dst,
                                [
                                    src[0] * opacity,
                                    src[1] * opacity,
                                    src[2] * opacity,
                                    src[3] * opacity,
                                ],
                            );
                        }
                        pool.push(row);
                    }
                    Prepared::Paint {
                        path,
                        paint,
                        clips,
                        bounds,
                    } => {
                        if y as f32 + 1. < bounds.y
                            || y as f32 >= bounds.y + bounds.height
                            || last as f32 <= bounds.x
                            || first as f32 >= bounds.x + bounds.width
                        {
                            continue;
                        }
                        coverage[..size].fill(0.);
                        for sample in 0..SAMPLES {
                            let sy = y as f64 + (sample as f64 + 0.5) / SAMPLES as f64;
                            if sy < clip.y as f64 || sy >= f64::from(clip.y + clip.height) {
                                continue;
                            }
                            let Some(mut spans) = intervals(path, sy, &mut report) else {
                                break;
                            };
                            for clip in clips {
                                let mut clipped = vec![];
                                for path in &clip.paths {
                                    let Some(values) = intervals(path, sy, &mut report) else {
                                        break;
                                    };
                                    clipped.extend(values);
                                }
                                let Some(clipped) = union(clipped, &mut report) else {
                                    break;
                                };
                                spans = intersection(&spans, &clipped);
                            }
                            for (a, b) in spans {
                                let a = a.max(first as f64).max(clip.x as f64);
                                let b = b.min(last as f64).min(f64::from(clip.x + clip.width));
                                if b <= a {
                                    continue;
                                }
                                let start = a.floor() as usize;
                                let end = b.ceil() as usize;
                                if !work(&mut report, end - start) {
                                    break;
                                }
                                for x in start..end {
                                    coverage[x - first] +=
                                        ((b.min(x as f64 + 1.) - a.max(x as f64)).max(0.)
                                            / SAMPLES as f64)
                                            as f32;
                                }
                            }
                        }
                        if report.truncated {
                            break;
                        }
                        let row = &mut layers.last_mut().unwrap().0;
                        for (index, &coverage) in coverage[..size].iter().enumerate() {
                            if coverage <= 0. {
                                continue;
                            }
                            let x = first + index;
                            if !inside(x as f32 + 0.5, y as f32 + 0.5) {
                                continue;
                            }
                            if report.pixels >= MAX_SVG_PAINT_PIXELS
                                || !work(&mut report, shader_work(paint))
                            {
                                report.truncated = true;
                                break;
                            }
                            let c = color(paint, Point::new(x as f64 + 0.5, y as f64 + 0.5));
                            let alpha = f32::from(c.3) / 255. * coverage.clamp(0., 1.);
                            blend(
                                &mut row[index],
                                [
                                    f32::from(c.0) / 255. * alpha,
                                    f32::from(c.1) / 255. * alpha,
                                    f32::from(c.2) / 255. * alpha,
                                    alpha,
                                ],
                            );
                            report.pixels += 1;
                        }
                    }
                }
                if report.truncated || report.invalid_surface {
                    break;
                }
            }
            // On a resource stop, discard unfinished isolated layers, preserving
            // the caller's pixels rather than publishing an uncomposited group.
            let row = &layers[0].0;
            for (index, p) in row.iter().enumerate() {
                pixels[y * width as usize + first + index] =
                    ((p[0].clamp(0., 1.) * 255.).round() as u32) << 16
                        | ((p[1].clamp(0., 1.) * 255.).round() as u32) << 8
                        | (p[2].clamp(0., 1.) * 255.).round() as u32;
            }
            while let Some((row, _)) = layers.pop() {
                pool.push(row);
            }
            if report.truncated || report.invalid_surface {
                break 'rows;
            }
        }
    }
    report
}
fn work(r: &mut SvgRenderReport, n: usize) -> bool {
    if r.work.saturating_add(n) > MAX_SVG_RENDER_WORK {
        r.truncated = true;
        false
    } else {
        r.work += n;
        true
    }
}
pub(crate) fn device_path(
    path: &FlatPath,
    t: SvgTransform,
    rule: FillRule,
    r: &mut SvgRenderReport,
) -> Option<DevicePath> {
    let mut edges = vec![];
    for c in &path.contours {
        if !work(r, c.points.len()) {
            return None;
        }
        if c.points.len() < 2 {
            continue;
        }
        for (a, b) in c
            .points
            .iter()
            .zip(c.points.iter().cycle().skip(1))
            .take(c.points.len())
        {
            let a = t.map(*a);
            let b = t.map(*b);
            if !a.valid() || !b.valid() {
                r.invalid_surface = true;
                return None;
            }
            if (a.y - b.y).abs() < 1e-15 {
                continue;
            }
            let (low, high, winding) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
            edges.push(Edge {
                low: low.y,
                high: high.y,
                x: low.x,
                slope: (high.x - low.x) / (high.y - low.y),
                winding,
            });
        }
    }
    Some(DevicePath { edges, rule })
}
pub(crate) fn intervals(
    path: &DevicePath,
    y: f64,
    r: &mut SvgRenderReport,
) -> Option<Vec<(f64, f64)>> {
    if !work(r, path.edges.len()) {
        return None;
    }
    let mut events = path
        .edges
        .iter()
        .filter(|e| y >= e.low && y < e.high)
        .map(|e| (e.x + (y - e.low) * e.slope, e.winding))
        .collect::<Vec<_>>();
    if !work(
        r,
        events
            .len()
            .saturating_mul(events.len().checked_ilog2().unwrap_or(0) as usize + 1),
    ) {
        return None;
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut winding, mut start) = (0, 0.);
    let mut spans = vec![];
    for (x, direction) in events {
        let before = match path.rule {
            FillRule::NonZero => winding != 0,
            FillRule::EvenOdd => winding % 2 != 0,
        };
        winding += direction;
        let after = match path.rule {
            FillRule::NonZero => winding != 0,
            FillRule::EvenOdd => winding % 2 != 0,
        };
        if !before && after {
            start = x;
        } else if before && !after && x > start {
            spans.push((start, x));
        }
    }
    Some(spans)
}
fn union(mut spans: Vec<(f64, f64)>, r: &mut SvgRenderReport) -> Option<Vec<(f64, f64)>> {
    if !work(
        r,
        spans
            .len()
            .saturating_mul(spans.len().checked_ilog2().unwrap_or(0) as usize + 1),
    ) {
        return None;
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = vec![];
    for (a, b) in spans {
        if let Some(last) = out.last_mut()
            && a <= last.1
        {
            last.1 = last.1.max(b);
            continue;
        }
        out.push((a, b));
    }
    Some(out)
}
pub(crate) fn intersection(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (mut i, mut j) = (0, 0);
    let mut out = vec![];
    while i < a.len() && j < b.len() {
        let l = a[i].0.max(b[j].0);
        let r = a[i].1.min(b[j].1);
        if r > l {
            out.push((l, r));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}
fn blend(dst: &mut Pixel, src: Pixel) {
    let inverse = 1. - src[3];
    for i in 0..4 {
        dst[i] = src[i] + dst[i] * inverse;
    }
}
fn shader(paint: &Paint, device_inverse: SvgTransform) -> Paint {
    match paint {
        Paint::Solid(c) => Paint::Solid(*c),
        Paint::Linear {
            inverse,
            from,
            to,
            stops,
            spread,
            opacity,
        } => Paint::Linear {
            inverse: inverse.multiply(device_inverse),
            from: *from,
            to: *to,
            stops: Arc::clone(stops),
            spread: *spread,
            opacity: *opacity,
        },
        Paint::Radial {
            inverse,
            center,
            focus,
            radius,
            stops,
            spread,
            opacity,
        } => Paint::Radial {
            inverse: inverse.multiply(device_inverse),
            center: *center,
            focus: *focus,
            radius: *radius,
            stops: Arc::clone(stops),
            spread: *spread,
            opacity: *opacity,
        },
    }
}
fn color(paint: &Paint, p: Point) -> Color {
    match paint {
        Paint::Solid(c) => *c,
        Paint::Linear {
            inverse,
            from,
            to,
            stops,
            spread,
            opacity,
        } => {
            let p = inverse.map(p);
            let d = to.minus(*from);
            let divisor = d.dot(d);
            let v = if divisor > 1e-20 {
                p.minus(*from).dot(d) / divisor
            } else {
                1.
            };
            alpha(sample(stops, v, *spread), *opacity)
        }
        Paint::Radial {
            inverse,
            center,
            focus,
            radius,
            stops,
            spread,
            opacity,
        } => {
            let p = inverse.map(p);
            let d = p.minus(*focus);
            let f = focus.minus(*center);
            let a = d.dot(d);
            let b = 2. * d.dot(f);
            let c = f.dot(f) - radius * radius;
            let v = if a < 1e-20 {
                0.
            } else {
                let t = (-b + (b * b - 4. * a * c).max(0.).sqrt()) / (2. * a);
                if t > 0. { 1. / t } else { 1. }
            };
            alpha(sample(stops, v, *spread), *opacity)
        }
    }
}
fn shader_work(paint: &Paint) -> usize {
    match paint {
        Paint::Solid(_) => 1,
        Paint::Linear { stops, .. } | Paint::Radial { stops, .. } => {
            3 + stops.len().checked_ilog2().unwrap_or(0) as usize
        }
    }
}
fn sample(stops: &[Stop], value: f64, spread: Spread) -> Color {
    let v = match spread {
        Spread::Pad => value.clamp(0., 1.),
        Spread::Repeat => value.rem_euclid(1.),
        Spread::Reflect => {
            let n = value.rem_euclid(2.);
            if n <= 1. { n } else { 2. - n }
        }
    };
    let next = stops.partition_point(|stop| stop.offset <= v);
    if next == 0 {
        return stops[0].color;
    }
    if next >= stops.len() {
        return stops.last().unwrap().color;
    }
    let a = &stops[next - 1];
    let b = &stops[next];
    let t = ((v - a.offset) / (b.offset - a.offset)) as f32;
    let mix = |a: u8, b: u8| (f32::from(a) * (1. - t) + f32::from(b) * t).round() as u8;
    Color(
        mix(a.color.0, b.color.0),
        mix(a.color.1, b.color.1),
        mix(a.color.2, b.color.2),
        mix(a.color.3, b.color.3),
    )
}
fn point_inside(path: &FlatPath, rule: FillRule, p: Point, remaining: &mut usize) -> bool {
    let mut winding = 0i32;
    for c in &path.contours {
        if *remaining < c.points.len() {
            *remaining = 0;
            return false;
        }
        *remaining -= c.points.len();
        if c.points.len() < 2 {
            continue;
        }
        for (a, b) in c
            .points
            .iter()
            .zip(c.points.iter().cycle().skip(1))
            .take(c.points.len())
        {
            if a.y <= p.y && p.y < b.y {
                if (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y) > 0. {
                    winding += 1;
                }
            } else if b.y <= p.y
                && p.y < a.y
                && (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y) < 0.
            {
                winding -= 1;
            }
        }
    }
    match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding % 2 != 0,
    }
}
fn in_clip(clip: &Clip, p: Point, remaining: &mut usize) -> bool {
    clip.paths
        .iter()
        .any(|(path, rule)| point_inside(path, *rule, p, remaining))
}
pub(super) fn hit_test(scene: &SvgScene, x: f32, y: f32) -> Option<NodeId> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let p = Point::new(x as f64, y as f64);
    let mut remaining = MAX_SVG_RENDER_WORK;
    for command in scene.commands.iter().rev() {
        if let Command::Paint {
            node,
            path,
            rule,
            clips,
            bounds,
            ..
        } = command
            && bounds.contains(x, y)
            && clips.iter().all(|c| in_clip(c, p, &mut remaining))
            && point_inside(path, *rule, p, &mut remaining)
        {
            return Some(*node);
        }
        if remaining == 0 {
            break;
        }
    }
    None
}
