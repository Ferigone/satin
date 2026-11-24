use super::*;
use crate::{
    css::{Style, Stylesheet},
    dom::Namespace,
    svg::{SvgScene, SvgStyle},
};
use std::collections::BTreeMap;

pub(super) fn svg_mime(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("image/svg+xml"))
}
/// An inert SVG image resource. It has no script realm, external references or
/// access to the embedding DOM. Only scene geometry and local CSS are evaluated.
#[derive(Clone, Debug)]
pub struct SvgImage {
    document: Arc<Document>,
    root: NodeId,
    sheets: Vec<String>,
    retained: usize,
}
impl SvgImage {
    pub fn from_source(source: &str) -> Result<Self, String> {
        if source.len() > RESOURCE_LIMIT {
            return Err("SVG source exceeds 4 MiB".into());
        }
        let document = crate::svg::parse_document(source).map_err(|error| error.to_string())?;
        if document.truncated || document.nodes.len() > crate::svg::MAX_SVG_NODES {
            return Err("SVG document exceeds node budget".into());
        }
        let root = document
            .connected_nodes()
            .find(|&id| document.namespace(id) == Namespace::Svg && document.tag(id) == "svg")
            .ok_or_else(|| "SVG image has no supported SVG root".to_owned())?;
        let sheets = document
            .connected_nodes()
            .filter(|&id| document.tag(id) == "style")
            .take(128)
            .map(|id| document.text(id))
            .collect::<Vec<_>>();
        let retained = source
            .len()
            .saturating_mul(2)
            .saturating_add(document.nodes.len().saturating_mul(256));
        Ok(Self {
            document: Arc::new(document),
            root,
            sheets,
            retained,
        })
    }
    pub fn retained_bytes(&self) -> usize {
        self.retained
    }
    pub fn intrinsic_size(&self) -> (f32, f32) {
        SvgScene::intrinsic_size(&self.document, self.root)
    }
    pub fn scene(&self, width: f32, height: f32) -> SvgScene {
        let mut rules = vec![];
        for sheet in &self.sheets {
            rules.extend(crate::css::parse(sheet, width));
            if rules.len() > 8192 {
                rules.truncate(8192);
                break;
            }
        }
        let stylesheet = Stylesheet::with_viewport(rules, width, height);
        let mut computed = vec![Style::default(); self.document.nodes.len()];
        let mut stack = vec![self.document.root];
        let mut root_font = 16.;
        let mut overrides = BTreeMap::<NodeId, SvgStyle>::new();
        while let Some(id) = stack.pop() {
            let n = &self.document.nodes[id.0];
            let parent = n.parent.map(|p| computed[p.0].clone()).unwrap_or_default();
            computed[id.0] = if matches!(n.kind, NodeKind::Element { .. }) {
                stylesheet.compute(&self.document, id, &parent, root_font)
            } else {
                parent
            };
            if id == self.root {
                root_font = computed[id.0].font_size;
            }
            if self.document.namespace(id) == Namespace::Svg {
                overrides.insert(id, computed[id.0].svg_style());
            }
            stack.extend(n.children.iter().rev().copied());
        }
        overrides.entry(self.root).or_default().color = Some(computed[self.root.0].color);
        overrides
            .entry(self.root)
            .or_default()
            .inherit
            .remove("color");
        // In image mode an SVG without a viewBox uses its root dimensions
        // as the source coordinate space. The painter maps that space onto
        // the concrete image box, including independent horizontal/vertical scaling.
        let image_axis = |name, target: f32| {
            self.document
                .attr(self.root, name)
                .and_then(|value| crate::svg::length(value, f64::from(target)))
                .filter(|&value| value > 0.)
                .map_or(target, |value| value as f32)
        };
        let (width, height) = if self.document.attr(self.root, "viewBox").is_some() {
            (width, height)
        } else {
            (image_axis("width", width), image_axis("height", height))
        };
        SvgScene::build(&self.document, self.root, width, height, &overrides)
    }
}
