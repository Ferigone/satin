//! SVG path grammar, endpoint arcs and adaptive curve flattening. Curves are
//! tested in viewport coordinates, so small icons and large viewBoxes receive
//! the same bounded geometric tolerance.
use super::{
    Budget,
    geometry::{Contour, FlatPath, Numbers, Point, SvgTransform},
};
use std::f64::consts::{PI, TAU};

const TOLERANCE: f64 = 0.1;
#[derive(Default)]
pub(crate) struct Writer {
    pub path: FlatPath,
    pub current: Point,
    start: Point,
    active: bool,
}
impl Writer {
    pub fn move_to(&mut self, p: Point, budget: &mut Budget) -> bool {
        if !budget.segment() {
            return false;
        }
        self.path.contours.push(Contour {
            points: vec![p],
            closed: false,
        });
        self.current = p;
        self.start = p;
        self.active = true;
        true
    }
    pub fn line_to(&mut self, p: Point, budget: &mut Budget) -> bool {
        if !p.valid() {
            budget.diagnostics.invalid_geometry = true;
            return false;
        }
        if !self.active && !self.move_to(self.current, budget) {
            return false;
        }
        if !budget.segment() {
            return false;
        }
        self.path.contours.last_mut().unwrap().points.push(p);
        self.current = p;
        true
    }
    pub fn close(&mut self) {
        if self.active {
            self.path.contours.last_mut().unwrap().closed = true;
            self.current = self.start;
            self.active = false;
        }
    }
    pub fn cubic(
        &mut self,
        p1: Point,
        p2: Point,
        p3: Point,
        t: SvgTransform,
        budget: &mut Budget,
    ) -> bool {
        let mut pending = vec![(self.current, p1, p2, p3, 0u8)];
        while let Some((a, b, c, d, depth)) = pending.pop() {
            if !budget.work(8) {
                return false;
            }
            let (aa, bb, cc, dd) = (t.map(a), t.map(b), t.map(c), t.map(d));
            let chord = dd.minus(aa);
            let length = chord.length();
            let distance = if length > 1e-12 {
                bb.minus(aa)
                    .cross(chord)
                    .abs()
                    .max(cc.minus(aa).cross(chord).abs())
                    / length
            } else {
                bb.minus(aa).length().max(cc.minus(aa).length())
            };
            let excess =
                bb.minus(aa).length() + cc.minus(bb).length() + dd.minus(cc).length() - length;
            if distance <= TOLERANCE && excess <= TOLERANCE || depth == 16 {
                if depth == 16 && (distance > TOLERANCE || excess > TOLERANCE) {
                    budget.diagnostics.truncated = true;
                }
                if !self.line_to(d, budget) {
                    return false;
                }
            } else {
                let ab = a.plus(b).times(0.5);
                let bc = b.plus(c).times(0.5);
                let cd = c.plus(d).times(0.5);
                let abc = ab.plus(bc).times(0.5);
                let bcd = bc.plus(cd).times(0.5);
                let mid = abc.plus(bcd).times(0.5);
                pending.push((mid, bcd, cd, d, depth + 1));
                pending.push((a, ab, abc, mid, depth + 1));
            }
        }
        true
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "SVG endpoint arc parameters follow the standard's seven-value command"
    )]
    pub fn arc(
        &mut self,
        mut rx: f64,
        mut ry: f64,
        angle: f64,
        large: bool,
        sweep: bool,
        end: Point,
        t: SvgTransform,
        budget: &mut Budget,
    ) -> bool {
        let start = self.current;
        rx = rx.abs();
        ry = ry.abs();
        if start == end {
            return true;
        }
        if rx == 0. || ry == 0. {
            return self.line_to(end, budget);
        }
        let (s, c) = angle.to_radians().sin_cos();
        let half = start.minus(end).times(0.5);
        let x = c * half.x + s * half.y;
        let y = -s * half.x + c * half.y;
        let radii = x * x / (rx * rx) + y * y / (ry * ry);
        if radii > 1. {
            let factor = radii.sqrt();
            rx *= factor;
            ry *= factor;
        }
        let numerator = (rx * rx * ry * ry - rx * rx * y * y - ry * ry * x * x).max(0.);
        let denominator = rx * rx * y * y + ry * ry * x * x;
        let factor = if denominator > 0. {
            (numerator / denominator).sqrt() * if large == sweep { -1. } else { 1. }
        } else {
            0.
        };
        let cx = factor * rx * y / ry;
        let cy = -factor * ry * x / rx;
        let center = Point::new(
            c * cx - s * cy + (start.x + end.x) / 2.,
            s * cx + c * cy + (start.y + end.y) / 2.,
        );
        let u = Point::new((x - cx) / rx, (y - cy) / ry);
        let v = Point::new((-x - cx) / rx, (-y - cy) / ry);
        let theta = u.y.atan2(u.x);
        let mut delta = u.cross(v).atan2(u.dot(v));
        if !sweep && delta > 0. {
            delta -= TAU;
        } else if sweep && delta < 0. {
            delta += TAU;
        }
        let ellipse = |a: f64| {
            let (sa, ca) = a.sin_cos();
            Point::new(
                center.x + rx * ca * c - ry * sa * s,
                center.y + rx * ca * s + ry * sa * c,
            )
        };
        if !center.valid() || !delta.is_finite() {
            budget.diagnostics.invalid_geometry = true;
            return false;
        }
        let pieces = (delta.abs() / (PI / 4.)).ceil().max(1.) as usize;
        for piece in 0..pieces {
            let from = theta + delta * piece as f64 / pieces as f64;
            let to = theta + delta * (piece + 1) as f64 / pieces as f64;
            let mut pending = vec![(from, to, 0u8)];
            while let Some((a, b, depth)) = pending.pop() {
                if !budget.work(5) {
                    return false;
                }
                let pa = t.map(ellipse(a));
                let pb = t.map(ellipse(b));
                let mid = (a + b) / 2.;
                let pm = t.map(ellipse(mid));
                let error = pm.minus(pa.plus(pb).times(0.5)).length();
                if error <= TOLERANCE || depth == 16 {
                    if depth == 16 && error > TOLERANCE {
                        budget.diagnostics.truncated = true;
                    }
                    let point = if piece + 1 == pieces && b == to {
                        end
                    } else {
                        ellipse(b)
                    };
                    if !self.line_to(point, budget) {
                        return false;
                    }
                } else {
                    pending.push((mid, b, depth + 1));
                    pending.push((a, mid, depth + 1));
                }
            }
        }
        true
    }
}

