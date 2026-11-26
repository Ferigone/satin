use image::{Rgba, RgbaImage};
use std::sync::OnceLock;

const SITES: [(&str, u32, u32); 4] = [
    ("github.com", 467, 693),
    ("www.youtube.com", 621, 693),
    ("www.figma.com", 773, 693),
    ("www.wikipedia.org", 926, 693),
];

/// Returns a packaged tab icon for an exact, known HTTPS site.
pub fn built_in_site_icon(address: &str) -> Option<&'static RgbaImage> {
    let authority = address
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    let index = SITES.iter().position(|(host, _, _)| *host == authority)?;

    // The atlas is retained for these existing tab icons only. Opening a start
    // page does not decode or copy the old backdrop and its control sprites.
    static ICONS: OnceLock<Option<[RgbaImage; SITES.len()]>> = OnceLock::new();
    let icons = ICONS.get_or_init(|| {
        let source = image::load_from_memory_with_format(
            include_bytes!("../../../../assets/satin-start-reference.png"),
            image::ImageFormat::Png,
        )
        .ok()?
        .into_rgba8();
        (source.dimensions() == (1586, 992))
            .then(|| SITES.map(|(_, x, y)| site_icon(&source, x, y)))
    });
    icons.as_ref()?.get(index)
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
