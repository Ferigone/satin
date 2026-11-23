//! Single-threaded software paint microbenchmark; see docs/RENDER_COST.md.
use rust_browser_engine::{
    DrawCommand, Fonts, Layout, LoadedPage,
    css::{self, Color},
    fonts::{FontFace, FontFaceEntry, FontSet},
    layout::{Affine2D, Viewport},
    paint::Canvas,
};
use std::{collections::HashSet, hint::black_box, sync::Arc, time::Instant};

const HTML: &str = include_str!("../../../tools/fixtures/render-cost/gallery.html");
const FONT: &[u8] = include_bytes!("../tests/fixtures/fonts/wide.ttf");
const WIDTH: u32 = 960;
const HEIGHT: u32 = 640;
const WARM_SAMPLES: usize = 31;

fn page() -> LoadedPage {
    // Install the deterministic post-decode pixels directly. This benchmark
    // does not include image decoding, URL fetches or browser startup.
    let mut page = LoadedPage::from_html("https://gallery.invalid/", HTML);
    for (index, (name, width, height)) in [
        ("one.png", 256, 192),
        ("two.png", 320, 224),
        ("three.png", 512, 384),
        ("four.png", 384, 256),
    ]
    .into_iter()
    .enumerate()
    {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            let seed = index as u32;
            let alpha = if index == 0 {
                255
            } else {
                [0, 64, 137, 255][((x / 17 + y / 13 + seed) % 4) as usize]
            };
            image::Rgba([(x + seed * 47) as u8, (y * 3) as u8, (x ^ y) as u8, alpha])
        });
        page.images
            .insert(page.resolve(name).unwrap(), Arc::new(image));
    }
    let rule = css::font_faces(&page.stylesheets[0], WIDTH as f32)
        .pop()
        .unwrap();
    page.fonts = Arc::new(
        FontSet::new(vec![FontFaceEntry {
            rule,
            face: FontFace::decode(FONT).unwrap(),
        }])
        .unwrap(),
    );
    page
}

fn main() {
    let page = page();
    let font = page.fonts.entries()[0].face.downgrade();
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build_with_viewport(
        &page,
        &fonts,
        Viewport {
            width: WIDTH as f32,
            height: HEIGHT as f32,
            ..Default::default()
        },
    );
    assert!(!layout.limited);
    let assets: Vec<_> = page.images.values().map(Arc::downgrade).collect();
    let retained_bytes: usize = page.images.values().map(|image| image.as_raw().len()).sum();
    let mut referenced = HashSet::new();
    let mut covered = vec![false; (WIDTH * HEIGHT) as usize];
    let (mut image_commands, mut visible_images, mut image_samples) = (0, 0, 0);
    for command in &layout.commands {
        match command {
            DrawCommand::Image { rect, image } => {
                referenced.insert(Arc::as_ptr(image));
                image_commands += 1;
                let bound = |n: f32, limit: u32| n.ceil().clamp(0., limit as f32) as usize;
                let left = bound(rect.x, WIDTH);
                let right = bound(rect.x + rect.width, WIDTH);
                let top = bound(rect.y, HEIGHT);
                let bottom = bound(rect.y + rect.height, HEIGHT);
                if left < right && top < bottom {
                    visible_images += 1;
                    image_samples += (right - left) * (bottom - top);
                    for y in top..bottom {
                        covered[y * WIDTH as usize + left..y * WIDTH as usize + right].fill(true);
                    }
                }
            }
            DrawCommand::PushState { transform, clip } => {
                assert_eq!(*transform, Affine2D::IDENTITY);
                assert!(clip.is_none());
            }
            _ => {}
        }
    }
    let unique_pixels = covered.iter().filter(|&&on| on).count();
    drop(covered);
    assert_eq!(
        (image_commands, visible_images, referenced.len()),
        (8, 6, 4)
    );
    drop(page);
    assert!(assets.iter().all(|image| image.strong_count() > 0));

    let mut canvas = Canvas::new(WIDTH, HEIGHT, layout.background);
    let initial_cache_bytes = fonts.cache_bytes();
    assert_eq!(initial_cache_bytes, 0);
    let mut paint = || {
        canvas.clear(layout.background);
        let start = Instant::now();
        canvas.document(&layout, &mut fonts, 0., 0.);
        let ns = start.elapsed().as_nanos() as u64;
        black_box(&canvas.pixels);
        assert!(!canvas.limited);
        (ns, fonts.cache_bytes())
    };
    let (cold_ns, cold_cache_bytes) = paint();
    assert!(cold_cache_bytes > 0);
    let mut warm_ns: Vec<_> = (0..WARM_SAMPLES)
        .map(|_| {
            let (ns, bytes) = paint();
            assert_eq!(bytes, cold_cache_bytes);
            ns
        })
        .collect();
    warm_ns.sort_unstable();
    let pixel_hash = canvas.pixels.iter().fold(0xcbf29ce484222325_u64, |h, p| {
        (h ^ u64::from(*p)).wrapping_mul(0x100000001b3)
    });
    let commands = layout.commands.len();
    drop(layout);
    assert!(assets.iter().all(|image| image.strong_count() == 0));
    assert!(font.upgrade().is_none());
    let empty = LoadedPage::from_html("https://gallery.invalid/empty", "");
    let empty = Layout::build(&empty, &fonts, 1.);
    let mut empty_canvas = Canvas::new(1, 1, Color::WHITE);
    for _ in 0..32 {
        empty_canvas.document(&empty, &mut fonts, 0., 0.);
    }
    assert_eq!(fonts.cache_bytes(), 0);

    println!(
        "{}",
        serde_json::json!({
            "viewport": [WIDTH, HEIGHT],
            "draw_commands": commands,
            "image_commands": image_commands,
            "visible_image_commands": visible_images,
            "unique_image_assets": assets.len(),
            "decoded_image_payload_bytes": retained_bytes,
            "image_pixel_samples": image_samples,
            "unique_image_destination_pixels": unique_pixels,
            "repeated_image_destination_samples": image_samples - unique_pixels,
            "cold_paint_ns": cold_ns,
            "warm_paint_p50_ns": warm_ns[WARM_SAMPLES / 2],
            "warm_paint_p95_ns": warm_ns[(WARM_SAMPLES * 95).div_ceil(100) - 1],
            "warm_samples": WARM_SAMPLES,
            "glyph_bitmap_bytes": [initial_cache_bytes, cold_cache_bytes, cold_cache_bytes, fonts.cache_bytes()],
            "released_after_layout_drop": assets.len(),
            "pixel_hash": format!("{pixel_hash:016x}"),
        })
    );
}
