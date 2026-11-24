//! Native inspection of the same DOM and layout snapshots used for painting.
#[path = "elements/editor.rs"]
mod editor;
#[path = "elements/rules.rs"]
mod rules;

use super::panel;
use rust_browser_engine::{
    Fonts, Layout, Rect,
    css::Color,
    dom::{Document, NodeId, NodeKind},
    paint::Canvas,
};
use std::{
    collections::HashSet,
    sync::{Arc, Weak},
};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const BAR: f32 = 32.;
const ROW: f32 = 21.;
const INDENT: f32 = 13.;
const SEARCH_LIMIT: usize = 20_000;
const TEXT_LIMIT: usize = 1024;
const BG: Color = Color(31, 30, 33, 255);
const CHROME: Color = Color(39, 38, 42, 255);
const ACTIVE: Color = Color(60, 52, 77, 255);
const INK: Color = Color(239, 236, 241, 255);
const DIM: Color = Color(181, 175, 187, 255);
const TAG: Color = Color(174, 201, 243, 255);
const ACCENT: Color = Color(205, 184, 251, 255);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Detail {
    #[default]
    Node,
    Computed,
    Rules,
}

#[derive(Clone, Copy)]
struct TreeRow {
    id: NodeId,
    depth: usize,
}

struct Line {
    text: String,
    heading: bool,
}

impl Line {
    fn heading(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            heading: true,
        }
    }
    fn value(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            heading: false,
        }
    }
}

#[derive(Default)]
pub(super) struct Elements {
    dom: Weak<Document>,
    layout: Weak<Layout>,
    toggled: HashSet<NodeId>,
    rows: Vec<TreeRow>,
    limited: bool,
    selected: Option<NodeId>,
    bounds: Option<Rect>,
    node_lines: Vec<Line>,
    css_lines: Vec<Line>,
    rules: rules::Rules,
    tree_offset: f32,
    detail_offset: f32,
    detail: Detail,
    narrow_details: bool,
    searching: bool,
    query: String,
    editor: Option<editor::Editor>,
    pending_edit: Option<(NodeId, String, String, bool)>,
    edit_status: String,
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width: width.max(0.),
        height: height.max(0.),
    }
}

struct Panes {
    search: Rect,
    buttons: [Rect; 4], // Tree (narrow only), Node, Rules, Computed
    tree: Rect,
    details: Rect,
    footer: Rect,
}

impl Panes {
    fn new(b: Rect, narrow_details: bool) -> Self {
        let wide = b.width >= 520.;
        let bar_h = BAR.min(b.height);
        let footer_h = (b.height - bar_h - ROW).clamp(0., 19.);
        let content = rect(b.x, b.y + bar_h, b.width, b.height - bar_h - footer_h);
        let split = if wide {
            (b.width * 0.51).round()
        } else {
            b.width
        };
        let button_w = if wide { 74. } else { 58_f32.min(b.width / 6.) };
        let start = if wide {
            b.x + split
        } else {
            b.x + b.width - button_w * 4.
        };
        let search = rect(b.x + 5., b.y + 3., start - b.x - 11., bar_h - 6.);
        let buttons = if wide {
            [
                Rect::default(),
                rect(start + 1., b.y, button_w, bar_h),
                rect(start + 1. + button_w, b.y, button_w, bar_h),
                rect(start + 1. + 2. * button_w, b.y, button_w + 8., bar_h),
            ]
        } else {
            std::array::from_fn(|i| rect(start + button_w * i as f32, b.y, button_w, bar_h))
        };
        let tree = if wide {
            rect(content.x, content.y, split - 1., content.height)
        } else if narrow_details {
            Rect::default()
        } else {
            content
        };
        let details = if wide {
            rect(start + 1., content.y, b.width - split - 1., content.height)
        } else if narrow_details {
            content
        } else {
            Rect::default()
        };
        Self {
            search,
            buttons,
            tree,
            details,
            footer: rect(b.x, content.y + content.height, b.width, footer_h),
        }
    }
}

