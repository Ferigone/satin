use super::{Bounds, Composite, Draw, Plan, SAMPLES, Shape, limit, span_coverage};
use crate::svg::{Point, SvgRenderReport};
use image::{GrayImage, RgbaImage, imageops};
use rust_browser_javascript::{JsError, Vm};

const MAX_SIGMA: f64 = 16.;
const MAX_MASK_PIXELS: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Default)]
pub(in crate::script::canvas::drawing) struct Shadow {
    pub color: [u8; 4],
    pub offset: Point,
    pub blur: f64,
}

impl Shadow {
    fn enabled(self, draw: Draw<'_>) -> bool {
        !draw.clear
            && draw.operator != Composite::Copy
            && self.color[3] != 0
            && (self.blur != 0. || self.offset != Point::default())
    }

    fn shade(self, mask_alpha: u8) -> [u8; 4] {
        let [r, g, b, alpha] = self.color;
        let combined = (u16::from(alpha) * u16::from(mask_alpha) + 127) / 255;
        [r, g, b, combined as u8]
    }
}

impl Bounds {
    fn translated(self, p: Point) -> Self {
        Self {
            left: self.left + p.x,
            top: self.top + p.y,
            right: self.right + p.x,
            bottom: self.bottom + p.y,
        }
    }

    fn expanded(self, distance: f64) -> Self {
        Self {
            left: self.left - distance,
            top: self.top - distance,
            right: self.right + distance,
            bottom: self.bottom + distance,
        }
    }
}

pub(super) struct ShadowPlan {
    plan: Plan,
    mask: Option<Mask>,
}

struct Mask {
    pixels: GrayImage,
    left: i32,
    top: i32,
}

impl ShadowPlan {
    pub fn paint(
        self,
        bitmap: &mut RgbaImage,
        draw: Draw<'_>,
        opaque: bool,
        sample: impl FnMut(f64, f64) -> [u8; 4],
    ) -> bool {
        let shadow = draw.shadow;
        let mut sample = sample;
        self.plan.paint(bitmap, draw, opaque, |x, y| {
            let alpha = if let Some(mask) = &self.mask {
                let x = x.floor() as i32 - mask.left;
                let y = y.floor() as i32 - mask.top;
                mask.pixels.get_pixel(x as u32, y as u32).0[0]
            } else {
                sample(x - shadow.offset.x, y - shadow.offset.y)[3]
            };
            shadow.shade(alpha)
        })
    }
}

pub(super) fn prepare(
    vm: &mut Vm,
    clip: Bounds,
    shape: &Shape,
    draw: Draw<'_>,
    sample: &mut impl FnMut(f64, f64) -> [u8; 4],
) -> Result<Option<ShadowPlan>, JsError> {
    let shadow = draw.shadow;
    if !shadow.enabled(draw) || clip.empty() {
        return Ok(None);
    }
    let shifted = Shape {
        bounds: shape.bounds.translated(shadow.offset),
        offset: shadow.offset,
        ..shape.clone()
    };
    let sigma = (shadow.blur / 2.).min(MAX_SIGMA);
    if sigma == 0. {
        return Ok(
            Plan::prepare(vm, clip, &shifted, draw)?.map(|plan| ShadowPlan { plan, mask: None })
        );
    }
    // Reserve a transparent margin past the visible blur so mask boundaries
    // cannot renormalize the Gaussian kernel at the canvas edge.
    let radius = (3. * sigma).ceil() + 1.;
    let visible = clip.intersect(shifted.bounds.expanded(radius));
    if visible.empty() {
        return Ok(
            Plan::prepare(vm, clip, &shifted, draw)?.map(|plan| ShadowPlan { plan, mask: None })
        );
    }
    let [left, top, right, bottom] = visible.pixels().map(f64::from);
    let output = Shape {
        bounds: Bounds {
            left,
            top,
            right,
            bottom,
        },
        path: None,
        edges: 0,
        offset: Point::default(),
    };
    let Some(plan) = Plan::prepare(vm, clip, &output, draw)? else {
        return Ok(None);
    };
    let extent = output.bounds.expanded(radius);
    let mask = create_mask(vm, &shifted, extent, sigma, sample)?;
    Ok(Some(ShadowPlan {
        plan,
        mask: Some(mask),
    }))
}

fn create_mask(
    vm: &mut Vm,
    shape: &Shape,
    extent: Bounds,
    sigma: f64,
    sample: &mut impl FnMut(f64, f64) -> [u8; 4],
) -> Result<Mask, JsError> {
    let left = extent.left.floor() as i32;
    let top = extent.top.floor() as i32;
    let width = (extent.right.ceil() as i32 - left) as u32;
    let height = (extent.bottom.ceil() as i32 - top) as u32;
    let area = width as usize * height as usize;
    if area > MAX_MASK_PIXELS {
        return Err(limit());
    }
    let kernel = (4. * sigma).ceil() as usize + 1;
    vm.charge_work((area * (1 + 2 * kernel)).div_ceil(16))?;
    let mut alpha = GrayImage::new(width, height);
    let mut coverage = vec![0.; width as usize];
    let mut report = SvgRenderReport::default();
    for y in 0..height {
        let global_y = f64::from(top) + f64::from(y);
        if shape.path.is_some() {
            let mut spans = Vec::new();
            for part in 0..SAMPLES {
                let py = global_y + (part as f64 + 0.5) / SAMPLES as f64;
                spans.extend(shape.intervals(py, &mut report)?);
            }
            span_coverage(&spans, &mut coverage, left);
        }
        for (x, covered) in coverage.iter().enumerate() {
            let global_x = f64::from(left) + x as f64;
            let amount = if shape.path.is_none() {
                shape.bounds.pixel_coverage(global_x, global_y)
            } else {
                covered.min(1.)
            };
            if amount > 0. {
                let a = sample(
                    global_x + 0.5 - shape.offset.x,
                    global_y + 0.5 - shape.offset.y,
                )[3];
                alpha.get_pixel_mut(x as u32, y).0[0] = (f32::from(a) * amount).round() as u8;
            }
        }
    }
    vm.charge_work(report.work.div_ceil(8))?;
    let pixels = imageops::blur(&alpha, sigma as f32);
    Ok(Mask { pixels, left, top })
}
