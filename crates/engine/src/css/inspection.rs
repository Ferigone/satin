//! Optional author-rule index, built only for the native inspector.
use super::{Rule, Selector, compile_selector, parse, stylesheet_media_applies, without_comments};
use crate::{
    LoadedPage,
    dom::{Document, NodeId, NodeKind},
};
use std::{
    mem::size_of,
    sync::{Arc, Weak},
};

impl LoadedPage {
    pub(crate) fn stylesheet_applies(&self, index: usize, width: f32) -> bool {
        self.stylesheet_media
            .get(index)
            .and_then(Option::as_deref)
            .is_none_or(|query| stylesheet_media_applies(query, width))
    }
}

#[derive(Default)]
pub struct InspectorStylesheets {
    sheets: Weak<Vec<Arc<str>>>,
    media: Weak<Vec<Option<Arc<str>>>>,
    width: Option<f32>,
    rules: Vec<(usize, Selector, Rule)>,
    limited: bool,
}

impl InspectorStylesheets {
    /// A viewport-width change also changes which @media rules enter the cascade.
    pub fn refresh(&mut self, page: &LoadedPage, width: f32) -> bool {
        let sheets = Arc::downgrade(&page.stylesheets);
        let media = Arc::downgrade(&page.stylesheet_media);
        if self.width == Some(width) && self.sheets.ptr_eq(&sheets) && self.media.ptr_eq(&media) {
            return false;
        }
        self.sheets = sheets;
        self.media = media;
        self.width = Some(width);
        self.limited = page.stylesheets.len() > 64;
        self.rules.clear();
        let mut source_bytes = 0;
        let mut stored_bytes = 0;
        'sheets: for (i, sheet) in page.stylesheets.iter().take(64).enumerate() {
            if !page.stylesheet_applies(i, width) {
                continue;
            }
            source_bytes += sheet.len();
            if source_bytes > 1024 * 1024 {
                self.limited = true;
                break;
            }
            for rule in parse(sheet, width) {
                let cost = rule.selector.len()
                    + rule
                        .declarations
                        .iter()
                        .map(|d| d.name.len() + d.value.len() + size_of::<super::Declaration>())
                        .sum::<usize>();
                stored_bytes += cost;
                if self.rules.len() >= 1024 || stored_bytes > 512 * 1024 {
                    self.limited = true;
                    break 'sheets;
                }
                if let Some(selector) = compile_selector(&without_comments(&rule.selector), 0) {
                    self.rules.push((i, selector, rule));
                }
            }
        }
        true
    }

    /// Direct author matches in source order. The boolean reports a truncated search.
    pub fn matching(&self, doc: &Document, id: NodeId) -> (Vec<(usize, &Rule)>, bool) {
        self.matching_with_form_state(doc, id, &|node| {
            crate::script::default_placeholder_shown(doc, node)
        })
    }
    pub fn matching_page(&self, page: &LoadedPage, id: NodeId) -> (Vec<(usize, &Rule)>, bool) {
        self.matching_with_form_state(&page.document, id, &|node| page.placeholder_shown(node))
    }
    fn matching_with_form_state(
        &self,
        doc: &Document,
        id: NodeId,
        placeholder: &dyn Fn(NodeId) -> bool,
    ) -> (Vec<(usize, &Rule)>, bool) {
        let mut found = Vec::new();
        if !doc
            .nodes
            .get(id.0)
            .is_some_and(|node| matches!(node.kind, NodeKind::Element { .. }))
        {
            return (found, false);
        }
        let mut remaining = 131_072;
        for (sheet, selector, rule) in &self.rules {
            // Use exactly the normal per-selector budget until the total inspector work expires.
            if remaining < 16_384 {
                return (found, true);
            }
            let mut fuel = 16_384;
            let matches =
                selector.matches_part(doc, id, selector.parts.len() - 1, &mut fuel, placeholder);
            remaining -= 16_384 - fuel;
            if matches {
                if found.len() == 64 {
                    return (found, true);
                }
                found.push((*sheet, rule));
            }
        }
        (found, self.limited)
    }
}