fn preview(s: &str, limit: usize) -> String {
    let mut chars = s.chars();
    let mut text: String = chars
        .by_ref()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

fn caption(doc: &Document, id: NodeId) -> String {
    match &doc.nodes[id.0].kind {
        NodeKind::Document => "#document".into(),
        NodeKind::Doctype { name, .. } => format!("<!DOCTYPE {}>", preview(name, 48)),
        NodeKind::Element { tag, attrs } => {
            let mut result = format!("<{}", preview(tag, 56));
            for name in ["id", "class"] {
                if let Some(value) = attrs.get(name) {
                    result.push_str(&format!(" {name}=\"{}\"", preview(value, 65)));
                }
            }
            result.push('>');
            result
        }
        NodeKind::Text(s) => format!("\"{}\"", preview(s.trim_start(), 120)),
        NodeKind::Comment(s) => format!("<!-- {} -->", preview(s, 100)),
    }
}

fn find(doc: &Document, id: NodeId, needle: &str) -> bool {
    let matches = |s: &str| preview(s, 512).to_ascii_lowercase().contains(needle);
    match &doc.nodes[id.0].kind {
        NodeKind::Element { tag, attrs } => {
            if let Some(pattern) = needle.strip_prefix('#') {
                return attrs
                    .get("id")
                    .is_some_and(|s| preview(s, 512).to_ascii_lowercase().contains(pattern));
            }
            if let Some(pattern) = needle.strip_prefix('.') {
                return attrs.get("class").is_some_and(|s| {
                    preview(s, 512)
                        .split_ascii_whitespace()
                        .any(|c| c.eq_ignore_ascii_case(pattern))
                });
            }
            matches(tag) || attrs.iter().take(64).any(|(k, v)| matches(k) || matches(v))
        }
        NodeKind::Text(s) | NodeKind::Comment(s) => matches(s),
        NodeKind::Doctype { name, .. } => matches(name),
        NodeKind::Document => matches("#document"),
    }
}

fn branch_open(doc: &Document, id: NodeId, toggled: &HashSet<NodeId>) -> bool {
    let default = id == doc.root || matches!(doc.tag(id), "html" | "body");
    default != toggled.contains(&id)
}

fn visible(doc: &Document, toggled: &HashSet<NodeId>, query: &str) -> (Vec<TreeRow>, bool) {
    let mut rows = Vec::new();
    let mut pending = vec![TreeRow {
        id: doc.root,
        depth: 0,
    }];
    let needle = query.to_ascii_lowercase();
    let mut visited = 0;
    while let Some(row) = pending.pop() {
        if !query.is_empty() && visited == SEARCH_LIMIT {
            return (rows, true);
        }
        visited += 1;
        let node = &doc.nodes[row.id.0];
        if !matches!(&node.kind, NodeKind::Text(s) if s.trim().is_empty())
            && (query.is_empty() || find(doc, row.id, &needle))
        {
            rows.push(row);
        }
        if !query.is_empty() || branch_open(doc, row.id, toggled) {
            pending.extend(node.children.iter().rev().map(|&id| TreeRow {
                id,
                depth: row.depth + 1,
            }));
        }
    }
    (rows, false)
}

fn offset(value: f32, count: usize, pane: Rect) -> f32 {
    if value.is_nan() {
        return 0.;
    }
    value.clamp(0., (count as f32 * ROW - pane.height).max(0.))
}

impl Elements {
    pub(super) fn sync_page(
        &mut self,
        page: Option<&rust_browser_engine::LoadedPage>,
        layout: Option<&Arc<Layout>>,
    ) {
        self.sync(page.map(|page| &page.document), layout);
        if self.detail == Detail::Rules {
            self.rules.sync(page, layout, self.selected);
        }
    }

    pub(super) fn sync(&mut self, dom: Option<&Arc<Document>>, layout: Option<&Arc<Layout>>) {
        let next_dom = dom.map(Arc::downgrade).unwrap_or_default();
        let next_layout = layout.map(Arc::downgrade).unwrap_or_default();
        let dom_changed = !self.dom.ptr_eq(&next_dom);
        let layout_changed = !self.layout.ptr_eq(&next_layout);
        self.dom = next_dom;
        self.layout = next_layout;
        if dom_changed {
            self.rebuild();
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|edit| Some(edit.node) != self.selected)
        {
            self.editor = None;
            self.pending_edit = None;
            self.edit_status.clear();
        }
        if dom_changed || layout_changed {
            self.update_details();
        }
    }

    fn rebuild(&mut self) {
        let Some(doc) = self.dom.upgrade() else {
            self.rows.clear();
            self.selected = None;
            self.limited = false;
            return;
        };
        (self.rows, self.limited) = visible(&doc, &self.toggled, &self.query);
        if self
            .selected
            .is_none_or(|id| !self.rows.iter().any(|r| r.id == id))
        {
            self.selected = if self.query.is_empty() {
                self.rows
                    .iter()
                    .find(|r| doc.tag(r.id) == "body")
                    .or(self.rows.first())
            } else {
                self.rows.first()
            }
            .map(|r| r.id);
            self.detail_offset = 0.;
        }
    }

    fn update_details(&mut self) {
        self.node_lines.clear();
        self.css_lines.clear();
        self.bounds = None;
        let (Some(doc), Some(id)) = (self.dom.upgrade(), self.selected) else {
            return;
        };
        let Some(node) = doc.nodes.get(id.0) else {
            return;
        };
        let lines = &mut self.node_lines;
        lines.push(Line::heading(caption(&doc, id)));
        if let NodeKind::Element { tag, attrs } = &node.kind {
            lines.push(Line::value(format!(
                "{}  ·  {} child nodes",
                preview(tag, 64),
                node.children.len()
            )));
            let mut trail = Vec::new();
            let mut parent = node.parent;
            while let Some(ancestor) = parent {
                if trail.len() == 8 {
                    trail.push("…".to_owned());
                    break;
                }
                let tag = doc.tag(ancestor);
                if !tag.is_empty() {
                    trail.push(preview(tag, 48));
                }
                parent = doc.nodes[ancestor.0].parent;
            }
            trail.reverse();
            if !trail.is_empty() {
                lines.push(Line::value(trail.join(" › ")))
            }
            if let Some(layout) = self.layout.upgrade() {
                self.bounds = layout.rect_for(id);
                let computed = layout.inspect_computed_style(&doc, id);
                lines.push(Line::heading("Box model · CSS pixels"));
                if let Some(r) = self.bounds {
                    lines.push(Line::value(format!(
                        "Visual bounds   {:.1} × {:.1}   at {:.1}, {:.1}",
                        r.width, r.height, r.x, r.y
                    )));
                } else {
                    lines.push(Line::value("No rendered box"));
                }
                for (label, prefix, suffix) in [
                    ("Margin", "margin-", ""),
                    ("Border", "border-", "-width"),
                    ("Padding", "padding-", ""),
                ] {
                    let edges = ["top", "right", "bottom", "left"].map(|edge| {
                        let name = format!("{prefix}{edge}{suffix}");
                        computed
                            .iter()
                            .find(|(k, _)| *k == name)
                            .map_or("—", |(_, v)| v.as_str())
                    });
                    lines.push(Line::value(format!("{label}   {}", edges.join("   "))));
                }
                self.css_lines = computed
                    .into_iter()
                    .map(|(key, value)| {
                        Line::value(format!("{key}: {}", preview(&value, TEXT_LIMIT)))
                    })
                    .collect();
            }
            lines.push(Line::heading(format!("Attributes ({})", attrs.len())));
            for (name, value) in attrs.iter().take(256) {
                lines.push(Line::value(format!(
                    "{} = \"{}\"",
                    preview(name, 128),
                    preview(value, TEXT_LIMIT)
                )));
            }
            if attrs.len() > 256 {
                lines.push(Line::value("Showing the first 256 attributes"))
            }
            lines.push(Line::heading("Namespace"));
            lines.push(Line::value(doc.namespace(id).uri()));
        } else {
            lines.push(Line::value(format!("{} child nodes", node.children.len())));
            match &node.kind {
                NodeKind::Comment(s) | NodeKind::Text(s) => {
                    lines.push(Line::value(preview(s, TEXT_LIMIT)))
                }
                NodeKind::Doctype {
                    public_id,
                    system_id,
                    ..
                } => {
                    for v in [public_id, system_id].into_iter().flatten() {
                        lines.push(Line::value(preview(v, TEXT_LIMIT)));
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) fn selected_rect(&self) -> Option<Rect> {
        self.bounds
    }

    pub(super) fn inspect(&mut self, id: NodeId, bounds: Rect) {
        let Some(doc) = self.dom.upgrade() else {
            return;
        };
        let Some(node) = doc.nodes.get(id.0) else {
            return;
        };
        let mut cursor = id;
        while let Some(parent) = doc.nodes[cursor.0].parent {
            cursor = parent;
        }
        if cursor != doc.root {
            return;
        }
        self.searching = false;
        self.query.clear();
        let mut parent = node.parent;
        while let Some(p) = parent {
            if !branch_open(&doc, p, &self.toggled) && !self.toggled.remove(&p) {
                self.toggled.insert(p);
            }
            parent = doc.nodes[p.0].parent;
        }
        self.narrow_details = false;
        self.selected = Some(id);
        self.editor = None;
        self.edit_status.clear();
        self.rebuild();
        self.reveal(bounds);
        self.detail_offset = 0.;
        self.update_details();
    }

    fn reveal(&mut self, bounds: Rect) {
        let pane = Panes::new(bounds, self.narrow_details).tree;
        let Some(index) = self
            .rows
            .iter()
            .position(|row| Some(row.id) == self.selected)
        else {
            return;
        };
        let start = index as f32 * ROW;
        self.tree_offset = offset(
            self.tree_offset
                .clamp((start + ROW - pane.height).min(start).max(0.), start),
            self.rows.len(),
            pane,
        );
    }

    fn select(&mut self, index: usize, bounds: Rect) {
        if let Some(row) = self.rows.get(index) {
            self.selected = Some(row.id);
            self.detail_offset = 0.;
            self.reveal(bounds);
            self.update_details();
        }
    }

    fn toggle(&mut self, id: NodeId) {
        if !self.toggled.remove(&id) {
            self.toggled.insert(id);
        }
        self.rebuild();
        self.update_details();
    }

    fn filter_changed(&mut self) {
        self.tree_offset = 0.;
        self.detail_offset = 0.;
        self.selected = None;
        self.rebuild();
        self.update_details();
    }
    fn edit_button(footer: Rect) -> Rect {
        let width = 85_f32.min(footer.width / 2.);
        rect(
            footer.x + footer.width - width,
            footer.y,
            width,
            footer.height,
        )
    }

    fn editable(&self) -> bool {
        self.dom
            .upgrade()
            .zip(self.selected)
            .is_some_and(|(doc, id)| {
                doc.nodes
                    .get(id.0)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Element { .. }))
            })
    }

    fn open_editor(&mut self, property: &str, computed: &str) {
        let Some(doc) = self.dom.upgrade() else {
            return;
        };
        let Some(id) = self.selected.filter(|_| self.editable()) else {
            return;
        };
        let declarations =
            rust_browser_engine::css::declarations(doc.attr(id, "style").unwrap_or(""));
        let authored = declarations
            .iter()
            .rev()
            .filter(|d| d.name == property)
            .find(|d| d.important)
            .or_else(|| declarations.iter().rev().find(|d| d.name == property));
        let value = authored.map_or(computed, |d| d.value.as_str());
        self.editor = Some(editor::Editor::new(
            id,
            property,
            value,
            authored.is_some_and(|d| d.important),
        ));
        self.edit_status = "Edit inline CSS   ·   Enter applies   ·   Esc cancels".into();
        self.narrow_details = true;
        self.searching = false;
    }

    fn editor_action(&mut self, action: editor::Action) {
        match action {
            editor::Action::None => {}
            editor::Action::Cancel => {
                self.editor = None;
                self.edit_status.clear();
            }
            editor::Action::Apply | editor::Action::Remove => {
                if let Some(editor) = &self.editor {
                    let data = editor.data(matches!(action, editor::Action::Remove));
                    if data.1.is_empty() {
                        self.edit_status = "Enter a CSS property name".into();
                    } else {
                        self.pending_edit = Some(data);
                    }
                }
            }
        }
    }

    pub(super) fn take_style_edit(&mut self) -> Option<(NodeId, String, String, bool)> {
        self.pending_edit.take()
    }

    pub(super) fn clipboard_selection(&mut self, cut: bool) -> Option<String> {
        self.editor.as_mut()?.clipboard_selection(cut)
    }

    pub(super) fn style_edit_result(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.editor = None;
                self.edit_status = "Inline style updated".into();
            }
            Err(message) => self.edit_status = preview(&message, 200),
        }
    }

    pub(super) fn click(&mut self, bounds: Rect, x: f32, y: f32) {
        let p = Panes::new(bounds, self.narrow_details);
        if let Some(editor) = &mut self.editor
            && panel::contains(p.details, x, y)
        {
            let action = editor.click(p.details, x, y);
            self.editor_action(action);
            return;
        }
        if self.editable() && panel::contains(Self::edit_button(p.footer), x, y) {
            self.open_editor("", "");
            return;
        }
        self.editor = None;
        self.edit_status.clear();
        if panel::contains(p.search, x, y) {
            self.searching = true;
            self.narrow_details = false;
            return;
        }
        self.searching = false;
        if let Some(i) = p.buttons.iter().position(|b| panel::contains(*b, x, y)) {
            self.narrow_details = i != 0;
            self.detail = match i {
                2 => Detail::Rules,
                3 => Detail::Computed,
                _ => Detail::Node,
            };
            self.detail_offset = 0.;
        } else if panel::contains(p.tree, x, y) {
            let top = offset(self.tree_offset, self.rows.len(), p.tree);
            let index = ((y - p.tree.y + top) / ROW).floor() as usize;
            let Some(&row) = self.rows.get(index) else {
                return;
            };
            self.narrow_details = false;
            self.select(index, bounds);
            if self.query.is_empty()
                && x < p.tree.x + 22. + indent(row.depth, p.tree)
                && self
                    .dom
                    .upgrade()
                    .is_some_and(|doc| !doc.nodes[row.id.0].children.is_empty())
            {
                self.toggle(row.id);
            }
        } else if panel::contains(p.details, x, y) {
            if self.detail == Detail::Computed {
                let columns = wrap_columns(p.details);
                let count = wrapped_count(self.detail_lines(), p.details);
                let mut row = ((y - p.details.y + offset(self.detail_offset, count, p.details))
                    / ROW)
                    .floor() as usize;
                let property = self.css_lines.iter().find_map(|line| {
                    let height = wrapped_height(&line.text, columns);
                    if row < height {
                        line.text
                            .split_once(": ")
                            .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    } else {
                        row -= height;
                        None
                    }
                });
                if let Some((name, value)) = property {
                    self.open_editor(&name, &value);
                }
            }
            self.narrow_details = true;
        }
    }

    fn detail_lines(&self) -> &[Line] {
        match self.detail {
            Detail::Node => &self.node_lines,
            Detail::Computed => &self.css_lines,
            Detail::Rules => &self.rules.lines,
        }
    }

    pub(super) fn scroll(&mut self, bounds: Rect, x: f32, y: f32, delta: f32) {
        let p = Panes::new(bounds, self.narrow_details);
        if panel::contains(p.tree, x, y) {
            self.tree_offset = offset(
                offset(self.tree_offset, self.rows.len(), p.tree) + delta,
                self.rows.len(),
                p.tree,
            );
        } else if panel::contains(p.details, x, y) {
            let count = wrapped_count(self.detail_lines(), p.details);
            self.detail_offset = offset(
                offset(self.detail_offset, count, p.details) + delta,
                count,
                p.details,
            );
        }
    }

    pub(super) fn key(&mut self, bounds: Rect, key: &Key, modifiers: ModifiersState) -> bool {
        if let Some(editor) = &mut self.editor {
            let action = editor.key(key, modifiers);
            self.editor_action(action);
            return true;
        }
        if *key == Key::Named(NamedKey::F2) {
            self.open_editor("", "");
            return true;
        }
        if modifiers.control_key()
            && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("f"))
        {
            self.searching = true;
            self.narrow_details = false;
            return true;
        }
        if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() {
            return false;
        }
        if self.searching {
            match key {
                Key::Character(value) => {
                    for c in value.chars().filter(|c| !c.is_control()) {
                        if self.query.len() + c.len_utf8() > 128 {
                            break;
                        }
                        self.query.push(c);
                    }
                }
                Key::Named(NamedKey::Backspace) => {
                    self.query.pop();
                }
                Key::Named(NamedKey::Escape) => {
                    self.searching = false;
                    self.query.clear();
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                    let index = self
                        .rows
                        .iter()
                        .position(|r| Some(r.id) == self.selected)
                        .unwrap_or(0);
                    let next = if *key == Key::Named(NamedKey::ArrowUp) {
                        index.saturating_sub(1)
                    } else {
                        index + 1
                    };
                    self.select(next, bounds);
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(id) = self.selected {
                        self.inspect(id, bounds);
                    } else {
                        self.searching = false;
                    }
                    return true;
                }
                _ => return false,
            }
            self.filter_changed();
            return true;
        }
        if *key == Key::Named(NamedKey::Tab) {
            // Tree → node → matching rules → computed style, also on compact docks.
            let current = if self.narrow_details {
                match self.detail {
                    Detail::Node => 1,
                    Detail::Rules => 2,
                    Detail::Computed => 3,
                }
            } else {
                0
            };
            let next = (current + if modifiers.shift_key() { 3 } else { 1 }) % 4;
            self.narrow_details = next != 0;
            self.detail = match next {
                2 => Detail::Rules,
                3 => Detail::Computed,
                _ => Detail::Node,
            };
            self.detail_offset = 0.;
            return true;
        }
        let p = Panes::new(bounds, self.narrow_details);
        if self.narrow_details {
            let count = wrapped_count(self.detail_lines(), p.details);
            let old = offset(self.detail_offset, count, p.details);
            let new = match key {
                Key::Named(NamedKey::ArrowDown) => old + ROW,
                Key::Named(NamedKey::ArrowUp) => old - ROW,
                Key::Named(NamedKey::PageDown) => old + p.details.height,
                Key::Named(NamedKey::PageUp) => old - p.details.height,
                Key::Named(NamedKey::Home) => 0.,
                Key::Named(NamedKey::End) => f32::MAX,
                Key::Named(NamedKey::Escape) => {
                    self.narrow_details = false;
                    return true;
                }
                _ => return false,
            };
            self.detail_offset = offset(new, count, p.details);
            return true;
        }
        let Some(doc) = self.dom.upgrade() else {
            return false;
        };
        let Some(index) = self.rows.iter().position(|r| Some(r.id) == self.selected) else {
            return false;
        };
        let id = self.rows[index].id;
        let page_rows = (p.tree.height / ROW).floor().max(1.) as usize;
        let last = self.rows.len().saturating_sub(1);
        let next = match key {
            Key::Named(NamedKey::ArrowDown) => (index + 1).min(last),
            Key::Named(NamedKey::ArrowUp) => index.saturating_sub(1),
            Key::Named(NamedKey::Home) => 0,
            Key::Named(NamedKey::End) => last,
            Key::Named(NamedKey::PageDown) => (index + page_rows).min(last),
            Key::Named(NamedKey::PageUp) => index.saturating_sub(page_rows),
            Key::Named(NamedKey::ArrowLeft) => {
                if !doc.nodes[id.0].children.is_empty() && branch_open(&doc, id, &self.toggled) {
                    self.toggle(id);
                    return true;
                }
                self.rows
                    .iter()
                    .position(|r| Some(r.id) == doc.nodes[id.0].parent)
                    .unwrap_or(index)
            }
            Key::Named(NamedKey::ArrowRight) => {
                if !doc.nodes[id.0].children.is_empty() && !branch_open(&doc, id, &self.toggled) {
                    self.toggle(id);
                    return true;
                }
                if self
                    .rows
                    .get(index + 1)
                    .is_some_and(|r| doc.nodes[r.id.0].parent == Some(id))
                {
                    index + 1
                } else {
                    index
                }
            }
            _ => return false,
        };
        self.select(next, bounds);
        true
    }

    pub(super) fn draw(&self, canvas: &mut Canvas, fonts: &mut Fonts, bounds: Rect, scale: f32) {
        let p = Panes::new(bounds, self.narrow_details);
        let mut ink = Ink {
            canvas,
            fonts,
            scale,
        };
        ink.fill(bounds, BG);
        ink.fill(
            rect(bounds.x, bounds.y, bounds.width, BAR.min(bounds.height)),
            CHROME,
        );
        ink.fill(p.search, if self.searching { ACTIVE } else { BG });
        let search = if self.query.is_empty() {
            if self.searching {
                "Type a tag, #id, .class or text".into()
            } else {
                "Find in DOM   Ctrl+F".into()
            }
        } else {
            format!("Find   {}", self.query)
        };
        ink.text(
            p.search,
            7.,
            &search,
            if self.searching { INK } else { DIM },
            false,
        );
        for (i, (&button, name)) in p
            .buttons
            .iter()
            .zip(["Tree", "Node", "Rules", "Computed"])
            .enumerate()
        {
            let chosen = match i {
                0 => !self.narrow_details,
                1 => self.detail == Detail::Node,
                2 => self.detail == Detail::Rules,
                _ => self.detail == Detail::Computed,
            };
            let active = chosen && (i == 0 || p.details.width > 0.);
            if active {
                ink.fill(button, ACTIVE);
            }
            ink.text(button, 5., name, if active { ACCENT } else { DIM }, active);
        }

        if panel::has_area(p.tree) {
            let top = offset(self.tree_offset, self.rows.len(), p.tree);
            if let Some(doc) = self.dom.upgrade() {
                let first = (top / ROW).floor() as usize;
                for (i, row) in self
                    .rows
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take((p.tree.height / ROW).ceil() as usize + 1)
                {
                    let box_y = p.tree.y + i as f32 * ROW - top;
                    let band = row_clip(p.tree, box_y);
                    if !panel::has_area(band) {
                        continue;
                    }
                    if self.selected == Some(row.id) {
                        ink.fill(band, ACTIVE)
                    }
                    let depth = if self.query.is_empty() {
                        indent(row.depth, p.tree)
                    } else {
                        0.
                    };
                    let has_children =
                        !doc.nodes[row.id.0].children.is_empty() && self.query.is_empty();
                    let marker = if !has_children {
                        ""
                    } else if branch_open(&doc, row.id, &self.toggled) {
                        "▾"
                    } else {
                        "▸"
                    };
                    ink.at(band, p.tree.x + 5. + depth, box_y + 15., marker, DIM, false);
                    let color = if matches!(doc.nodes[row.id.0].kind, NodeKind::Element { .. }) {
                        TAG
                    } else {
                        DIM
                    };
                    ink.at(
                        band,
                        p.tree.x + 20. + depth,
                        box_y + 15.,
                        &caption(&doc, row.id),
                        color,
                        false,
                    );
                }
            }
            if self.rows.is_empty() {
                ink.text(
                    rect(p.tree.x, p.tree.y, p.tree.width, ROW.min(p.tree.height)),
                    8.,
                    if self.query.is_empty() {
                        "No document"
                    } else {
                        "No matching nodes"
                    },
                    DIM,
                    false,
                );
            }
        }
        if let Some(editor) = &self.editor {
            editor.draw(&mut ink, p.details);
        } else if panel::has_area(p.details) {
            let lines = self.detail_lines();
            if lines.is_empty() {
                ink.text(
                    rect(
                        p.details.x,
                        p.details.y,
                        p.details.width,
                        ROW.min(p.details.height),
                    ),
                    8.,
                    "Select a rendered element to see computed CSS",
                    DIM,
                    false,
                );
            }
            let columns = wrap_columns(p.details);
            let count = wrapped_count(lines, p.details);
            let top = offset(self.detail_offset, count, p.details);
            let mut y = p.details.y - top;
            for line in lines {
                let chunks = wrapped_height(&line.text, columns);
                if y + ROW * chunks as f32 <= p.details.y {
                    y += ROW * chunks as f32;
                    continue;
                }
                let mut chars = line.text.chars();
                for _ in 0..chunks {
                    let value: String = chars.by_ref().take(columns).collect();
                    if y + ROW > p.details.y {
                        let band = row_clip(p.details, y);
                        if line.heading {
                            ink.fill(band, CHROME);
                        }
                        ink.at(
                            band,
                            p.details.x + 8.,
                            y + 15.,
                            &value,
                            if line.heading { ACCENT } else { INK },
                            line.heading,
                        );
                    }
                    y += ROW;
                    if y >= p.details.y + p.details.height {
                        break;
                    }
                }
                if y >= p.details.y + p.details.height {
                    break;
                }
            }
        }
        if panel::has_area(p.footer) {
            ink.fill(p.footer, CHROME);
            let summary = if self.query.is_empty() {
                format!(
                    "{} visible nodes   ·   Arrows navigate   ·   Tab switches details",
                    self.rows.len()
                )
            } else {
                format!(
                    "{} matches{}   ·   Enter reveals node",
                    self.rows.len(),
                    if self.limited {
                        " in the first 20,000 nodes"
                    } else {
                        ""
                    }
                )
            };
            let button = Self::edit_button(p.footer);
            let can_edit = self.editable() && self.editor.is_none();
            let message = if self.edit_status.is_empty() {
                &summary
            } else {
                &self.edit_status
            };
            let text_area = if can_edit {
                rect(
                    p.footer.x,
                    p.footer.y,
                    p.footer.width - button.width,
                    p.footer.height,
                )
            } else {
                p.footer
            };
            ink.text(text_area, 7., message, DIM, false);
            if can_edit {
                ink.fill(button, ACTIVE);
                ink.text(button, 5., "Edit CSS   F2", ACCENT, true);
            }
        }
        ink.canvas.clip(panel::scaled(bounds, scale));
    }
}

