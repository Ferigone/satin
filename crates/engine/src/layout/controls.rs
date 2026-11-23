use super::*;
impl Builder<'_> {
    pub(super) fn is_replaced_control(&self, id: NodeId) -> bool {
        self.page.document.namespace(id) == crate::dom::Namespace::Html
            && matches!(self.page.document.tag(id), "input" | "textarea" | "select")
            && self.page.form_controls.contains_key(&id)
    }
    pub(super) fn control_intrinsic(&mut self, id: NodeId) -> (f32, f32) {
        let control = self.page.form_controls[&id].clone();
        let s = &self.styles[id.0];
        let digit = self.shape("0", s).advance.max(1.);
        let line = s
            .line_height_absolute
            .unwrap_or(s.font_size * s.line_height);
        match control.kind.as_str() {
            "checkbox" | "radio" => (13., 13.),
            "textarea" => {
                let cols = self
                    .page
                    .document
                    .attr(id, "cols")
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|&value| value > 0)
                    .unwrap_or(20)
                    .min(1000);
                let rows = self
                    .page
                    .document
                    .attr(id, "rows")
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|&value| value > 0)
                    .unwrap_or(2)
                    .min(1000);
                (cols as f32 * digit, rows as f32 * line)
            }
            "select-one" | "select-multiple" => {
                let mut width = digit;
                for option in &control.options {
                    let work = option.label.len() / 16 + 1;
                    if self.work_remaining < work {
                        self.limited = true;
                        break;
                    }
                    self.work_remaining -= work;
                    width = width.max(self.shape(&option.label, s).advance);
                }
                let rows = self
                    .page
                    .document
                    .attr(id, "size")
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|&value| value > 0)
                    .unwrap_or(if control.kind == "select-multiple" {
                        4
                    } else {
                        1
                    })
                    .min(1000);
                (width + digit * 2., line * rows as f32)
            }
            "submit" | "reset" | "button" => {
                let label = if control.value.is_empty() {
                    if control.kind == "submit" {
                        "Prześlij"
                    } else if control.kind == "reset" {
                        "Resetuj"
                    } else {
                        ""
                    }
                } else {
                    &control.value
                };
                (self.shape(label, s).advance.max(digit), line)
            }
            _ => {
                let cols = self
                    .page
                    .document
                    .attr(id, "size")
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|&value| value > 0)
                    .unwrap_or(20)
                    .min(1000);
                (cols as f32 * digit, line)
            }
        }
    }
    pub(super) fn control_command(&mut self, id: NodeId, rect: Rect, s: &Style) {
        let control = self.page.form_controls[&id].clone();
        let listbox = control.kind == "select-multiple"
            || self
                .page
                .document
                .attr(id, "size")
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|value| value > 1);
        self.commands.push(DrawCommand::Control {
            node: id,
            rect,
            control,
            style: ControlPaintStyle {
                font_size: s.font_size,
                color: s.color,
                font: s.font_request(),
                font_set: self.page.fonts.clone(),
                line_height: s
                    .line_height_absolute
                    .unwrap_or(s.font_size * s.line_height),
                placeholder: self
                    .page
                    .document
                    .attr(id, "placeholder")
                    .unwrap_or("")
                    .chars()
                    .take(8192)
                    .collect(),
                listbox,
            },
        });
    }
}
