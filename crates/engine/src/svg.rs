//! Independently implemented bounded SVG scene construction and software paint.
//! Presentation attributes/inline declarations are handled locally; embedders
//! may supply post-cascade overrides from the browser's CSS implementation.
#[path = "svg/document.rs"]
mod document;
#[path = "svg/geometry.rs"]
mod geometry;
#[path = "svg/path.rs"]
mod path;
#[path = "svg/raster.rs"]
mod raster;
#[path = "svg/scene.rs"]
mod scene;
#[path = "svg/style.rs"]
mod style;

use crate::{
    css::Color,
    dom::{Document, NodeId},
};
pub use document::{
    MAX_SVG_DOCUMENT_ATTRIBUTES, MAX_SVG_DOCUMENT_SOURCE, MAX_SVG_DOCUMENT_STRINGS,
    SvgDocumentError, SvgDocumentErrorKind, parse_document,
};
pub(crate) use geometry::{Contour, FlatPath, Point};
pub use geometry::{SvgRect, SvgTransform};
pub(crate) use path::{Writer as PathWriter, parse as parse_path};
pub(crate) use raster::{DevicePath, device_path, intersection, intervals};
pub(crate) use scene::{StrokeStyle, stroke};
use std::collections::BTreeMap;
pub use style::{FillRule, LineCap, LineJoin, SvgPaint, SvgStyle, parse_paint};

/// All coordinates and geometry remain bounded even for malformed remote SVG.
pub const MAX_SVG_NODES: usize = 16_384;
pub const MAX_SVG_SEGMENTS: usize = 65_536;
pub const MAX_SVG_DEPTH: usize = 64;
pub const MAX_SVG_COMMANDS: usize = 16_384;
pub const MAX_SVG_WORK: usize = 4_000_000;
pub const MAX_SVG_RENDER_WORK: usize = 16_000_000;
pub const MAX_SVG_PAINT_PIXELS: usize = 4_194_304;
pub const MAX_SVG_COORDINATE: f64 = 1_000_000.;

#[derive(Default)]
pub(crate) struct Budget {
    pub diagnostics: SvgDiagnostics,
}
impl Budget {
    pub fn work(&mut self, amount: usize) -> bool {
        if self.diagnostics.work.saturating_add(amount) > MAX_SVG_WORK {
            self.diagnostics.truncated = true;
            false
        } else {
            self.diagnostics.work += amount;
            true
        }
    }
    pub fn segment(&mut self) -> bool {
        if self.diagnostics.segments >= MAX_SVG_SEGMENTS {
            self.diagnostics.truncated = true;
            false
        } else {
            self.diagnostics.segments += 1;
            self.work(1)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SvgDiagnostics {
    pub truncated: bool,
    pub invalid_geometry: bool,
    pub unsupported: bool,
    pub nodes: usize,
    pub segments: usize,
    pub work: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SvgRenderReport {
    pub truncated: bool,
    pub invalid_surface: bool,
    pub work: usize,
    pub pixels: usize,
}

#[derive(Clone, Debug)]
pub struct SvgScene {
    pub width: f32,
    pub height: f32,
    pub diagnostics: SvgDiagnostics,
    /// Geometry bounds in root viewport coordinates, including group unions.
    /// These are broad-phase bounds; hit_test applies actual fills/strokes/clips.
    pub node_bounds: BTreeMap<NodeId, SvgRect>,
    /// Unclipped object geometry bounds in root viewport coordinates, without
    /// stroke expansion. Includes geometry with fill:none or visibility:hidden
    /// and unions into rendered group/use ancestors; display:none is omitted.
    pub geometry_bounds: BTreeMap<NodeId, SvgRect>,
    pub(crate) commands: Vec<scene::Command>,
}
impl SvgScene {
    pub fn build(
        document: &Document,
        root: NodeId,
        width: f32,
        height: f32,
        styles: &BTreeMap<NodeId, SvgStyle>,
    ) -> Self {
        scene::build(document, root, width, height, styles)
    }
    /// Intrinsic width/height and viewBox aspect ratio for replaced-box sizing.
    /// Unspecified dimensions use the SVG replaced-element defaults 300x150.
    pub fn intrinsic_size(document: &Document, root: NodeId) -> (f32, f32) {
        scene::intrinsic_size(document, root)
    }
    /// Paint into the caller's packed 0x00RRGGBB canvas. The target rectangle maps
    /// root viewport units into canvas coordinates; clip is in canvas pixels.
    pub fn paint(
        &self,
        pixels: &mut [u32],
        width: u32,
        height: u32,
        target: SvgRect,
        clip: SvgRect,
    ) -> SvgRenderReport {
        raster::paint(self, pixels, width, height, target, clip)
    }
    pub fn paint_transformed<F: Fn(f32, f32) -> bool>(
        &self,
        pixels: &mut [u32],
        width: u32,
        height: u32,
        local_to_device: [f32; 6],
        clip_bbox: SvgRect,
        inside_clip: F,
    ) -> SvgRenderReport {
        raster::paint_transformed(
            self,
            pixels,
            width,
            height,
            local_to_device,
            clip_bbox,
            inside_clip,
        )
    }
    /// SVG-local coordinates, in painter's reverse order. Transparent paint is
    /// still painted geometry for SVG's default visiblePainted pointer behavior.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        raster::hit_test(self, x, y)
    }
}

pub(crate) fn alpha(color: Color, opacity: f32) -> Color {
    Color(
        color.0,
        color.1,
        color.2,
        (f32::from(color.3) * opacity.clamp(0., 1.)).round() as u8,
    )
}
