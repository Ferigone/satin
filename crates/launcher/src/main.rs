#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::{
    path::{Component, PathBuf},
    process::Command,
};

fn main() {
    if let Err(error) = launch() {
        show_error(&error);
        std::process::exit(1);
    }
}
fn launch() -> Result<(), String> {
    let root = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("Brak katalogu instalacji")?
        .to_path_buf();
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("current.json"))
            .map_err(|e| format!("Nie można odczytać wersji przeglądarki: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let relative = PathBuf::from(
        metadata["executable"]
            .as_str()
            .ok_or("Nieprawidłowy manifest wersji")?,
    );
    if relative
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err("Nieprawidłowa ścieżka wersji".into());
    }
    let target = root
        .join(relative)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !target.starts_with(
        root.join("versions")
            .canonicalize()
            .map_err(|e| e.to_string())?,
    ) {
        return Err("Plik przeglądarki znajduje się poza katalogiem wersji".into());
    }
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let cli = args
        .iter()
        .any(|a| a == "--version" || a == "--headless" || a == "--help" || a == "--smoke-window");
    let mut command = Command::new(target);
    command.current_dir(&root).args(&args);
    if !args.iter().any(|a| a == "--profile") {
        command.arg("--profile").arg(root.join("User Data"));
    }
    if cli {
        let status = command.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("Przeglądarka zakończyła działanie: {status}"));
        }
    } else {
        command.spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn show_error(message: &str) {
    eprintln!("Satin: {message}");
    #[cfg(windows)]
    {
        if std::env::args().any(|a| {
            matches!(
                a.as_str(),
                "--headless" | "--smoke-window" | "--version" | "--help"
            )
        }) {
            return;
        }
        let title = "Satin\0".encode_utf16().collect::<Vec<_>>();
        let text = format!("{message}\0").encode_utf16().collect::<Vec<_>>();
        // This isolated platform call only reports local launcher failures.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
}
