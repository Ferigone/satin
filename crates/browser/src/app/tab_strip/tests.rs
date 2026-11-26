use super::*;

fn reference(width: f32, count: usize) -> TabStrip {
    TabStrip::new(width, 3. * caption_button_width(width), count)
}

#[test]
fn regular_capsules_share_width_and_have_no_trailing_tab_gap() {
    for (count, plus_x) in [(1, 327.), (2, 546.), (3, 765.)] {
        let strip = reference(1586., count);
        assert_eq!(strip.list, rect(107., 6., plus_x - 111., 31.));
        for (index, tab) in strip.visible_tabs().enumerate() {
            assert_eq!(tab.index, index);
            assert_eq!(tab.face, rect(107. + index as f32 * 219., 6., 216., 31.));
            assert_eq!(tab.face, tab.visible_face);
        }
        let plus = strip.new_tab.unwrap();
        assert_eq!(plus.bounds, rect(plus_x, 6., 31., 31.));
        assert_eq!(plus.glyph, rect(plus_x + 7., 13., 17., 17.));
        assert_eq!(
            strip.tab_list.unwrap().bounds,
            rect(plus_x + 35., 6., 31., 31.)
        );
    }
}

#[test]
fn viewport_width_drives_the_width_clamp_and_narrow_padding() {
    for (width, left, face, caption) in [
        (639.5, 48., 106., 35.),
        (660., 48., 106., 35.),
        (660.25, 107., 106., 42.),
        (800., 107., 120., 42.),
        (1440., 107., 216., 42.),
        (1920., 107., 216., 42.),
    ] {
        let strip = reference(width, 2);
        assert_eq!(caption_button_width(width), caption);
        assert_eq!(strip.tab(0).unwrap().face, rect(left, 6., face, 31.));
        assert_eq!(
            strip.tab(1).unwrap().face,
            rect(left + face + 3., 6., face, 31.)
        );
        assert_eq!(strip.new_tab.unwrap().bounds.x, left + 2. * face + 7.);
    }
}

#[test]
fn close_hit_and_face_have_the_same_row_geometry_as_their_paint() {
    let strip = reference(640., 2);
    let tab = strip.tab(0).unwrap();
    let close = tab.close.unwrap();
    assert_eq!(tab.face, rect(48., 6., 106., 31.));
    assert_eq!(tab.favicon, rect(60., 12., 19., 19.));
    assert_eq!(tab.title, rect(87., 6., 30., 31.));
    assert_eq!(close.hit, rect(125., 10., 21., 23.));
    assert_eq!(close.glyph, rect(130.5, 16.5, 10., 10.));
    assert_eq!(close.hover_circle, rect(127.5, 13.5, 16., 16.));

    assert!(!tab.visible_face.contains(49., 5.99));
    assert!(!tab.visible_face.contains(49., 37.));
    assert!(tab.visible_face.contains(125., 9.99)); // selects above the X
    assert!(!close.hit.contains(125., 9.99));
    assert!(close.hit.contains(125., 10.));
    assert!(!close.hit.contains(146., 20.));
    for x in [154., 155., 156.] {
        assert!(!tab.visible_face.contains(x, 21.));
        assert!(!strip.tab(1).unwrap().visible_face.contains(x, 21.));
    }
}

#[test]
fn overflowing_tabs_scroll_under_a_fixed_clip_never_into_the_controls() {
    for (width, list, plus) in [
        (640., rect(48., 6., 407., 31.), 459.),
        (1586., rect(107., 6., 1269., 31.), 1380.),
    ] {
        let strip = reference(width, 24);
        assert_eq!(strip.list, list);
        assert!(strip.max_scroll() > 0.);
        assert_eq!(strip.new_tab.as_ref().unwrap().bounds.x, plus);
        assert!(strip.tab(23).is_none());

        let strip = strip.reveal(23);
        assert_eq!(strip.scroll(), strip.max_scroll());
        assert_eq!(strip.new_tab.as_ref().unwrap().bounds.x, plus);
        assert!(strip.tab(0).is_none());
        let last = strip.tab(23).unwrap();
        assert_eq!(last.face, last.visible_face);
        assert_eq!(right(last.face), right(strip.list));
        for tab in strip.visible_tabs() {
            assert!(tab.visible_face.x >= list.x && right(tab.visible_face) <= right(list));
            if let Some(close) = tab.close {
                assert!(close.hit.x >= list.x && right(close.hit) <= right(list));
            }
        }
    }
}

#[test]
fn a_close_sliver_without_its_glyph_is_selection_only() {
    let strip = reference(640., 24);
    let last = strip.tab(3).unwrap();
    assert_eq!(last.visible_face, rect(375., 6., 80., 31.));
    assert!(!last.visible_face.contains(455., 20.));
    assert!(last.close.is_none()); // 3 px of the nominal X button at the right edge

    let strip = strip.scroll_to(80.);
    let first = strip.tab(0).unwrap();
    assert_eq!(first.face, rect(-32., 6., 106., 31.));
    assert_eq!(first.visible_face, rect(48., 6., 26., 31.));
    assert_eq!(first.close.unwrap().hit, rect(48., 10., 18., 23.));
    assert!(strip.scroll_to(86.).tab(0).unwrap().close.is_none());
}

#[test]
fn restore_offsets_clamp_and_active_reveal_only_moves_as_far_as_needed() {
    let strip = reference(640., 24).scroll_to(15.).reveal(1);
    assert_eq!(strip.scroll(), 15.);
    let strip = strip.reveal(0);
    assert_eq!(strip.scroll(), 0.);
    let strip = strip.reveal(3);
    assert_eq!(strip.scroll(), 26.);
    assert_eq!(strip.reveal(3).scroll(), 26.);

    // Closing tabs or resizing to a window with more available room.
    assert_eq!(reference(640., 2).scroll_to(26.).scroll(), 0.);
    assert_eq!(reference(1586., 4).scroll_to(26.).scroll(), 0.);
    assert_eq!(reference(640., 24).scroll_to(-8.).scroll(), 0.);
    let strip = reference(640., 24).scroll_to(f32::INFINITY);
    assert_eq!(strip.scroll(), strip.max_scroll());
}

#[test]
fn reserve_wide_caption_controls_and_fail_safely_below_the_window_minimum() {
    let strip = TabStrip::new(900., 200., 24);
    assert_eq!(strip.list.width, 509.);
    assert_eq!(right(strip.tab_list.unwrap().bounds), 686.);

    let strip = reference(190., 3);
    assert_eq!(strip.visible_tabs().count(), 0);
    assert!(strip.new_tab.is_none() && strip.tab_list.is_none());
    let strip = reference(200., 3);
    assert_eq!(strip.new_tab.unwrap().bounds, rect(52., 6., 31., 31.));
    assert!(strip.tab_list.is_none());

    let strip = reference(640., 0).reveal(0);
    assert_eq!(strip.visible_tabs().count(), 0);
    assert_eq!(strip.new_tab.unwrap().bounds.x, 52.);
}

#[test]
fn revealing_a_tab_wider_than_the_clip_keeps_the_nearest_edge() {
    let strip = reference(260., 3);
    assert_eq!(strip.list.width, 27.);
    assert_eq!(strip.scroll_to(10.).reveal(0).scroll(), 10.);
    assert_eq!(reference(260., 3).scroll_to(110.).reveal(0).scroll(), 79.);
    assert_eq!(reference(260., 3).reveal(2).scroll(), 218.);
}
