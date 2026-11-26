//! Pixel fingerprint worker for tools/css-wpt.py. Inputs are local, static HTML pages.
use ring::digest::{Context, SHA256};
use rust_browser_engine::{Fonts, Layout, LoadedPage, layout::Viewport, paint::Canvas};
use rust_browser_engine::{
    css,
    fonts::{FontFace, FontFaceEntry, FontSet},
};
use std::{
    env, fs,
    io::{self, BufRead},
    path::Path,
    sync::Arc,
};

fn render(
    root: &Path,
    path: &str,
    fonts: &mut Fonts,
    ahem: Option<&Arc<FontSet>>,
) -> Result<serde_json::Value, String> {
    let source = fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
    let mut page = LoadedPage::from_html(&format!("https://web-platform.test/{path}"), &source);
    if let Some(font_set) = ahem
        && page.document.html_elements("link").any(|id| {
            page.document.attr(id, "href") == Some("/fonts/ahem.css")
                && page
                    .document
                    .attr(id, "rel")
                    .is_some_and(|rel| rel.eq_ignore_ascii_case("stylesheet"))
        })
    {
        page.fonts = Arc::clone(font_set);
    }
    if page.document.truncated {
        return Err("document parse limit".into());
    }
    let viewport = Viewport {
        width: 800.,
        height: 600.,
        ..Viewport::default()
    };
    let layout = Layout::build_with_viewport(&page, fonts, viewport);
    let mut canvas = Canvas::new(800, 600, layout.background);
    canvas.document(&layout, fonts, 0., 0.);
    if layout.limited || canvas.limited {
        return Err("engine work limit".into());
    }
    let mut hash = Context::new(&SHA256);
    let mut row = [0u8; 800 * 4];
    for pixels in canvas.pixels.chunks_exact(800) {
        for (out, pixel) in row.chunks_exact_mut(4).zip(pixels) {
            out.copy_from_slice(&pixel.to_le_bytes());
        }
        hash.update(&row);
    }
    let hash: String = hash
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(serde_json::json!({
        "sha256": hash,
        "uniform": canvas.pixels.iter().all(|p| *p == canvas.pixels[0])
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::args_os().nth(1).ok_or("missing WPT checkout")?;
    let root = Path::new(&root);
    let ahem = if env::args_os().nth(2).as_deref() == Some(std::ffi::OsStr::new("--ahem")) {
        let sheet = fs::read_to_string(root.join("fonts/ahem.css"))?;
        let [rule]: [_; 1] = css::font_faces(&sheet, 800.)
            .try_into()
            .map_err(|_| "expected one Ahem font rule")?;
        let face = FontFace::decode(&fs::read(root.join("fonts/Ahem.ttf"))?)?;
        Some(Arc::new(FontSet::new(vec![FontFaceEntry { rule, face }])?))
    } else {
        None
    };
    let mut fonts = Fonts::new()?;
    for line in io::stdin().lock().lines() {
        let path = line?;
        let result = render(root, &path, &mut fonts, ahem.as_ref())
            .unwrap_or_else(|error| serde_json::json!({"error":error}));
        println!("{result}");
    }
    Ok(())
}
