use super::*;

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn pixel(canvas: &Canvas, x: usize, y: usize) -> u32 {
    canvas.pixels[y * canvas.width as usize + x]
}

#[test]
fn css_angle_direction_and_wrapping() {
    let box_ = rect(0., 0., 5., 5.);
    let stops = [
        (0., Color(255, 0, 0, 255)),
        (0.5, Color(0, 255, 0, 255)),
        (1., Color(0, 0, 255, 255)),
    ];
    let mut c = Canvas::new(5, 5, Color::WHITE);
    for (angle, first, last) in [
        (0., (2, 4), (2, 0)),
        (90., (0, 2), (4, 2)),
        (180., (2, 0), (2, 4)),
        (-90., (4, 2), (0, 2)),
        (450., (0, 2), (4, 2)),
    ] {
        c.fill_linear_gradient(box_, angle, stops);
        assert_eq!(pixel(&c, first.0, first.1), 0xcc3300);
        assert_eq!(pixel(&c, 2, 2), 0x00ff00);
        assert_eq!(pixel(&c, last.0, last.1), 0x0033cc);
    }
}

#[test]
fn reference_110_degree_gradient_uses_full_box_projection_even_when_clipped() {
    let mut c = Canvas::new(100, 40, Color::BLACK);
    let box_ = rect(0., 0., 100., 40.);
    let stops = [
        (0., Color(38, 35, 40, 255)),
        (0.69, Color(34, 33, 36, 255)),
        (1., Color(54, 49, 62, 255)),
    ];
    c.fill_linear_gradient(box_, 110., stops);
    // Same x, different y: the samples are on either side of the 69% stop.
    assert_eq!(pixel(&c, 0, 0), 0x262328);
    assert_eq!(pixel(&c, 67, 0), 0x232125);
    assert_eq!(pixel(&c, 67, 39), 0x242226);
    assert_eq!(pixel(&c, 99, 39), 0x36313e);
    c.clear(Color::BLACK);
    c.clip(rect(66.6, 0., 2., 40.));
    c.fill_linear_gradient(box_, 110., stops);
    assert_eq!(pixel(&c, 66, 0), 0);
    assert_eq!(pixel(&c, 67, 0), 0x232125);
    assert_eq!(pixel(&c, 68, 39), 0x242327);
    assert_eq!(pixel(&c, 69, 39), 0);
}

#[test]
fn fixed_up_stops_can_jump_or_extend_outside_the_box() {
    let mut c = Canvas::new(49, 1, Color::BLACK);
    let box_ = rect(0., 0., 49., 1.);
    c.fill_linear_gradient(
        box_,
        90.,
        [
            (0., Color::BLACK),
            (0.5, Color(255, 0, 0, 255)),
            (0.25, Color::WHITE),
        ],
    );
    assert_eq!(pixel(&c, 23, 0), 0xf50000);
    assert_eq!(pixel(&c, 24, 0), 0xffffff);
    c.fill_linear_gradient(
        box_,
        90.,
        [(-1., Color::BLACK), (1., Color::WHITE), (2., Color::BLACK)],
    );
    assert_eq!(pixel(&c, 24, 0), 0xbfbfbf);
    c.fill_linear_gradient(
        box_,
        90.,
        [
            (0.25, Color::WHITE),
            (0.5, Color::BLACK),
            (0.75, Color(42, 1, 99, 255)),
        ],
    );
    assert_eq!(pixel(&c, 0, 0), 0xffffff);
    assert_eq!(pixel(&c, 48, 0), 0x2a0163);
}

#[test]
fn transparent_gradient_stops_blend_premultiplied_against_each_destination() {
    let mut c = Canvas::new(2, 1, Color(20, 100, 180, 255));
    c.pixels[1] = 0xdcb414;
    c.fill_linear_gradient(
        rect(0., 0., 2., 1.),
        90.,
        [
            (0., Color(255, 0, 0, 0)),
            (0.5, Color(0, 0, 255, 128)),
            (1., Color(0, 255, 0, 0)),
        ],
    );
    assert_eq!(c.pixels, [0x0f4bc7, 0xa5874f]);
}

#[test]
fn rounded_gradient_uses_individual_corners_and_subpixel_coverage() {
    let mut c = Canvas::new(18, 16, Color::BLACK);
    let box_ = rect(3., 3., 12., 10.);
    let white = [(0., Color::WHITE), (0.69, Color::WHITE), (1., Color::WHITE)];
    c.fill_rounded_linear_gradient(box_, [4., 0., 0., 0.], 110., white);
    assert_eq!(pixel(&c, 3, 3), 0);
    assert_eq!(pixel(&c, 4, 3), 0x333333);
    assert_eq!(pixel(&c, 14, 3), 0xffffff);
    assert_eq!(pixel(&c, 3, 12), 0xffffff);

    c.clear(Color::BLACK);
    c.fill_rounded_rect(rect(3.75, 3., 0.5, 10.), [0.; 4], Color::WHITE);
    assert_eq!(pixel(&c, 3, 5), 0x404040);
    assert_eq!(pixel(&c, 4, 5), 0x404040);
}

