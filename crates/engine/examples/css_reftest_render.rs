//! Pixel fingerprint worker for tools/css-wpt.py. Inputs are local, static HTML pages.
use ring::digest::{Context, SHA256};
use rust_browser_engine::{Fonts, Layout, LoadedPage, layout::Viewport, paint::Canvas};
use std::{
    env, fs,
    io::{self, BufRead},
    path::Path,
};

fn render(root: &Path, path: &str, fonts: &mut Fonts) -> Result<serde_json::Value, String> {
    let source = fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
    let page = LoadedPage::from_html(&format!("https://web-platform.test/{path}"), &source);
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
    let mut fonts = Fonts::new()?;
    for line in io::stdin().lock().lines() {
        let path = line?;
        let result = render(Path::new(&root), &path, &mut fonts)
            .unwrap_or_else(|error| serde_json::json!({"error":error}));
        println!("{result}");
    }
    Ok(())
}