pub(crate) fn parse(source: &str, t: SvgTransform, budget: &mut Budget) -> FlatPath {
    let mut n = Numbers::new(source);
    let mut w = Writer::default();
    let mut command = None;
    let mut cubic = None;
    let mut quadratic = None;
    let mut started = false;
    while !n.done() {
        if !budget.work(1) {
            break;
        }
        if n.source
            .as_bytes()
            .get(n.pos)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            command = Some(n.source.as_bytes()[n.pos]);
            n.pos += 1;
            n.reset_separator();
        }
        let Some(c) = command else {
            budget.diagnostics.invalid_geometry = true;
            break;
        };
        let upper = c.to_ascii_uppercase();
        let relative = c.is_ascii_lowercase();
        let from = w.current;
        if !started && upper != b'M' {
            budget.diagnostics.invalid_geometry = true;
            break;
        }
        let pair = |n: &mut Numbers<'_>, budget: &mut Budget| -> Option<Point> {
            let p = Point::new(n.number(budget)?, n.number(budget)?);
            Some(if relative { p.plus(from) } else { p })
        };
        let result = (|| -> Option<bool> {
            Some(match upper {
                b'M' => {
                    let p = pair(&mut n, budget)?;
                    started = true;
                    command = Some(if relative { b'l' } else { b'L' });
                    w.move_to(p, budget)
                }
                b'L' => w.line_to(pair(&mut n, budget)?, budget),
                b'H' => {
                    let x = n.number(budget)?;
                    w.line_to(
                        Point::new(if relative { from.x + x } else { x }, from.y),
                        budget,
                    )
                }
                b'V' => {
                    let y = n.number(budget)?;
                    w.line_to(
                        Point::new(from.x, if relative { from.y + y } else { y }),
                        budget,
                    )
                }
                b'C' => {
                    let a = pair(&mut n, budget)?;
                    let b = pair(&mut n, budget)?;
                    let end = pair(&mut n, budget)?;
                    cubic = Some(b);
                    w.cubic(a, b, end, t, budget)
                }
                b'S' => {
                    let a = cubic.map_or(from, |p: Point| from.times(2.).minus(p));
                    let b = pair(&mut n, budget)?;
                    let end = pair(&mut n, budget)?;
                    cubic = Some(b);
                    w.cubic(a, b, end, t, budget)
                }
                b'Q' => {
                    let q = pair(&mut n, budget)?;
                    let end = pair(&mut n, budget)?;
                    quadratic = Some(q);
                    w.cubic(
                        from.plus(q.minus(from).times(2. / 3.)),
                        end.plus(q.minus(end).times(2. / 3.)),
                        end,
                        t,
                        budget,
                    )
                }
                b'T' => {
                    let q = quadratic.map_or(from, |p: Point| from.times(2.).minus(p));
                    let end = pair(&mut n, budget)?;
                    quadratic = Some(q);
                    w.cubic(
                        from.plus(q.minus(from).times(2. / 3.)),
                        end.plus(q.minus(end).times(2. / 3.)),
                        end,
                        t,
                        budget,
                    )
                }
                b'A' => {
                    let rx = n.number(budget)?;
                    let ry = n.number(budget)?;
                    let angle = n.number(budget)?;
                    let large = n.flag(budget)?;
                    let sweep = n.flag(budget)?;
                    let end = pair(&mut n, budget)?;
                    w.arc(rx, ry, angle, large, sweep, end, t, budget)
                }
                b'Z' => {
                    w.close();
                    command = None;
                    true
                }
                _ => return None,
            })
        })();
        if !matches!(upper, b'C' | b'S') {
            cubic = None;
        }
        if !matches!(upper, b'Q' | b'T') {
            quadratic = None;
        }
        if result != Some(true) {
            if !budget.diagnostics.truncated {
                budget.diagnostics.invalid_geometry = true;
            }
            break;
        }
    }
    w.path
}

