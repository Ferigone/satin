use rust_browser_engine::{
    Fonts, Layout,
    layout::{Rect, ScrollOffset},
    paint::Canvas,
};

pub(crate) enum PageRenderer {
    Software,
    #[cfg(feature = "vello-renderer")]
    Vello(Box<rust_browser_engine::paint::vello_renderer::VelloRenderer>),
}

impl PageRenderer {
    pub(crate) fn from_argument(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("software") => Ok(Self::Software),
            #[cfg(feature = "vello-renderer")]
            Some("vello") => rust_browser_engine::paint::vello_renderer::VelloRenderer::new()
                .map(Box::new)
                .map(Self::Vello),
            #[cfg(not(feature = "vello-renderer"))]
            Some("vello") => Err("Vello requires the vello-renderer build feature".into()),
            Some(other) => Err(format!("Unknown document renderer {other:?}")),
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Software => "software",
            #[cfg(feature = "vello-renderer")]
            Self::Vello(_) => "vello",
        }
    }

    pub(crate) fn document(
        &mut self,
        canvas: &mut Canvas,
        layout: &Layout,
        fonts: &mut Fonts,
        viewport: Rect,
        scroll: ScrollOffset,
        scale: f32,
    ) -> Result<(), String> {
        match self {
            Self::Software => {
                canvas.clip(viewport);
                canvas.document_at(layout, fonts, viewport.y, scroll, scale);
                Ok(())
            }
            #[cfg(feature = "vello-renderer")]
            Self::Vello(renderer) => renderer.document(canvas, layout, viewport, scroll, scale),
        }
    }
}
