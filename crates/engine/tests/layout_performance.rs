use rust_browser_engine::{
    Fonts, Layout, LoadedPage,
    css::{self, Style, Stylesheet},
    html,
};
use std::time::Instant;

#[test]
fn shaping_cache_has_bounded_entries_and_payload() {
    let fonts = Fonts::new().unwrap();
    for i in 0..5000 {
        let _ = fonts.shape(&format!("cache-key-{i:05}-word"), 16., false, false, false);
    }
    let before = fonts.shape_cache_stats();
    let _ = fonts.shape("a repeated word", 16., false, false, false);
    let _ = fonts.shape("a repeated word", 16., false, false, false);
    let after = fonts.shape_cache_stats();
    assert!(after.entries <= 4096);
    assert!(after.payload_bytes <= 4 * 1024 * 1024);
    assert!(after.hits > before.hits);
}
#[test]
fn emergency_wrapping_preserves_combining_clusters() {
    let fonts = Fonts::new().unwrap();
    let text = "a\u{301}".repeat(10000);
    let runs = fonts.break_word(&text, 16., false, false, false, 10.);
    let standalone = fonts.shape("a\u{301}", 16., false, false, false);
    assert_eq!(
        runs.iter().map(|r| r.glyphs.len()).sum::<usize>(),
        10000 * standalone.glyphs.len()
    );
    assert!(runs.iter().all(|r| r.advance.is_finite()));
}
fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}
#[test]
#[ignore = "synthetic release benchmark; no browser-wide speed or RAM claim"]
fn synthetic_style_and_wrapping_measurements() {
    let mut sheet = String::new();
    let mut source = String::new();
    for i in 0..4000 {
        sheet.push_str(&format!(
            ".c{i}{{color:#{:06x};padding:2px}}",
            (i * 251) % 0xffffff
        ));
        source.push_str(&format!(
            "<p class='c{i}'>Repeated synthetic paragraph text</p>"
        ));
    }
    let doc = html::parse(&source);
    let ids: Vec<_> = doc.elements("p").collect();
    let start = Instant::now();
    let compiled = Stylesheet::new(css::parse(&sheet, 800.), 800.);
    let compile_ms = start.elapsed().as_secs_f64() * 1000.;
    let candidates: usize = ids
        .iter()
        .map(|&id| compiled.candidate_rule_count(&doc, id))
        .sum();
    assert_eq!(candidates, 4000);
    let mut style_samples = vec![];
    let parent = Style::default();
    for _ in 0..7 {
        let start = Instant::now();
        for &id in &ids {
            std::hint::black_box(compiled.compute(&doc, id, &parent, 16.));
        }
        style_samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    let fonts = Fonts::new().unwrap();
    let page = LoadedPage::from_html(
        "https://example.test/",
        &format!("<!doctype html><style>body{{margin:0}}{sheet}</style>{source}"),
    );
    let _ = Layout::build(&page, &fonts, 800.);
    let mut layout_samples = vec![];
    for _ in 0..7 {
        let start = Instant::now();
        std::hint::black_box(Layout::build(&page, &fonts, 800.));
        layout_samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    println!(
        "4000 class rules / 4000 paragraphs: parse+compile={compile_ms:.3} ms; indexed style median(7)={:.3} ms; warm layout median(7)={:.3} ms; candidate visits={candidates} versus 16000000 exhaustive rule visits",
        median(style_samples),
        median(layout_samples)
    );
    for length in [20000, 200000] {
        let text = "w".repeat(length);
        let mut samples = vec![];
        let mut lines = 0;
        for _ in 0..7 {
            let start = Instant::now();
            let runs = fonts.break_word(&text, 16., false, false, false, 300.);
            lines = runs.len();
            std::hint::black_box(runs);
            samples.push(start.elapsed().as_secs_f64() * 1000.);
        }
        println!(
            "Emergency wrap: {length} ASCII chars, {lines} runs, median(7)={:.3} ms",
            median(samples)
        );
    }
    let stats = fonts.shape_cache_stats();
    println!(
        "Shaping cache: entries={}, accounted payload={} bytes, hits={}, misses={}; metadata/allocator overhead and process RAM not measured",
        stats.entries, stats.payload_bytes, stats.hits, stats.misses
    );
}
