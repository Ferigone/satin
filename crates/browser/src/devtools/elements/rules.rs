use super::{Line, preview};
use rust_browser_engine::{
    Layout, LoadedPage,
    css::{Declaration, InspectorStylesheets, declarations},
    dom::{Document, NodeId, NodeKind},
};
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(super) struct Rules {
    index: InspectorStylesheets,
    dom: Weak<Document>,
    selected: Option<NodeId>,
    pub lines: Vec<Line>,
}

impl Rules {
    pub fn sync(
        &mut self,
        page: Option<&LoadedPage>,
        layout: Option<&Arc<Layout>>,
        selected: Option<NodeId>,
    ) {
        let (Some(page), Some(layout), Some(id)) = (page, layout, selected) else {
            *self = Self::default();
            return;
        };
        let sheets_changed = self.index.refresh(page, layout.viewport.width);
        let dom = Arc::downgrade(&page.document);
        if !sheets_changed && self.dom.ptr_eq(&dom) && self.selected == Some(id) {
            return;
        }
        self.dom = dom;
        self.selected = selected;
        self.lines.clear();
        if !page
            .document
            .nodes
            .get(id.0)
            .is_some_and(|n| matches!(n.kind, NodeKind::Element { .. }))
        {
            return;
        }
        let mut remaining = 256usize;
        if let Some(style) = page.document.attr(id, "style") {
            self.lines.push(Line::heading("element.style"));
            if style.len() > 64 * 1024 {
                self.lines.push(Line::value(
                    "Inline style exceeds the 64 KiB rule preview limit",
                ));
            } else {
                for declaration in declarations(style) {
                    if remaining == 0 {
                        break;
                    }
                    self.lines.push(authored(&declaration));
                    remaining -= 1;
                }
            }
        }
        let (matched, partial) = self.index.matching_page(page, id);
        self.lines.push(Line::heading(format!(
            "Direct author matches ({}) · source order",
            matched.len()
        )));
        for (sheet, rule) in matched {
            if remaining == 0 {
                break;
            }
            let s = rule.specificity;
            self.lines.push(Line::heading(preview(&rule.selector, 180)));
            self.lines.push(Line::value(format!(
                "Stylesheet {}   ·   specificity {},{},{}",
                sheet + 1,
                s >> 20,
                (s >> 10) & 1023,
                s & 1023
            )));
            for declaration in &rule.declarations {
                if remaining == 0 {
                    break;
                }
                self.lines.push(authored(declaration));
                remaining -= 1;
            }
        }
        if partial || remaining == 0 {
            self.lines
                .push(Line::heading("Results limited by the inspection budget"));
        }
    }
}
fn authored(d: &Declaration) -> Line {
    Line::value(format!(
        "{}: {}{};",
        preview(&d.name, 96),
        preview(&d.value, 512),
        if d.important { " !important" } else { "" }
    ))
}
