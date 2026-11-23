use super::*;
use std::collections::BTreeMap;

impl Builder<'_> {
    pub(super) fn svg_image(
        &mut self,
        id: NodeId,
        rect: Rect,
    ) -> Option<Arc<crate::svg::SvgScene>> {
        let uri = self.page.image_asset_key(id)?;
        let asset = self.page.svg_images.get(&uri)?;
        let key = (
            Arc::as_ptr(asset) as usize,
            rect.width.to_bits(),
            rect.height.to_bits(),
        );
        if let Some(scene) = self.svg_images.get(&key) {
            return Some(scene.clone());
        }
        if self.svg_work_left == 0 || rect.width <= 0. || rect.height <= 0. {
            self.limited |= self.svg_work_left == 0;
            return None;
        }
        let scene = Arc::new(asset.scene(rect.width, rect.height));
        self.limited |= scene.diagnostics.truncated || scene.diagnostics.work > self.svg_work_left;
        self.svg_work_left = self.svg_work_left.saturating_sub(scene.diagnostics.work);
        if self.svg_images.len() < 128 {
            self.svg_images.insert(key, scene.clone());
        }
        Some(scene)
    }
    pub(super) fn draw_svg(&mut self, id: NodeId, rect: Rect) {
        if self.svg_work_left == 0 || rect.width <= 0. || rect.height <= 0. {
            self.limited |= self.svg_work_left == 0;
            return;
        }
        let doc = &self.page.document;
        let mut overrides = BTreeMap::new();
        let mut stack = vec![id];
        let mut remaining = crate::svg::MAX_SVG_NODES;
        while let Some(node) = stack.pop() {
            if remaining == 0 {
                self.limited = true;
                break;
            }
            remaining -= 1;
            if doc.namespace(node) == crate::dom::Namespace::Svg {
                overrides.insert(node, self.styles[node.0].svg_style());
            }
            stack.extend(
                doc.nodes[node.0]
                    .children
                    .iter()
                    .rev()
                    .take(remaining)
                    .copied(),
            );
        }
        overrides.entry(id).or_default().color = Some(self.styles[id.0].color);
        overrides.entry(id).or_default().inherit.remove("color");
        let scene = Arc::new(crate::svg::SvgScene::build(
            doc,
            id,
            rect.width,
            rect.height,
            &overrides,
        ));
        self.limited |= scene.diagnostics.truncated || scene.diagnostics.work > self.svg_work_left;
        self.svg_work_left = self.svg_work_left.saturating_sub(scene.diagnostics.work);
        self.commands.owner = id;
        self.commands.push(DrawCommand::Svg {
            node: id,
            rect,
            scene,
            interactive: true,
        });
    }
}
