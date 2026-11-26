use super::*;

fn raster(glyph: Glyph, origin: Point, size: f32, width: u32, height: u32) -> Vec<u8> {
    let mut mask = vec![0; (width * height) as usize];
    glyph.rasterize_sized(origin.0, origin.1, size, width, height, |x, y, coverage| {
        assert!(x < width && y < height);
        let slot = &mut mask[(y * width + x) as usize];
        assert_eq!(*slot, 0, "pixel blended twice for {glyph:?}");
        assert!(coverage > 0);
        *slot = coverage;
    });
    mask
}

#[test]
fn all_reference_glyphs_rasterize_distinct_masks_at_chrome_sizes() {
    assert_eq!(paths::ALL.len(), 50); // 49 catalog glyphs + the Satin mark
    for size in [11., 13., 16., 17., 34.] {
        let mut masks = std::collections::HashSet::new();
        for &glyph in paths::ALL {
            let mask = raster(glyph, Point(1.375, 2.625), size, 38, 39);
            assert!(mask.iter().any(|&a| a != 0), "blank {glyph:?} at {size}");
            assert!(masks.insert(mask), "duplicate {glyph:?} at {size}");
        }
    }
}

#[test]
fn legacy_navigation_aliases_and_history_arrow_symmetry() {
    assert_eq!(Glyph::Back, Glyph::ArrowLeft);
    assert_eq!(Glyph::Forward, Glyph::ArrowRight);
    assert_eq!(Glyph::Reload, Glyph::RotateCw);
    assert_eq!(Glyph::Minimize, Glyph::Minus);
    assert_eq!(Glyph::Restore, Glyph::Copy);
    for size in [13, 17, 20, 25, 30, 40] {
        let back = raster(Glyph::Back, Point(0., 0.), size as f32, size, size);
        let forward = raster(Glyph::Forward, Point(0., 0.), size as f32, size, size);
        for (a, b) in back
            .chunks_exact(size as usize)
            .zip(forward.chunks_exact(size as usize))
        {
            assert!(a.iter().eq(b.iter().rev()), "at {size} device pixels");
        }
    }

    let mut legacy = vec![0; 30 * 30];
    Glyph::Back.rasterize(0., 0., 1.5, 30, 30, |x, y, a| {
        legacy[(y * 30 + x) as usize] = a;
    });
    assert_eq!(legacy, raster(Glyph::Back, Point(0., 0.), 30., 30, 30));
}

#[test]
fn fractional_dpi_and_negative_origins_clip_without_moving_art() {
    let size = 17. * 1.25;
    let full = raster(Glyph::Globe, Point(7.375, 6.625), size, 32, 32);
    let clipped = raster(Glyph::Globe, Point(-2.625, -1.375), size, 9, 11);
    assert!(clipped.iter().any(|&a| a != 0));
    for y in 0..11 {
        for x in 0..9 {
            assert_eq!(clipped[y * 9 + x], full[(y + 8) * 32 + x + 10]);
        }
    }
}

#[test]
fn curves_and_open_spaces_follow_the_svg() {
    let ink = |glyph: Glyph, x, y| glyph.shape().sample(Point(x, y), false).0;
    assert_eq!(ink(Glyph::Reload, 4.1, 11.4), 100); // large reverse sweep
    assert_eq!(ink(Glyph::Reload, 20., 13.), 0); // gap below arrowhead
    assert_eq!(ink(Glyph::Search, 5., 10.5), 100);
    assert_eq!(ink(Glyph::Search, 10.5, 10.5), 0);
    assert_eq!(ink(Glyph::Puzzle, 12.5, 2.5), 100); // upper round tab
    assert_eq!(ink(Glyph::Puzzle, 12.5, 20.), 0); // gap under lower tab
    assert_eq!(ink(Glyph::Shield, 17.8, 16.9375), 100); // cubic at t=.5
    assert_eq!(ink(Glyph::Globe, 8., 12.9), 100); // ellipse, not equator
    assert_eq!(ink(Glyph::Ellipsis, 5., 12.), 0); // stroked circles, not dots
    assert_eq!(ink(Glyph::Ellipsis, 6., 12.), 100);
}

#[test]
fn satin_mark_preserves_path_opacity_and_star_keeps_a_separate_fill() {
    let brand = raster(Glyph::Brand, Point(0., 0.), 24., 24, 24);
    assert_eq!(brand[8 * 24 + 12], 255);
    assert_eq!(brand[11 * 24 + 17], 125); // SVG opacity .49
    assert_eq!(brand[4 * 24 + 20], 0);

    let outline = raster(Glyph::Star, Point(0., 0.), 24., 24, 24);
    assert_eq!(outline[12 * 24 + 12], 0);
    let mut center = None;
    Glyph::rasterize_bookmarked_star(0., 0., 24., 24, 24, |x, y, stroke, fill| {
        assert_eq!(stroke, outline[(y * 24 + x) as usize]);
        assert!(u16::from(stroke) + u16::from(fill) <= 255);
        if (x, y) == (12, 12) {
            center = Some((stroke, fill));
        }
    });
    assert_eq!(center, Some((0, 255)));
}