fn indent(depth: usize, pane: Rect) -> f32 {
    (depth as f32 * INDENT).min(pane.width * 0.4)
}

fn row_clip(pane: Rect, y: f32) -> Rect {
    let top = y.max(pane.y);
    rect(
        pane.x,
        top,
        pane.width,
        (y + ROW).min(pane.y + pane.height) - top,
    )
}

fn wrap_columns(pane: Rect) -> usize {
    ((pane.width - 16.) / 8.).floor().max(1.) as usize
}
fn wrapped_height(line: &str, cols: usize) -> usize {
    line.chars().count().max(1).div_ceil(cols)
}
fn wrapped_count(lines: &[Line], pane: Rect) -> usize {
    let cols = wrap_columns(pane);
    lines.iter().map(|l| wrapped_height(&l.text, cols)).sum()
}

struct Ink<'a> {
    canvas: &'a mut Canvas,
    fonts: &'a mut Fonts,
    scale: f32,
}
impl Ink<'_> {
    fn fill(&mut self, r: Rect, color: Color) {
        if panel::has_area(r) {
            let physical = panel::scaled(r, self.scale);
            self.canvas.clip(physical);
            self.canvas.fill_rect(physical, color);
        }
    }
    fn text(&mut self, r: Rect, pad: f32, value: &str, color: Color, bold: bool) {
        self.at(r, r.x + pad, r.y + r.height / 2. + 4., value, color, bold);
    }
    fn at(&mut self, r: Rect, x: f32, y: f32, value: &str, color: Color, bold: bool) {
        if panel::has_area(r) {
            self.canvas.clip(panel::scaled(r, self.scale));
            self.canvas.label(
                value,
                x * self.scale,
                y * self.scale,
                12. * self.scale,
                color,
                bold,
                self.fonts,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_browser_engine::{LoadedPage, html};

    fn area() -> Rect {
        rect(20., 140., 720., 260.)
    }

    #[test]
    fn picker_opens_ancestors_and_search_reveals_the_actual_node() {
        let doc = Arc::new(html::parse(
            "<main><section><article id='result' class='hero'>Content</article></section></main>",
        ));
        let article = doc.elements("article").next().unwrap();
        let main = doc.elements("main").next().unwrap();
        let mut view = Elements::default();
        view.sync(Some(&doc), None);
        assert!(!view.rows.iter().any(|r| r.id == article));
        view.inspect(article, area());
        assert_eq!(view.selected, Some(article));
        assert!(view.rows.iter().any(|r| r.id == article));
        view.toggle(main);
        assert!(!view.rows.iter().any(|r| r.id == article));
        assert!(view.key(area(), &Key::Character("f".into()), ModifiersState::CONTROL));
        view.key(
            area(),
            &Key::Character(".hero".into()),
            ModifiersState::empty(),
        );
        assert_eq!(
            view.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            [article]
        );
        view.key(
            area(),
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert!(view.query.is_empty());
        assert_eq!(view.selected, Some(article));
        assert!(view.rows.iter().any(|r| r.id == article));
        assert!(
            view.node_lines
                .iter()
                .any(|l| l.text.contains("id = \"result\""))
        );
    }

    #[test]
    fn inline_editor_requires_an_explicit_commit_and_keeps_rejected_input() {
        let mut view = Elements::default();
        let doc = Arc::new(html::parse("<div style='color: red !important'>x</div>"));
        let node = doc.elements("div").next().unwrap();
        view.sync(Some(&doc), None);
        view.inspect(node, area());
        view.open_editor("color", "rgb(255, 0, 0)");
        assert!(view.take_style_edit().is_none());
        view.key(
            area(),
            &Key::Character("blue".into()),
            ModifiersState::empty(),
        );
        view.key(
            area(),
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert_eq!(
            view.take_style_edit(),
            Some((node, "color".into(), "blue".into(), true))
        );
        view.style_edit_result(Err("Unsupported".into()));
        assert!(view.editor.is_some());
        view.editor_action(editor::Action::Remove);
        assert_eq!(
            view.take_style_edit(),
            Some((node, "color".into(), "".into(), true))
        );
        view.style_edit_result(Ok(()));
        assert!(view.editor.is_none());
        view.key(area(), &Key::Named(NamedKey::F2), ModifiersState::empty());
        view.key(
            area(),
            &Key::Named(NamedKey::Escape),
            ModifiersState::empty(),
        );
        assert!(view.take_style_edit().is_none());
    }

    #[test]
    fn detached_selection_disappears_and_inspection_cannot_keep_an_old_document_alive() {
        let mut doc = Arc::new(html::parse("<body><b>hi</b></body>"));
        let node = doc.elements("b").next().unwrap();
        let parent = doc.nodes[node.0].parent.unwrap();
        let mut view = Elements::default();
        view.sync(Some(&doc), None);
        view.inspect(node, area());
        let next = Arc::make_mut(&mut doc);
        next.nodes[parent.0].children.retain(|id| *id != node);
        next.nodes[node.0].parent = None;
        view.sync(Some(&doc), None);
        assert_ne!(view.selected, Some(node));
        view.inspect(node, area());
        assert_ne!(view.selected, Some(node));
        drop(doc);
        assert!(view.dom.upgrade().is_none());
        view.sync(None, None);
        assert!(view.rows.is_empty());
        assert!(view.node_lines.is_empty());
    }

    #[test]
    fn inspector_uses_real_visual_bounds_and_cssom_resolution() {
        let page = LoadedPage::from_html(
            "https://devtools.test/",
            "<style>#sample { display: block; width: 83px; height: 41px; padding: 2px; border: 1px solid red; color: #112233; }</style><div id='sample'></div>",
        );
        let id = page.document.elements("div").next().unwrap();
        let fonts = Fonts::new().unwrap();
        let layout = Arc::new(Layout::build(&page, &fonts, 800.));
        let mut view = Elements::default();
        view.sync(Some(&page.document), Some(&layout));
        view.inspect(id, area());
        assert_eq!(view.selected_rect().unwrap().width, 89.);
        for expected in [
            "width: 83px",
            "height: 41px",
            "color: rgb(17, 34, 51)",
            "padding-left: 2px",
        ] {
            assert!(
                view.css_lines.iter().any(|l| l.text == expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn rules_page_shows_inline_and_matching_author_declarations() {
        let page = LoadedPage::from_html(
            "https://devtools.test/",
            "<style>section p { color: red !important } .other { float: left }</style><section><p style='padding: 4px'>sample</p></section>",
        );
        let id = page.document.elements("p").next().unwrap();
        let layout = Arc::new(Layout::build(&page, &Fonts::new().unwrap(), 640.));
        let mut view = Elements::default();
        view.sync_page(Some(&page), Some(&layout));
        view.inspect(id, area());
        let button = Panes::new(area(), false).buttons[2];
        view.click(area(), button.x + 3., button.y + 3.);
        view.sync_page(Some(&page), Some(&layout));
        let text = view
            .detail_lines()
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "element.style",
            "padding: 4px;",
            "section p",
            "Stylesheet 1",
            "color: red !important;",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!text.contains("float"));
    }

    #[test]
    fn compact_mode_and_keyboard_keep_tree_and_details_reachable() {
        let narrow = rect(31., 124., 340., 140.);
        let mut view = Elements::default();
        let doc = Arc::new(html::parse("<main><p>hello</p></main>"));
        view.sync(Some(&doc), None);
        let main = doc.elements("main").next().unwrap();
        view.inspect(main, narrow);
        view.key(
            narrow,
            &Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        );
        view.key(
            narrow,
            &Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        );
        assert_eq!(view.selected, doc.elements("p").next());
        let p = Panes::new(narrow, false);
        assert!(panel::has_area(p.tree) && !panel::has_area(p.details));
        view.click(narrow, p.buttons[1].x + 2., p.buttons[1].y + 2.);
        let p = Panes::new(narrow, view.narrow_details);
        assert!(!panel::has_area(p.tree) && panel::has_area(p.details));
        view.key(narrow, &Key::Named(NamedKey::Tab), ModifiersState::empty());
        assert!(view.detail == Detail::Rules);
        view.key(narrow, &Key::Named(NamedKey::Tab), ModifiersState::empty());
        assert!(view.detail == Detail::Computed);
        view.key(
            narrow,
            &Key::Named(NamedKey::Escape),
            ModifiersState::empty(),
        );
        assert!(!view.narrow_details);
    }
}
