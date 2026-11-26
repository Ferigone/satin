use super::LoadedPage;
use image::{Rgba, RgbaImage, imageops::crop_imm};
use std::sync::{Arc, OnceLock};

const ART_URL: &str = "about:satin-mark";
const BLOOM_URL: &str = "about:satin-bloom";
const FIELD_URL: &str = "about:satin-search-surface";
const SEARCH_ICON_URL: &str = "about:satin-search-icon";
const SUBMIT_ICON_URL: &str = "about:satin-submit-icon";
const ADD_ICON_URL: &str = "about:satin-add-icon";
const SETTINGS_ICON_URL: &str = "about:satin-settings-icon";
const SITE_SPRITES: [(&str, &str, u32, u32); 4] = [
    ("github.com", "about:satin-github-icon", 467, 693),
    ("www.youtube.com", "about:satin-youtube-icon", 621, 693),
    ("www.figma.com", "about:satin-figma-icon", 773, 693),
    ("www.wikipedia.org", "about:satin-wikipedia-icon", 926, 693),
];

pub(crate) fn is_home_image(url: &str) -> bool {
    matches!(
        url,
        ART_URL
            | BLOOM_URL
            | FIELD_URL
            | SEARCH_ICON_URL
            | SUBMIT_ICON_URL
            | ADD_ICON_URL
            | SETTINGS_ICON_URL
    ) || SITE_SPRITES.iter().any(|(_, name, _, _)| *name == url)
}

/// Returns the start-page mark for an exact, known HTTPS site.
pub fn built_in_site_icon(address: &str) -> Option<&'static RgbaImage> {
    let authority = address
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    let (_, icon, _, _) = SITE_SPRITES
        .iter()
        .find(|(host, _, _, _)| *host == authority)?;
    images()
        .iter()
        .find_map(|(url, image)| (*url == *icon).then_some(image.as_ref()))
}

pub(super) fn add_to(page: &mut LoadedPage) {
    for (url, image) in images() {
        page.images.insert((*url).into(), image.clone());
    }
}