pub(crate) fn ellipse(
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    t: SvgTransform,
    budget: &mut Budget,
) -> FlatPath {
    let mut w = Writer::default();
    if rx <= 0. || ry <= 0. {
        return w.path;
    }
    if !w.move_to(Point::new(cx + rx, cy), budget) {
        return w.path;
    }
    for p in [
        Point::new(cx, cy + ry),
        Point::new(cx - rx, cy),
        Point::new(cx, cy - ry),
        Point::new(cx + rx, cy),
    ] {
        if !w.arc(rx, ry, 0., false, true, p, t, budget) {
            break;
        }
    }
    w.close();
    w.path
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_commands_and_compact_arc_flags() {
        let mut b = Budget::default();
        let p = parse(
            "M1 2h3v4l2-1q4 0 4 4t4 4c1 2 3 4 5 6s1 2 3 4a2 3 45 0110 20z",
            SvgTransform::IDENTITY,
            &mut b,
        );
        assert!(!b.diagnostics.invalid_geometry);
        assert_eq!(p.contours.len(), 1);
        assert!(p.contours[0].closed);
        assert!(p.contours[0].points.len() > 12);
    }
    #[test]
    fn errors_keep_only_complete_segments() {
        let mut b = Budget::default();
        let p = parse("M0 0 L10 10 L20", SvgTransform::IDENTITY, &mut b);
        assert!(b.diagnostics.invalid_geometry);
        assert_eq!(p.contours[0].points.len(), 2);
    }
}
