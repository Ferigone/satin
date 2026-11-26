//! Deterministic hidden-window input and observations, never OS input.
use rust_browser_engine::{
    Layout,
    dom::{Document, NodeId},
    layout::Viewport,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
};

const MAX_ACTION_BYTES: usize = 1024 * 1024;
const MAX_CHECKPOINT_HTML: usize = 4 * 1024 * 1024;
const MAX_CONTEXT_ENTRIES: usize = 32_768;
const MAX_CONTEXT_CLASS_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SmokeAction {
    Click {
        id: String,
    },
    ClickInRow(RowControl),
    ClickAt {
        x: f32,
        y: f32,
    },
    Text {
        value: String,
    },
    Key {
        value: String,
        #[serde(default)]
        control: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        alt: bool,
    },
    Title {
        value: String,
    },
    WaitTitle {
        value: String,
        timeout_ms: u64,
    },
    SampleMedia { after_ms: u64 },
    Checkpoint(Checkpoint),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowControl {
    within_id: String,
    label_class: String,
    label_prefix: String,
    control_alt: String,
    pub timeout_ms: u64,
}

impl RowControl {
    fn validate(&self) -> Result<(), String> {
        if [
            &self.within_id,
            &self.label_class,
            &self.label_prefix,
            &self.control_alt,
        ]
        .into_iter()
        .any(|s| s.is_empty() || s.len() > 256)
            || !(1..=1_800_000).contains(&self.timeout_ms)
        {
            return Err("Invalid smoke row target".into());
        }
        Ok(())
    }

    pub fn find(&self, document: &Document) -> Result<Option<NodeId>, String> {
        let mut roots = document
            .connected_nodes()
            .filter(|&n| document.attr(n, "id") == Some(&self.within_id));
        let Some(root) = roots.next() else {
            return Ok(None);
        };
        if roots.next().is_some() {
            return Err("Ambiguous smoke row container".into());
        }
        let ancestor = |id: NodeId, test: &dyn Fn(NodeId) -> bool| {
            std::iter::successors(Some(id), |n| document.nodes[n.0].parent).find(|n| test(*n))
        };
        let mut rows = Vec::new();
        for node in document.connected_nodes() {
            if document
                .attr(node, "class")
                .is_some_and(|c| c.split_ascii_whitespace().any(|t| t == self.label_class))
                && ancestor(node, &|n| n == root).is_some()
                && document.text(node).trim().starts_with(&self.label_prefix)
                && let Some(row) = ancestor(node, &|n| document.tag(n) == "tr")
                && ancestor(row, &|n| n == root).is_some()
            {
                rows.push(row);
            }
        }
        let row = match rows.as_slice() {
            [] => return Ok(None),
            [row] => *row,
            _ => return Err("Ambiguous smoke row label".into()),
        };
        let mut controls = document.html_elements("img").filter(|&n| {
            document.attr(n, "alt") == Some(&self.control_alt)
                && ancestor(n, &|n| document.tag(n) == "tr") == Some(row)
        });
        let control = controls.next();
        if controls.next().is_some() {
            return Err("Ambiguous control in smoke row".into());
        }
        Ok(control)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    name: String,
    within_id: String,
    #[serde(default)]
    descendant_class: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    positive: bool,
    pub timeout_ms: u64,
    #[serde(default)]
    diagnose_timeout: bool,
    #[serde(default)]
    timeout_context: Option<TimeoutContext>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeoutContext {
    pane_class: String,
    control_id: String,
    selected_class: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointObservation {
    DocumentNotReady,
    MissingRoot,
    MissingDescendant,
    TextMismatch,
    Hidden,
    NoLayoutBox,
    NotHittable,
    Match,
}

impl CheckpointObservation {
    pub fn is_match(self) -> bool {
        self == Self::Match
    }

    fn label(self) -> &'static str {
        match self {
            Self::DocumentNotReady => "document-not-ready",
            Self::MissingRoot => "missing-root",
            Self::MissingDescendant => "missing-descendant",
            Self::TextMismatch => "text-mismatch",
            Self::Hidden => "exact-text-hidden",
            Self::NoLayoutBox => "exact-text-no-layout-box",
            Self::NotHittable => "exact-text-not-hittable",
            Self::Match => "exact-text-hit-too-late",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CheckpointSample {
    observation: CheckpointObservation,
    context: Option<TimeoutContextSample>,
}

impl From<CheckpointObservation> for CheckpointSample {
    fn from(observation: CheckpointObservation) -> Self {
        Self {
            observation,
            context: None,
        }
    }
}

impl CheckpointSample {
    pub fn is_match(self) -> bool {
        self.observation.is_match()
    }
}

#[derive(Clone, Copy, Debug)]
struct TimeoutContextSample {
    pane_rect: &'static str,
    run_class: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextNode {
    Missing,
    Unique(NodeId),
    Ambiguous,
}

impl ContextNode {
    fn record(&mut self, node: NodeId) {
        *self = match *self {
            Self::Missing => Self::Unique(node),
            _ => Self::Ambiguous,
        };
    }

    fn label(self, inspect: impl FnOnce(NodeId) -> &'static str) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Ambiguous => "ambiguous",
            Self::Unique(node) => inspect(node),
        }
    }
}

impl TimeoutContext {
    fn sample(&self, document: &Document, layout: &Layout) -> TimeoutContextSample {
        // Bound the connected walk (including its pending stack). An unchecked
        // pane class could hide a match or a duplicate, so it is not absence.
        if document.nodes.len() > MAX_CONTEXT_ENTRIES {
            return TimeoutContextSample {
                pane_rect: "unavailable",
                run_class: "unavailable",
            };
        }
        let mut pane = ContextNode::Missing;
        let mut control = ContextNode::Missing;
        let mut class_bytes = Some(MAX_CONTEXT_CLASS_BYTES);
        for node in document.connected_nodes() {
            if pane != ContextNode::Ambiguous
                && let Some(remaining) = class_bytes
                && let Some(value) = document.attr(node, "class")
            {
                class_bytes = remaining.checked_sub(value.len());
                if class_bytes.is_some()
                    && value
                        .split_ascii_whitespace()
                        .any(|token| token == self.pane_class.as_str())
                {
                    pane.record(node);
                }
            }
            if control != ContextNode::Ambiguous
                && document.attr(node, "id") == Some(self.control_id.as_str())
            {
                control.record(node);
            }
        }
        let pane_rect = if class_bytes.is_none() {
            "unavailable"
        } else {
            pane.label(|node| {
                if layout.elements.len() > MAX_CONTEXT_ENTRIES {
                    "unavailable"
                } else if layout.elements.iter().any(|entry| entry.node == node) {
                    "present"
                } else {
                    "none"
                }
            })
        };
        let run_class = control.label(|node| {
            let value = document.attr(node, "class").unwrap_or_default();
            if value.len() > MAX_CONTEXT_CLASS_BYTES {
                "unavailable"
            } else if value
                .split_ascii_whitespace()
                .any(|token| token == self.selected_class.as_str())
            {
                "match"
            } else {
                "no-match"
            }
        });
        TimeoutContextSample {
            pane_rect,
            run_class,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointDocument {
    pub tab_id: u64,
    pub document_navigation: u64,
}

/// Own only paths created by this attempt. A failed smoke run has no success
/// envelope, even if ordinary unwinding cannot clean up a sidecar.
#[derive(Default)]
pub struct CheckpointOutputs {
    run_id: Option<String>,
    files: Vec<(String, PathBuf)>,
}

impl CheckpointOutputs {
    fn record(&mut self, name: &str, path: PathBuf) -> &str {
        self.files.push((name.into(), path));
        self.run_id.get_or_insert_with(|| {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{}-{stamp}", std::process::id())
        })
    }

    pub fn summary(&self) -> Option<serde_json::Value> {
        let run_id = self.run_id.as_deref()?;
        Some(serde_json::json!({
            "runId": run_id,
            "names": self.files.iter().map(|(name, _)| name).collect::<Vec<_>>()
        }))
    }

    pub fn commit(&mut self) {
        self.files.clear();
    }
}

impl Drop for CheckpointOutputs {
    fn drop(&mut self) {
        for (_, path) in self.files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Checkpoint {
    fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || self.name.len() > 32
            || !self
                .name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
            || std::iter::once(&self.within_id)
                .chain(self.descendant_class.as_ref())
                .any(|value| {
                    value.is_empty() || value.len() > 256 || value.chars().any(char::is_whitespace)
                })
            || self.positive == self.text.is_some()
            || self
                .text
                .as_ref()
                .is_some_and(|text| text.is_empty() || text.len() > 1024 || text.trim() != text)
            || !(1..=21_600_000).contains(&self.timeout_ms)
        {
            return Err("Invalid smoke checkpoint parameters".into());
        }
        if let Some(context) = &self.timeout_context
            && (!self.diagnose_timeout
                || [
                    &context.pane_class,
                    &context.control_id,
                    &context.selected_class,
                ]
                .into_iter()
                .any(|value| {
                    value.is_empty() || value.len() > 256 || value.chars().any(char::is_whitespace)
                }))
        {
            return Err("Invalid smoke checkpoint timeout context".into());
        }
        Ok(())
    }

    pub fn error(&self, message: &str) -> String {
        format!("Smoke checkpoint {:?}: {message}", self.name)
    }

    pub fn timeout_error(&self, last: Option<CheckpointSample>) -> String {
        let message = "timed out waiting for a layout hit with exact text";
        if self.diagnose_timeout {
            let reason = last.map_or("no-post-click-sample", |sample| sample.observation.label());
            if let Some(context) = last.and_then(|sample| sample.context) {
                return self.error(&format!(
                    "{message}; last_observation={reason}; pane_rect={}; run_class={}",
                    context.pane_rect, context.run_class
                ));
            }
            self.error(&format!("{message}; last_observation={reason}"))
        } else {
            self.error(message)
        }
    }

    pub fn require_document(
        &self,
        expected: CheckpointDocument,
        actual: CheckpointDocument,
    ) -> Result<(), String> {
        if expected != actual {
            return Err(self.error("active tab or document changed"));
        }
        Ok(())
    }

    pub fn require_unmet(&self, matched: bool) -> Result<(), String> {
        if matched {
            return Err(self.error("condition already matched before input"));
        }
        Ok(())
    }

    pub fn observe(
        &self,
        document: &Document,
        layout: &Layout,
        viewport: Viewport,
    ) -> Result<CheckpointObservation, String> {
        let mut roots = document
            .connected_nodes()
            .filter(|&node| document.attr(node, "id") == Some(self.within_id.as_str()));
        let Some(root) = roots.next() else {
            return Ok(CheckpointObservation::MissingRoot);
        };
        if roots.next().is_some() {
            return Err(self.error("ambiguous root id"));
        }

        let text_matches = |node| {
            let contents = document.text(node);
            let value = contents.trim();
            if self.positive {
                value.bytes().all(|c| c.is_ascii_digit())
                    && value.parse::<u64>().is_ok_and(|n| n > 0)
            } else {
                self.text.as_deref() == Some(value)
            }
        };
        let Some(class) = &self.descendant_class else {
            return Ok(if text_matches(root) {
                layout_observation(document, layout, viewport, root)
            } else {
                CheckpointObservation::TextMismatch
            });
        };
        let mut pending = document.nodes[root.0].children.clone();
        let mut matched = None;
        let mut found_descendant = false;
        while let Some(node) = pending.pop() {
            pending.extend(document.nodes[node.0].children.iter().copied());
            if document.attr(node, "class").is_some_and(|value| {
                value
                    .split_ascii_whitespace()
                    .any(|token| token == class.as_str())
            }) {
                found_descendant = true;
                if text_matches(node) && matched.replace(node).is_some() {
                    return Err(self.error("ambiguous matching descendants"));
                }
            }
        }
        Ok(match matched {
            Some(node) => layout_observation(document, layout, viewport, node),
            None if found_descendant => CheckpointObservation::TextMismatch,
            None => CheckpointObservation::MissingDescendant,
        })
    }

    pub fn sample_after_click(
        &self,
        document: &Document,
        layout: &Layout,
        viewport: Viewport,
    ) -> Result<CheckpointSample, String> {
        let observation = self.observe(document, layout, viewport)?;
        let mut sample = CheckpointSample::from(observation);
        if observation == CheckpointObservation::NoLayoutBox
            && self.diagnose_timeout
            && let Some(context) = &self.timeout_context
        {
            sample.context = Some(context.sample(document, layout));
        }
        Ok(sample)
    }

    fn output_path(&self, frame: &Path) -> PathBuf {
        let mut path = frame.as_os_str().to_os_string();
        path.push(format!(".{}.json", self.name));
        path.into()
    }

    pub fn capture(
        &self,
        frame: &Path,
        outputs: &mut CheckpointOutputs,
        identity: CheckpointDocument,
        url: &str,
        title: &str,
        document: &Document,
    ) -> Result<(), String> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Receipt<'a> {
            #[serde(flatten)]
            identity: CheckpointDocument,
            run_id: &'a str,
            url: &'a str,
            title: &'a str,
            html: &'a str,
        }
        let html = document.serialize();
        if html.len() > MAX_CHECKPOINT_HTML {
            return Err(self.error("HTML exceeds 4 MiB"));
        }
        let path = self.output_path(frame);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| self.error(&format!("could not create new output: {error}")))?;
        let run_id = outputs.record(&self.name, path);
        serde_json::to_writer(
            file,
            &Receipt {
                identity,
                run_id,
                url,
                title,
                html: &html,
            },
        )
        .map_err(|error| self.error(&format!("could not write output: {error}")))
    }
}

// A box alone may be clipped, offscreen or covered. Use the native content
// hit test on the part inside the viewport; this is not a pixel oracle.
fn layout_observation(
    document: &Document,
    layout: &Layout,
    viewport: Viewport,
    node: NodeId,
) -> CheckpointObservation {
    let mut ancestor = Some(node);
    while let Some(id) = ancestor {
        if document.attr(id, "hidden").is_some() {
            return CheckpointObservation::Hidden;
        }
        ancestor = document.nodes[id.0].parent;
    }
    let rects = layout.rects_for(node);
    if rects.is_empty() {
        return CheckpointObservation::NoLayoutBox;
    }
    let hittable = rects.into_iter().any(|rect| {
        let left = rect.x.max(viewport.scroll_x);
        let top = rect.y.max(viewport.scroll_y);
        let right = (rect.x + rect.width).min(viewport.scroll_x + viewport.width);
        let bottom = (rect.y + rect.height).min(viewport.scroll_y + viewport.height);
        if right <= left || bottom <= top {
            return false;
        }
        let mut hit = layout.element_at((left + right) * 0.5, (top + bottom) * 0.5);
        while let Some(id) = hit {
            if id == node {
                return true;
            }
            hit = document.nodes.get(id.0).and_then(|entry| entry.parent);
        }
        false
    });
    if hittable {
        CheckpointObservation::Match
    } else {
        CheckpointObservation::NotHittable
    }
}

pub fn read_actions(path: &Path) -> Result<VecDeque<SmokeAction>, String> {
    if std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_ACTION_BYTES as u64
    {
        return Err("Smoke actions exceed 1 MiB".into());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    parse_actions(&bytes)
}

fn parse_actions(bytes: &[u8]) -> Result<VecDeque<SmokeAction>, String> {
    if bytes.len() > MAX_ACTION_BYTES {
        return Err("Smoke actions exceed 1 MiB".into());
    }
    let actions: VecDeque<SmokeAction> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if actions.len() > 256 {
        return Err("Smoke actions exceed 256 steps".into());
    }
    let mut follows_click = false;
    let mut names = HashSet::new();
    for action in &actions {
        if matches!(action, SmokeAction::SampleMedia { after_ms } if *after_ms > 60_000) {
            return Err("Media sample delay exceeds 60 seconds".into());
        }
        if let SmokeAction::ClickInRow(row) = action {
            row.validate()?;
        }
        if let SmokeAction::Checkpoint(checkpoint) = action {
            checkpoint.validate()?;
            if !follows_click {
                return Err(checkpoint.error("must immediately follow click or click_at"));
            }
            if names.len() == 8 || !names.insert(checkpoint.name.as_str()) {
                return Err(checkpoint.error("checkpoint names must be unique (maximum eight)"));
            }
        }
        follows_click = matches!(
            action,
            SmokeAction::Click { .. } | SmokeAction::ClickAt { .. } | SmokeAction::ClickInRow(_)
        );
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_browser_engine::{Fonts, LoadedPage};

    #[test]
    fn row_control_selects_its_own_row_and_rejects_ambiguity() {
        let row = |label: &str| {
            format!(
                "<tr><td class='label'>{label}</td><td><img alt='Select'><img alt='Run'></td></tr>"
            )
        };
        let page = |rows: String| {
            LoadedPage::from_html(
                "https://example.test/",
                &format!("<table id='outer'>{rows}</table>"),
            )
        };
        let target: RowControl = serde_json::from_str(r#"{"within_id":"outer","label_class":"label","label_prefix":"Chapter - language (","control_alt":"Run","timeout_ms":30000}"#).unwrap();
        let original = page(row("Chapter - language-extra (4)") + &row("Chapter - language (8)"));
        let button = target.find(&original.document).unwrap().unwrap();
        assert_eq!(original.document.html_elements("img").last(), Some(button));
        assert!(
            target
                .find(&page(row("Loading")).document)
                .unwrap()
                .is_none()
        );
        assert!(
            target
                .find(
                    &page(row("Chapter - language (8)") + &row("Chapter - language (9)")).document
                )
                .is_err()
        );
    }

    #[test]
    fn positive_root_checkpoint_requires_a_visible_positive_integer() {
        let probe: Checkpoint = serde_json::from_str(
            r#"{"name":"sample","within_id":"counter","positive":true,"timeout_ms":90000}"#,
        )
        .unwrap();
        probe.validate().unwrap();
        for (value, matched) in [
            ("0", false),
            ("-1", false),
            ("19 tests", false),
            ("19", true),
        ] {
            let page = LoadedPage::from_html(
                "https://example.test/",
                &format!("<span id='counter'>{value}</span>"),
            );
            let fonts = Fonts::new().unwrap();
            let viewport = Viewport::default();
            let layout = Layout::build_with_viewport(&page, &fonts, viewport);
            assert_eq!(
                probe
                    .observe(&page.document, &layout, viewport)
                    .unwrap()
                    .is_match(),
                matched
            );
        }
    }

    fn checkpoint() -> Checkpoint {
        Checkpoint {
            name: "results".into(),
            within_id: "report".into(),
            descendant_class: Some("ran".into()),
            text: Some("2".into()),
            positive: false,
            timeout_ms: 100,
            diagnose_timeout: false,
            timeout_context: None,
        }
    }

    fn observe(html: &str) -> Result<CheckpointObservation, String> {
        let page = LoadedPage::from_html("https://example.test/", html);
        let fonts = Fonts::new().unwrap();
        let viewport = Viewport::default();
        let layout = Layout::build_with_viewport(&page, &fonts, viewport);
        checkpoint().observe(&page.document, &layout, viewport)
    }

    #[test]
    fn checkpoint_uses_exact_scoped_layout_text() {
        use CheckpointObservation::*;
        assert!(
            observe(r#"<div id="report"><span class="other ran"> 2 </span></div>"#)
                .unwrap()
                .is_match()
        );
        for (html, expected) in [
            (
                r#"<div id="wrong"><span class="ran">2</span></div>"#,
                MissingRoot,
            ),
            (
                r#"<div id="report"><span class="ran-extra">2</span></div>"#,
                MissingDescendant,
            ),
            (
                r#"<div id="report"><span class="ran">12</span></div>"#,
                TextMismatch,
            ),
            (
                r#"<div id="report" style="display:none"><span class="ran">2</span></div>"#,
                NoLayoutBox,
            ),
            (
                r#"<div id="report" hidden><span class="ran">2</span></div>"#,
                Hidden,
            ),
        ] {
            let actual = observe(html).unwrap();
            assert_eq!(actual, expected, "{html}");
            assert!(!actual.is_match());
        }
        // Move the sampled viewport past a known laid-out element to check
        // clipping separately from the absence of any layout box.
        let page = LoadedPage::from_html(
            "https://example.test/",
            r#"<div id="report"><span class="ran">2</span></div>"#,
        );
        let fonts = Fonts::new().unwrap();
        let viewport = Viewport::default();
        let layout = Layout::build_with_viewport(&page, &fonts, viewport);
        let outside = Viewport {
            scroll_y: 2000.,
            ..viewport
        };
        assert_eq!(
            checkpoint()
                .observe(&page.document, &layout, outside)
                .unwrap(),
            NotHittable
        );
    }

    #[test]
    fn checkpoint_timeout_reason_is_opt_in_and_contains_no_page_text() {
        let default = checkpoint();
        assert_eq!(
            default.timeout_error(Some(CheckpointObservation::Hidden.into())),
            r#"Smoke checkpoint "results": timed out waiting for a layout hit with exact text"#
        );
        let SmokeAction::Checkpoint(opted_in) = serde_json::from_str(
            r#"{"action":"checkpoint","name":"report","within_id":"sensitive-root","descendant_class":"sensitive-class","text":"private expected text","timeout_ms":100,"diagnose_timeout":true}"#,
        ).unwrap() else { panic!("expected checkpoint"); };
        for (sample, reason) in [
            (None, "no-post-click-sample"),
            (
                Some(CheckpointObservation::DocumentNotReady),
                "document-not-ready",
            ),
            (
                Some(CheckpointObservation::NotHittable),
                "exact-text-not-hittable",
            ),
            (
                Some(CheckpointObservation::Match),
                "exact-text-hit-too-late",
            ),
        ] {
            assert_eq!(
                opted_in.timeout_error(sample.map(Into::into)),
                format!(
                    r#"Smoke checkpoint "report": timed out waiting for a layout hit with exact text; last_observation={reason}"#
                )
            );
        }
    }

    const TIMEOUT_CONTEXT: &str =
        r#"{"pane_class":"content-tests","control_id":"run","selected_class":"selected"}"#;
    const UNBOXED_ROW: &str =
        r#"<div id="report"><span class="ran" style="display:none">2</span></div>"#;

    fn parse_timeout_context(enabled: bool, context: &str) -> Result<Checkpoint, String> {
        let mut actions = parse_actions(
            format!(
                r#"[{{"action":"click","id":"run"}},{{"action":"checkpoint","name":"results","within_id":"report","descendant_class":"ran","text":"2","timeout_ms":100,"diagnose_timeout":{enabled},"timeout_context":{context}}}]"#
            )
            .as_bytes(),
        )?;
        let Some(SmokeAction::Checkpoint(checkpoint)) = actions.pop_back() else {
            panic!("expected checkpoint");
        };
        Ok(checkpoint)
    }

    fn sample_fixture(checkpoint: &Checkpoint, html: &str) -> Result<CheckpointSample, String> {
        let page = LoadedPage::from_html("about:blank", html);
        let fonts = Fonts::new().unwrap();
        let viewport = Viewport::default();
        let layout = Layout::build_with_viewport(&page, &fonts, viewport);
        checkpoint.sample_after_click(&page.document, &layout, viewport)
    }

    #[test]
    fn checkpoint_timeout_context_distinguishes_pane_and_row() {
        let checkpoint = parse_timeout_context(true, TIMEOUT_CONTEXT).unwrap();
        for (pane_style, selection, row_style, pane, run) in [
            ("display:none", "unselected", "", "none", "no-match"),
            ("", "selected", "display:none", "present", "match"),
            ("display:none", "selected", "", "none", "match"),
        ] {
            let sample = sample_fixture(
                &checkpoint,
                &format!(
                    r#"<a id="run" class="nav-link {selection}">Run</a>
                    <div class="content-tests" style="{pane_style}"><p>Sibling</p>
                    <div id="report"><span class="ran" style="{row_style}">2</span></div></div>"#
                ),
            )
            .unwrap();
            assert!(!sample.is_match());
            assert_eq!(
                checkpoint.timeout_error(Some(sample)),
                format!(
                    r#"Smoke checkpoint "results": timed out waiting for a layout hit with exact text; last_observation=exact-text-no-layout-box; pane_rect={pane}; run_class={run}"#
                )
            );
        }
    }

    #[test]
    fn checkpoint_timeout_context_refuses_to_guess_missing_or_ambiguous_targets() {
        let checkpoint = parse_timeout_context(true, TIMEOUT_CONTEXT).unwrap();
        let pane = r#"<div class="content-tests">Pane</div>"#;
        let run = r#"<a id="run" class="nav-link selected">Run</a>"#;
        let huge_class = "x".repeat(MAX_CONTEXT_CLASS_BYTES + 1);
        let oversized_pane = format!(r#"<div class="{huge_class} content-tests">Pane</div>"#);
        let oversized_run = format!(r#"<a id="run" class="{huge_class} selected">Run</a>"#);
        for (extra, pane, run) in [
            (format!("{pane}{pane}{run}"), "ambiguous", "match"),
            (format!("{pane}{run}{run}"), "present", "ambiguous"),
            (
                format!(r#"<div class="content-tests-extra">Other</div>{run}"#),
                "missing",
                "match",
            ),
            (
                format!("{pane}<template>{run}</template>"),
                "present",
                "missing",
            ),
            (
                format!(r#"{pane}<a id="run" class="Selected selected-more">Run</a>"#),
                "present",
                "no-match",
            ),
            (
                format!(r#"{pane}<a id="run">Run</a>"#),
                "present",
                "no-match",
            ),
            (
                format!("{pane}{run}{oversized_pane}"),
                "unavailable",
                "match",
            ),
            (
                format!("{pane}{pane}{oversized_run}"),
                "ambiguous",
                "unavailable",
            ),
        ] {
            let sample = sample_fixture(&checkpoint, &format!("{extra}{UNBOXED_ROW}")).unwrap();
            assert!(
                checkpoint
                    .timeout_error(Some(sample))
                    .ends_with(&format!("pane_rect={pane}; run_class={run}"))
            );
        }
    }

    #[test]
    fn checkpoint_timeout_context_is_opt_in_and_never_reused_on_a_different_sample() {
        assert!(parse_timeout_context(false, TIMEOUT_CONTEXT).is_err());
        for context in [
            TIMEOUT_CONTEXT.replace("content-tests", "content-tests .nested"),
            TIMEOUT_CONTEXT.replace("selected\"", "\""),
            TIMEOUT_CONTEXT.replace("content-tests", &"x".repeat(257)),
            TIMEOUT_CONTEXT.replace("pane_class", "selector"),
        ] {
            assert!(parse_timeout_context(true, &context).is_err());
        }
        let checkpoint = parse_timeout_context(true, TIMEOUT_CONTEXT).unwrap();
        let mut sample = sample_fixture(&checkpoint, UNBOXED_ROW).unwrap();
        assert!(
            checkpoint
                .timeout_error(Some(sample))
                .contains("pane_rect=")
        );
        for (html, reason) in [
            ("", "missing-root"),
            (r#"<div id="report"></div>"#, "missing-descendant"),
            (
                r#"<div id="report"><span class="ran">12</span></div>"#,
                "text-mismatch",
            ),
        ] {
            sample = sample_fixture(&checkpoint, html).unwrap();
            assert!(
                checkpoint
                    .timeout_error(Some(sample))
                    .ends_with(&format!("last_observation={reason}"))
            );
        }
        assert!(
            checkpoint
                .timeout_error(None)
                .ends_with("last_observation=no-post-click-sample")
        );
        assert!(
            checkpoint
                .timeout_error(Some(CheckpointObservation::DocumentNotReady.into()))
                .ends_with("last_observation=document-not-ready")
        );
        let matched = sample_fixture(
            &checkpoint,
            r#"<div id="report"><span class="ran">2</span></div>"#,
        )
        .unwrap();
        assert!(matched.is_match());
        assert!(matched.context.is_none());
        let ambiguous = format!("{UNBOXED_ROW}{UNBOXED_ROW}");
        assert!(
            sample_fixture(&checkpoint, &ambiguous)
                .unwrap_err()
                .ends_with("ambiguous root id")
        );
        let diagnostic_only = parse_timeout_context(true, "null").unwrap();
        let sample = sample_fixture(&diagnostic_only, UNBOXED_ROW).unwrap();
        assert!(
            diagnostic_only
                .timeout_error(Some(sample))
                .ends_with("last_observation=exact-text-no-layout-box")
        );
    }

    #[test]
    fn checkpoint_rejects_ambiguous_scope_even_if_one_match_is_hidden() {
        let root =
            observe(r#"<div id="report"></div><div id="report"><span class="ran">2</span></div>"#);
        assert!(root.unwrap_err().contains("ambiguous root"));
        let descendants = observe(
            r#"<div id="report"><span class="ran">2</span><span class="ran" hidden>2</span></div>"#,
        );
        assert!(
            descendants
                .unwrap_err()
                .contains("ambiguous matching descendants")
        );
    }

    #[test]
    fn checkpoint_rejects_preexisting_state_and_changed_document() {
        let checkpoint = checkpoint();
        assert!(checkpoint.require_unmet(false).is_ok());
        assert!(
            checkpoint
                .require_unmet(true)
                .unwrap_err()
                .contains("before input")
        );
        let identity = CheckpointDocument {
            tab_id: 1,
            document_navigation: 7,
        };
        assert!(checkpoint.require_document(identity, identity).is_ok());
        for actual in [
            CheckpointDocument {
                document_navigation: 8,
                ..identity
            },
            CheckpointDocument {
                tab_id: 2,
                ..identity
            },
        ] {
            assert!(checkpoint.require_document(identity, actual).is_err());
        }
    }

    #[test]
    fn checkpoint_steps_require_input_and_unique_safe_names() {
        let click = r#"{"action":"click_at","x":5,"y":5}"#;
        let capture = r#"{"action":"checkpoint","name":"result","within_id":"report","descendant_class":"ran","text":"2","timeout_ms":100}"#;
        let parse = |s: &str| parse_actions(s.as_bytes()).is_ok();
        assert!(parse(&format!("[{click},{capture}]")));
        assert!(!parse(&format!("[{capture}]")));
        assert!(!parse(&format!("[{click},{capture},{click},{capture}]")));
        assert!(!parse(&format!(
            "[{click},{}]",
            capture.replace("result", "../result")
        )));
        assert!(!parse(&format!(
            "[{click},{}]",
            capture.replace("100", "0")
        )));
        assert!(parse(
            r#"[{"action":"wait_title","value":"old","timeout_ms":100}]"#
        ));
    }

    #[test]
    fn checkpoint_output_never_reuses_or_overwrites_an_existing_sidecar() {
        let root = std::env::temp_dir().join(format!(
            "satin-checkpoint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let frame = root.join("smoke.png");
        let checkpoint = checkpoint();
        let output = checkpoint.output_path(&frame);
        let identity = CheckpointDocument {
            tab_id: 1,
            document_navigation: 7,
        };
        let document = rust_browser_engine::html::parse("<p>fresh</p>");
        std::fs::write(&output, "stale").unwrap();
        let capture = |outputs: &mut CheckpointOutputs| {
            checkpoint.capture(
                &frame,
                outputs,
                identity,
                "https://example.test/",
                "fixture",
                &document,
            )
        };
        let mut outputs = CheckpointOutputs::default();
        assert!(
            capture(&mut outputs)
                .unwrap_err()
                .contains("could not create new output")
        );
        assert!(outputs.summary().is_none());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "stale");
        std::fs::remove_file(&output).unwrap();
        capture(&mut outputs).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(json["tabId"], 1);
        assert_eq!(json["documentNavigation"], 7);
        let summary = outputs.summary().unwrap();
        assert_eq!(json["runId"], summary["runId"]);
        assert_eq!(summary["names"], serde_json::json!(["results"]));
        assert!(json["html"].as_str().unwrap().contains("<p>fresh</p>"));
        assert!(!frame.exists());
        drop(outputs);
        assert!(
            !output.exists(),
            "unsuccessful attempt must roll back its sidecar"
        );

        let mut outputs = CheckpointOutputs::default();
        capture(&mut outputs).unwrap();
        outputs.commit();
        drop(outputs);
        assert!(output.exists(), "successful attempt keeps its sidecar");
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
