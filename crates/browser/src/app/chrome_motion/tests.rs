use super::*;

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn near(value: f32, expected: f32) {
    assert!((value - expected).abs() < 0.000_01, "{value} != {expected}");
}

#[test]
fn toolbar_and_caption_use_default_css_ease_and_distinct_durations() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    assert_eq!(motion.button(Button::Back, false), 0.);
    assert_eq!(motion.caption(2, false, false), (0., 0.));
    motion.begin_frame(t);
    assert_eq!(motion.button(Button::Back, true), 0.);
    assert_eq!(motion.caption(2, true, true), (0., 0.));

    motion.begin_frame(t + ms(65));
    near(motion.button(Button::Back, true), 0.802_403_4);
    motion.begin_frame(t + ms(80));
    let (hot, press) = motion.caption(2, true, true);
    near(hot, 0.802_403_4);
    near(press, hot);

    motion.begin_frame(t + ms(130));
    assert_eq!(motion.button(Button::Back, true), 1.);
    assert!(motion.caption(2, true, true).0 < 1.);
    motion.begin_frame(t + ms(160));
    assert_eq!(motion.caption(2, true, true), (1., 1.));
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), None);
}

#[test]
fn tab_hover_color_border_and_close_have_reference_curves() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    motion.tab(10, false, false, Some(false));
    motion.tab(20, false, false, Some(false));
    motion.begin_frame(t);
    motion.tab(10, false, true, Some(true));
    motion.tab(20, true, false, Some(false));

    motion.begin_frame(t + ms(60));
    let tab = motion.tab(10, false, true, Some(true));
    near(tab.close, 0.802_403_4);
    near(tab.close_hover, tab.close);
    assert_eq!(tab.border, 0.);
    motion.begin_frame(t + ms(85));
    let tab = motion.tab(10, false, true, Some(true));
    near(tab.hover, 0.946_427_5);
    near(tab.text, 0.802_403_4);
    let selected = motion.tab(20, true, false, Some(false));
    near(selected.border, 0.802_403_4);
    assert_eq!(selected.hover, 0.);

    motion.begin_frame(t + ms(120));
    let tab = motion.tab(10, false, true, Some(true));
    assert_eq!(tab.close, 1.);
    assert!(tab.text < 1.);
    motion.begin_frame(t + ms(170));
    let tab = motion.tab(10, false, true, Some(true));
    assert_eq!((tab.hover, tab.text), (1., 1.));
    assert_eq!(motion.tab(20, true, false, None).border, 1.);
}

#[test]
fn reversal_starts_at_the_rendered_value_and_shortens_like_css() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    motion.button(Button::Menu, false);
    motion.begin_frame(t);
    motion.button(Button::Menu, true);
    let reverse = t + ms(65);
    motion.begin_frame(reverse);
    let prior = motion.button(Button::Menu, true);
    near(prior, 0.802_403_4);
    assert_eq!(motion.button(Button::Menu, false), prior);
    for millis in [16, 32, 48, 64, 80, 100] {
        motion.begin_frame(reverse + ms(millis));
        let value = motion.button(Button::Menu, false);
        assert!(value > 0. && value < prior);
    }
    motion.begin_frame(reverse + ms(105));
    assert_eq!(motion.button(Button::Menu, false), 0.);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), None);
}

#[test]
fn deadline_is_one_shot_and_ends_with_exact_endpoint_frame() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    motion.button(Button::Downloads, false);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), None);
    motion.begin_frame(t);
    motion.button(Button::Downloads, true);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), Some(t + ms(16)));
    assert!(!motion.take_due(t + ms(15)));
    assert!(motion.take_due(t + ms(16)));
    assert!(!motion.take_due(t + ms(120)));
    assert_eq!(motion.deadline(), None);

    // A delayed redraw advances by elapsed time, not by simulated ticks.
    motion.begin_frame(t + ms(127));
    assert!(motion.button(Button::Downloads, true) < 1.);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), Some(t + ms(130)));
    assert!(motion.take_due(t + ms(130)));
    motion.begin_frame(t + ms(130));
    assert_eq!(motion.button(Button::Downloads, true), 1.);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), None);
}

#[test]
fn reduced_policy_snaps_and_failed_present_does_not_arm_a_wake() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    motion.button(Button::NewTab, false);
    motion.begin_frame(t);
    motion.button(Button::NewTab, true);
    motion.finish_frame(false);
    assert_eq!(motion.deadline(), None);
    motion.begin_frame(t + ms(16));
    motion.button(Button::NewTab, true);
    motion.finish_frame(true);
    assert!(motion.deadline().is_some());

    assert!(motion.set_enabled(false));
    assert_eq!(motion.deadline(), None);
    motion.begin_frame(t + ms(32));
    assert_eq!(motion.button(Button::NewTab, true), 1.);
    assert_eq!(motion.button(Button::NewTab, false), 0.);
    assert_eq!(motion.caption(2, true, true), (1., 1.));
    let ink = motion.tab(10, false, true, Some(true));
    assert_eq!((ink.hover, ink.close), (1., 1.));
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), None);
    assert!(!motion.set_enabled(false));
    assert!(motion.set_enabled(true));
    motion.begin_frame(t + ms(48));
    assert_eq!(motion.button(Button::NewTab, true), 0.);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), Some(t + ms(64)));
}

#[test]
fn tab_tracks_follow_identity_and_hidden_tracks_stop_scheduling() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    motion.tab(20, false, false, Some(false));
    motion.tab(10, false, false, Some(false));
    motion.finish_frame(true);
    motion.begin_frame(t);
    motion.tab(20, false, true, Some(true));
    motion.tab(10, false, false, Some(false));
    motion.finish_frame(true);

    motion.begin_frame(t + ms(60));
    let replacement = motion.tab(30, true, false, Some(false));
    let relocated = motion.tab(20, false, true, Some(true));
    assert_eq!(
        (replacement.border, replacement.close, replacement.hover),
        (1., 1., 0.)
    );
    near(relocated.close, 0.802_403_4);
    motion.finish_frame(true);
    assert_eq!(motion.tabs.len(), 2);
    assert!(motion.deadline().is_some());

    motion.begin_frame(t + ms(61));
    motion.tab(30, true, false, None);
    motion.finish_frame(true);
    assert_eq!(motion.tabs.len(), 1);
    assert_eq!(motion.deadline(), None);
}

#[test]
fn capped_tab_storage_and_non_close_caption_do_not_add_invisible_wakes() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<1>::new(t, true);
    motion.caption(0, true, false);
    motion.begin_frame(t);
    assert_eq!(motion.caption(0, true, true), (1., 0.));
    for id in 0..50 {
        assert_eq!(motion.tab(id, true, false, None).border, 1.);
    }
    assert_eq!(motion.tabs.len(), 2);
    motion.finish_frame(true);
    assert_eq!(motion.deadline(), None);
}

#[test]
fn every_toolbar_and_strip_button_has_an_independent_track() {
    let t = Instant::now();
    let mut motion = ChromeMotion::<2>::new(t, true);
    let others = [
        Button::Forward,
        Button::Reload,
        Button::Home,
        Button::Bookmark,
        Button::TabList,
    ];
    for button in others {
        assert_eq!(motion.button(button, false), 0.);
    }
    motion.button(Button::Back, false);
    motion.begin_frame(t);
    motion.button(Button::Back, true);
    motion.begin_frame(t + ms(65));
    for button in others {
        assert_eq!(motion.button(button, false), 0.);
    }
    near(motion.button(Button::Back, true), 0.802_403_4);
}
