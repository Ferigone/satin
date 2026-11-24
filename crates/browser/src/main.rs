#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod file_picker;
mod headless;
mod jobs;
mod media;
mod profile;
mod shortcuts;
mod testing;
#[cfg(all(windows, target_arch = "x86_64"))]
mod updater;

use rust_browser_engine::{Fonts, Layout, paint::Canvas, script::ScriptRuntime};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("Satin: {error}");
        #[cfg(windows)]
        if !std::env::args().any(|a| a == "--headless" || a == "--smoke-window") {
            let title = "Satin\0".encode_utf16().collect::<Vec<_>>();
            let message = format!("{error}\0").encode_utf16().collect::<Vec<_>>();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                        | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION,
                );
            }
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|a| a == "--version") {
        println!("Satin {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help") {
        println!(
            "rust-browser [--url URL] [--profile DIRECTORY] [--headless --screenshot FILE --dump-dom FILE --click-id ID --click-count 1 --run-for-ms 500] [--smoke-window FILE --smoke-click-id ID --smoke-actions JSON_FILE] [--width PX --height PX] [--file-picker-responses JSON_FILE]"
        );
        return Ok(());
    }
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let url = value("--url").or_else(|| args.first().filter(|a| !a.starts_with('-')).cloned());
    let headless = args.iter().any(|a| a == "--headless");
    let mut file_picker_responses = if args.iter().any(|a| a == "--file-picker-responses") {
        if !headless && value("--smoke-window").is_none() {
            return Err("--file-picker-responses requires --headless or --smoke-window".into());
        }
        let path = value("--file-picker-responses").ok_or("Missing file picker responses path")?;
        Some(file_picker::Responses::read(std::path::Path::new(&path))?)
    } else {
        None
    };
    if headless {
        let url = rust_browser_engine::normalize_url(url.as_deref().unwrap_or("about:demo"))?;
        let width = value("--width")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(1280)
            .clamp(160, 4096);
        let height = value("--height")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(800)
            .clamp(100, 8192);
        let headless_profile = value("--profile")
            .map(PathBuf::from)
            .map(profile::Profile::open)
            .transpose()?
            .map(|(profile, _)| profile);
        let cookies = headless_profile
            .as_ref()
            .map(profile::Profile::cookies)
            .unwrap_or_default();
        let viewport = rust_browser_engine::layout::Viewport {
            width: width as f32,
            height: height as f32,
            ..Default::default()
        };
        let storage = headless_profile
            .as_ref()
            .map(profile::Profile::storage)
            .unwrap_or_default();
        let (mut page, mut runtime, mut network) =
            headless::load(&url, cookies, viewport, storage)?;
        if let Some(id) = value("--click-id") {
            let target = page
                .document
                .connected_nodes()
                .find(|&node| page.document.attr(node, "id") == Some(id.as_str()))
                .ok_or_else(|| format!("Click target #{id} does not exist"))?;
            if let Some(runtime) = &mut runtime {
                for _ in 0..value("--click-count")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 100)
                {
                    runtime.click(target);
                    file_picker::answer_headless(runtime, &mut file_picker_responses)?;
                }
                runtime.snapshot(&mut page);
            }
        }
        if runtime.is_some() {
            headless::run_tasks(
                &mut network,
                &mut runtime,
                &mut page,
                value("--run-for-ms").and_then(|value| value.parse().ok()),
                &mut file_picker_responses,
            )?;
        }
        if let Some(path) = value("--dump-dom") {
            std::fs::write(path, page.document.serialize()).map_err(|e| e.to_string())?;
        }
        if let Some(profile) = &headless_profile {
            profile.save_storage()?;
        }
        let mut fonts = Fonts::new()?;
        let layout = if let Some(runtime) = &runtime {
            runtime
                .layout_for_viewport(runtime.viewport())
                .map_err(|e| e.to_string())?
        } else {
            std::sync::Arc::new(Layout::build_with_viewport(&page, &fonts, viewport))
        };
        let mut paint_limited = false;
        if let Some(path) = value("--screenshot") {
            let mut canvas = Canvas::new(width, height, layout.background);
            canvas.document(
                &layout,
                &mut fonts,
                0.,
                runtime.as_ref().map_or(0., |r| r.viewport().scroll_y),
            );
            canvas
                .save_png(std::path::Path::new(&path))
                .map_err(|e| e.to_string())?;
            paint_limited = canvas.limited;
        }
        let mut svg_scenes = 0usize;
        let mut svg_unsupported = false;
        for command in &layout.commands {
            if let rust_browser_engine::DrawCommand::Svg { scene, .. } = command {
                svg_scenes += 1;
                svg_unsupported |= scene.diagnostics.unsupported;
            }
        }
        println!(
            "{}",
            serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "url": page.url,
                "title": page.title(),
                "nodes": page.document.nodes.len(),
                "commands": layout.commands.len(),
                "links": layout.links.len(),
                "document_height": layout.height,
                "layout_limited": layout.limited,
                "paint_limited": paint_limited,
                "svg": { "scenes": svg_scenes, "unsupported": svg_unsupported },
                "resources": page.stats.resources,
                "bytes": page.stats.bytes,
                "skipped_resources": page.stats.skipped_resources,
                "load_ms": page.stats.elapsed_ms,
                "glyph_cache_bytes": fonts.cache_bytes(),
                "background": [layout.background.0, layout.background.1, layout.background.2],
                "media": runtime.as_ref().map(ScriptRuntime::media_status).unwrap_or_default(),
                "javascript": {
                    "realm": runtime.is_some(),
                    "executed_scripts": runtime.as_ref().map_or(0, ScriptRuntime::executed_scripts),
                    "pending_requests": runtime.as_ref().map_or(0, ScriptRuntime::pending_network),
                    "diagnostics": runtime.as_ref().map(ScriptRuntime::diagnostics).unwrap_or_default()
                }
            })
        );
        return Ok(());
    }
    let profile = value("--profile")
        .map(PathBuf::from)
        .unwrap_or_else(profile::default_path);
    let url = url
        .as_deref()
        .map(rust_browser_engine::normalize_url)
        .transpose()?;
    #[cfg(all(windows, target_arch = "x86_64"))]
    if !args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--smoke-window" | "--smoke-click-id" | "--smoke-actions"
        )
    }) {
        updater::start_for_installed_launch();
    }
    let smoke_output = value("--smoke-window").map(PathBuf::from);
    let smoke_size = smoke_output.as_ref().and_then(|_| {
        let width = value("--width").and_then(|width| width.parse::<u32>().ok());
        let height = value("--height").and_then(|height| height.parse::<u32>().ok());
        (width.is_some() || height.is_some()).then(|| {
            winit::dpi::PhysicalSize::new(
                width.unwrap_or(1180).clamp(640, 4096),
                height.unwrap_or(840).clamp(400, 4096),
            )
        })
    });
    app::run(
        profile,
        url,
        smoke_output,
        value("--smoke-click-id"),
        value("--smoke-actions").map(PathBuf::from),
        file_picker_responses,
        smoke_size,
        // Register lane 09's native shell here once the facade is integrated.
        None,
    )
}