#[test]
fn outline_is_one_translucent_ring_with_fractional_edges() {
    let mut c = Canvas::new(18, 16, Color::BLACK);
    let box_ = rect(3., 3., 12., 10.);
    let tint = Color(255, 0, 0, 128);
    c.outline_rounded_rect(box_, [4.; 4], 0.25, 1.25, tint);
    assert_eq!(pixel(&c, 9, 0), 0);
    assert_eq!(pixel(&c, 9, 1), 0x400000);
    assert_eq!(pixel(&c, 9, 2), 0x600000);
    assert_eq!(pixel(&c, 9, 3), 0);
    assert_eq!(pixel(&c, 9, 9), 0);
    assert_eq!(pixel(&c, 3, 2), pixel(&c, 14, 2));
    assert!((0x010000..0x600000).contains(&pixel(&c, 3, 2)));

    c.clear(Color::BLACK);
    c.outline_rounded_rect(box_, [4.; 4], -1., 1., tint);
    assert_eq!(pixel(&c, 9, 2), 0);
    assert_eq!(pixel(&c, 9, 3), 0x800000);
    assert_eq!(pixel(&c, 9, 4), 0);
}

#[test]
fn shadow_obeys_blur_offset_spread_and_mask() {
    let mut c = Canvas::new(50, 42, Color::WHITE);
    let box_ = rect(15., 10., 20., 12.);
    let radii = [4.; 4];
    let mut s = RoundedShadow {
        offset_x: 0.,
        offset_y: 0.,
        blur_radius: 0.,
        spread: 0.,
        color: Color(255, 0, 0, 128),
    };
    c.shadow_rounded_rect(box_, radii, s);
    assert!(c.pixels.iter().all(|&p| p == 0xffffff));
    s.offset_y = 4.;
    s.spread = 1.;
    c.shadow_rounded_rect(box_, radii, s);
    assert_eq!(pixel(&c, 25, 21), 0xffffff);
    assert_eq!(pixel(&c, 25, 22), 0xff7f7f);
    assert_eq!(pixel(&c, 25, 26), 0xff7f7f);
    assert_eq!(pixel(&c, 25, 27), 0xffffff);

    c.clear(Color::WHITE);
    s.spread = 0.;
    s.blur_radius = 6.;
    c.shadow_rounded_rect(box_, radii, s);
    assert_eq!(pixel(&c, 25, 21), 0xffffff);
    assert!(pixel(&c, 15, 10) < 0xffffff); // Outside the rounded corner, inside its box.
    let edge = pixel(&c, 25, 22);
    let tail = pixel(&c, 25, 29);
    assert!((0xff7f7f..tail).contains(&edge));
    assert!(tail < 0xffffff);
    assert!(edge < pixel(&c, 25, 8));
    assert_eq!(pixel(&c, 25, 35), 0xffffff);
}

#[test]
fn bounds_charge_only_clipped_bands_and_fail_before_drawing() {
    let mut c = Canvas::new(220, 170, Color::BLACK);
    let box_ = rect(10., 10., 200., 150.);
    c.work_left = 1600;
    c.outline_rounded_rect(box_, [0.; 4], 0., 1., Color::WHITE);
    assert!(!c.limited);
    assert!(c.work_left < 1600);
    assert_eq!(pixel(&c, 110, 9), 0xffffff);
    assert_eq!(pixel(&c, 110, 10), 0);
    assert_eq!(pixel(&c, 110, 100), 0);

    c.clear(Color::BLACK);
    c.clip(rect(90., 90., 40., 40.));
    c.work_left = 1;
    c.shadow_rounded_rect(
        box_,
        [12.; 4],
        RoundedShadow {
            offset_x: 0.,
            offset_y: 16.,
            blur_radius: 48.,
            spread: 0.,
            color: Color::WHITE,
        },
    );
    assert!(!c.limited);
    assert_eq!(c.work_left, 1);

    c.clip(rect(9., 9., 2., 2.));
    c.work_left = 3;
    let stops = [(0., Color::WHITE), (0.69, Color::WHITE), (1., Color::WHITE)];
    c.fill_linear_gradient(rect(-1.0e10, -1.0e10, 2.0e10, 2.0e10), 110., stops);
    assert!(c.limited);
    assert!(c.pixels.iter().all(|&p| p == 0));
    c.clear(Color::BLACK);
    c.work_left = 4;
    c.fill_linear_gradient(rect(-1.0e10, -1.0e10, 2.0e10, 2.0e10), 110., stops);
    assert!(!c.limited);
    assert_eq!(c.work_left, 0);
    assert_eq!(c.pixels.iter().filter(|&&p| p == 0xffffff).count(), 4);
}

#[test]
fn invalid_inputs_are_noops_and_shadow_blur_is_capped() {
    let mut c = Canvas::new(24, 24, Color::WHITE);
    let box_ = rect(8., 8., 8., 8.);
    let work = c.work_left;
    c.fill_linear_gradient(box_, f32::NAN, [(0., Color::BLACK); 3]);
    c.fill_linear_gradient(box_, 110., [(f32::INFINITY, Color::BLACK); 3]);
    c.fill_rounded_rect(rect(8., 8., 0., 8.), [4.; 4], Color::BLACK);
    c.outline_rounded_rect(box_, [4.; 4], 0., -1., Color::BLACK);
    let mut s = RoundedShadow {
        offset_x: 0.,
        offset_y: 0.,
        blur_radius: -1.,
        spread: 0.,
        color: Color::BLACK,
    };
    c.shadow_rounded_rect(box_, [4.; 4], s);
    assert_eq!(c.work_left, work);
    assert!(c.pixels.iter().all(|&p| p == 0xffffff));

    s.blur_radius = 256.;
    s.spread = 10.;
    c.shadow_rounded_rect(box_, [4.; 4], s);
    let capped = c.pixels.clone();
    assert_ne!(pixel(&c, 2, 2), 0xffffff);
    c.clear(Color::WHITE);
    s.blur_radius = 100_000.;
    c.shadow_rounded_rect(box_, [4.; 4], s);
    assert_eq!(c.pixels, capped);
}