fn images() -> &'static [(&'static str, Arc<RgbaImage>)] {
    type Images = Vec<(&'static str, Arc<RgbaImage>)>;
    static IMAGES: OnceLock<Images> = OnceLock::new();
    IMAGES.get_or_init(|| {
        let mut images = vec![
            (FIELD_URL, Arc::new(search_surface())),
            (SEARCH_ICON_URL, Arc::new(search_icon())),
            (SUBMIT_ICON_URL, Arc::new(button_icon(true))),
            (ADD_ICON_URL, Arc::new(button_icon(false))),
            (SETTINGS_ICON_URL, Arc::new(settings_icon())),
        ];
        if let Ok(source) = image::load_from_memory_with_format(
            include_bytes!("../../../../assets/satin-start-reference.png"),
            image::ImageFormat::Png,
        ) {
            let source = source.into_rgba8();
            if source.dimensions() == (1586, 992) {
                // Keep the artwork above the field. All controls are laid out
                // and painted independently from this part of the reference.
                images.push((
                    ART_URL,
                    Arc::new(crop_imm(&source, 0, 122, 1586, 435).to_image()),
                ));
                images.push((BLOOM_URL, Arc::new(lower_bloom(&source))));
                images.extend(
                    SITE_SPRITES.map(|(_, name, x, y)| (name, Arc::new(site_icon(&source, x, y)))),
                );
            }
        }
        images
    })
}

fn lower_bloom(source: &RgbaImage) -> RgbaImage {
    const WIDTH: u32 = 512;
    const HEIGHT: u32 = 160;
    const EDGE: [f32; 3] = [8., 8., 9.];
    let colors: Vec<[f32; 3]> = (0..WIDTH)
        .map(|x| {
            let x = x * 1585 / (WIDTH - 1);
            let mut sum = [0.; 3];
            for y in [546, 550, 554] {
                for offset in (-24..=24).step_by(4) {
                    let neighbor = (x as i32 + offset).clamp(0, 1585) as u32;
                    let pixel = source.get_pixel(neighbor, y);
                    for channel in 0..3 {
                        sum[channel] += f32::from(pixel[channel]);
                    }
                }
            }
            sum.map(|channel| channel / 39.)
        })
        .collect();
    RgbaImage::from_fn(WIDTH, HEIGHT, |x, y| {
        let fade = (y as f32 / (HEIGHT - 1) as f32 * std::f32::consts::FRAC_PI_2)
            .cos()
            .powi(2);
        let seam = source.get_pixel(x * 1585 / (WIDTH - 1), 556);
        let smoothing = (y as f32 / 6.).min(1.);
        let color = std::array::from_fn::<_, 3, _>(|channel| {
            let upper = f32::from(seam[channel]) * (1. - smoothing)
                + colors[x as usize][channel] * smoothing;
            (EDGE[channel] + (upper - EDGE[channel]) * fade).round() as u8
        });
        Rgba([color[0], color[1], color[2], 255])
    })
}

fn site_icon(source: &RgbaImage, x: u32, y: u32) -> RgbaImage {
    let edge = source.get_pixel(x, y);
    RgbaImage::from_fn(48, 48, |dx, dy| {
        let original = source.get_pixel(x + dx, y + dy);
        let difference = (0..3)
            .map(|channel| original[channel].abs_diff(edge[channel]))
            .max()
            .unwrap_or(0);
        let alpha = ((f32::from(difference) - 4.) / 16.).clamp(0., 1.);
        Rgba([
            original[0],
            original[1],
            original[2],
            (255. * alpha).round() as u8,
        ])
    })
}

fn search_surface() -> RgbaImage {
    const WIDTH: u32 = 928;
    const HEIGHT: u32 = 67;
    let radius = HEIGHT as f32 / 2.;
    RgbaImage::from_fn(WIDTH, HEIGHT, |x, y| {
        let x = x as f32 + 0.5;
        let y = y as f32 + 0.5;
        let dx = (radius - x).max(x - (WIDTH as f32 - radius)).max(0.);
        let distance = dx.hypot(y - radius);
        let alpha = coverage(radius, distance);
        if distance >= radius - 1.25 {
            Rgba([83, 81, 80, alpha])
        } else {
            let warmth = (1. - (2. * x / WIDTH as f32 - 1.).abs()).powi(2);
            Rgba([
                (25. + 12. * warmth).round() as u8,
                (24. + 8. * warmth).round() as u8,
                (24. + 5. * warmth).round() as u8,
                alpha,
            ])
        }
    })
}

fn coverage(radius: f32, distance: f32) -> u8 {
    (255. * (radius - distance + 0.5).clamp(0., 1.)).round() as u8
}

fn line_distance(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let t = (((point.0 - a.0) * dx + (point.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0., 1.);
    (point.0 - a.0 - t * dx).hypot(point.1 - a.1 - t * dy)
}

fn search_icon() -> RgbaImage {
    RgbaImage::from_fn(32, 32, |x, y| {
        let point = (x as f32 + 0.5, y as f32 + 0.5);
        let ring = ((point.0 - 13.5).hypot(point.1 - 13.5) - 9.).abs();
        let handle = line_distance(point, (20., 20.), (27., 27.));
        Rgba([221, 217, 215, coverage(1.15, ring.min(handle))])
    })
}

fn button_icon(submit: bool) -> RgbaImage {
    const EDGE: u32 = 44;
    let segments: &[((f32, f32), (f32, f32))] = if submit {
        &[
            ((30., 14.), (30., 23.)),
            ((30., 23.), (13., 23.)),
            ((13., 23.), (19., 17.)),
            ((13., 23.), (19., 29.)),
        ]
    } else {
        &[((13., 22.), (31., 22.)), ((22., 13.), (22., 31.))]
    };
    RgbaImage::from_fn(EDGE, EDGE, |x, y| {
        let point = (x as f32 + 0.5, y as f32 + 0.5);
        let corner = ((point.0 - 22.).abs() - 12.)
            .max(0.)
            .hypot(((point.1 - 22.).abs() - 12.).max(0.));
        let distance = segments
            .iter()
            .map(|&(a, b)| line_distance(point, a, b))
            .fold(f32::INFINITY, f32::min);
        let ink = coverage(1., distance);
        if ink > 0 {
            let channel = 43 + (177 * u16::from(ink) / 255) as u8;
            Rgba([channel, channel - 1, channel - 2, 255])
        } else if corner > 8.5 {
            Rgba([80, 77, 74, coverage(10., corner)])
        } else if submit {
            Rgba([47, 45, 44, 255])
        } else {
            Rgba([33, 32, 31, 255])
        }
    })
}

fn settings_icon() -> RgbaImage {
    RgbaImage::from_fn(32, 28, |x, y| {
        let point = (x as f32 + 0.5, y as f32 + 0.5);
        let alpha = [(9., 20.), (19., 12.)]
            .into_iter()
            .map(|(row, knob)| {
                let ring = ((point.0 - knob).hypot(point.1 - row) - 3.1).abs();
                let left = line_distance(point, (3., row), (knob - 3.2, row));
                let right = line_distance(point, (knob + 3.2, row), (29., row));
                coverage(0.8, ring.min(left).min(right))
            })
            .max()
            .unwrap_or(0);
        Rgba([210, 205, 202, alpha])
    })
}
