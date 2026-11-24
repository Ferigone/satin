//! Independent web-engine implementation. Platform crates supply transport,
//! shaping and pixels; HTML, CSS, DOM and document layout live in this crate.
#![forbid(unsafe_code)]
mod canvas;
pub mod cookies;
pub mod css;
pub mod data_url;
pub mod dom;
pub mod fonts;
pub mod history;
pub mod html;
pub mod layout;
pub mod loader;
pub mod mime;
pub mod network;
pub mod paint;
mod raw_json;
pub mod script;
pub mod svg;
pub mod web_storage;

pub use fonts::Fonts;
pub use layout::{DrawCommand, Layout, Rect};
pub use loader::{LoadedPage, Loader, normalize_url};
