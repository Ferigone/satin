use super::*;
use crate::{layout::ControlPaintStyle, script::ControlSnapshot};
impl Canvas {
    pub(super) fn form_control(
        &mut self,
        rect: Rect,
        control: &ControlSnapshot,
        style: &ControlPaintStyle,
        fonts: &mut Fonts,
        scale: f32,
    ) {
        self.push_state(Affine2D::IDENTITY, Some(rect));
        let ink = if control.disabled {
            Color(120, 124, 128, 255)
        } else {
            style.color
        };
        let accent = Color(17, 121, 173, 255);
        let size = style.font_size * scale;
        let line = (style.line_height * scale).max(1.);
        if matches!(control.kind.as_str(), "checkbox" | "radio") {
            let edge = rect.width.min(rect.height).min(24. * scale).max(0.);
            let cx = rect.x + rect.width / 2.;
            let cy = rect.y + rect.height / 2.;
            if control.kind == "radio" {
                self.control_disc(
                    cx,
                    cy,
                    edge / 2.,
                    if control.checked { accent } else { ink },
                );
                self.control_disc(cx, cy, (edge / 2. - scale).max(0.), Color::WHITE);
                if control.checked {
                    self.control_disc(cx, cy, edge * 0.24, accent);
                }
            } else {
                self.fill_rect(
                    rect,
                    if control.checked {
                        accent
                    } else {
                        Color::WHITE
                    },
                );
                if control.checked {
                    self.stroke_line(
                        cx - edge * 0.3,
                        cy,
                        cx - edge * 0.08,
                        cy + edge * 0.24,
                        2. * scale,
                        Color::WHITE,
                    );
                    self.stroke_line(
                        cx - edge * 0.08,
                        cy + edge * 0.24,
                        cx + edge * 0.32,
                        cy - edge * 0.24,
                        2. * scale,
                        Color::WHITE,
                    );
                }
            }
        } else if control.kind.starts_with("select-") {
            if style.listbox {
                let rows = (rect.height / line).floor().max(1.) as usize;
                let selected = control
                    .options
                    .iter()
                    .position(|option| option.selected)
                    .unwrap_or(0);
                let first = if control.focused {
                    selected.saturating_sub(rows - 1)
                } else {
                    0
                };
                for (row, option) in control
                    .options
                    .iter()
                    .skip(first)
                    .take(rows + 1)
                    .enumerate()
                {
                    let y = rect.y + row as f32 * line;
                    if option.selected {
                        self.fill_rect(
                            Rect {
                                x: rect.x,
                                y,
                                width: rect.width,
                                height: line,
                            },
                            if control.focused {
                                accent
                            } else {
                                Color(213, 224, 233, 255)
                            },
                        );
                    }
                    let color = if option.disabled {
                        Color(140, 140, 140, 255)
                    } else if option.selected && control.focused {
                        Color::WHITE
                    } else {
                        ink
                    };
                    let run = fonts.shape_text(&option.label, size, &style.font, &style.font_set);
                    self.text(rect.x + 2. * scale, y + run.ascent, &run, color, fonts);
                }
            } else {
                let label = control
                    .options
                    .iter()
                    .find(|option| option.selected)
                    .map(|option| option.label.as_str())
                    .unwrap_or("");
                let run = fonts.shape_text(label, size, &style.font, &style.font_set);
                self.text(
                    rect.x + scale,
                    rect.y + (rect.height - line) / 2. + run.ascent,
                    &run,
                    ink,
                    fonts,
                );
                let arrow_width = 12. * scale;
                self.fill_rect(
                    Rect {
                        x: (rect.x + rect.width - arrow_width - 3. * scale).max(rect.x),
                        y: rect.y,
                        width: arrow_width + 3. * scale,
                        height: rect.height,
                    },
                    Color(241, 244, 246, 255),
                );
                let cx = rect.x + rect.width - 8. * scale;
                let cy = rect.y + rect.height / 2.;
                self.stroke_line(
                    cx - 3. * scale,
                    cy - scale,
                    cx,
                    cy + 2. * scale,
                    scale.max(1.),
                    ink,
                );
                self.stroke_line(
                    cx,
                    cy + 2. * scale,
                    cx + 3. * scale,
                    cy - scale,
                    scale.max(1.),
                    ink,
                );
            }
        } else {
            let password = control.kind == "password";
            let editable = !matches!(
                control.kind.as_str(),
                "submit" | "reset" | "button" | "file" | "hidden"
            );
            let mut value = if password {
                "•".repeat(control.value.encode_utf16().count())
            } else {
                control.value.clone()
            };
            if value.is_empty() && matches!(control.kind.as_str(), "submit" | "reset") {
                value = if control.kind == "submit" {
                    "Prześlij".into()
                } else {
                    "Resetuj".into()
                };
            }
            if control.kind == "file" {
                value = if value.is_empty() {
                    "Wybierz plik…".into()
                } else {
                    format!("Wybierz plik…  {value}")
                };
            }
            let placeholder = editable && value.is_empty();
            if placeholder {
                value = style.placeholder.clone();
            }
            let units = value.encode_utf16().collect::<Vec<_>>();
            let start = control.selection_start.min(units.len());
            let end = control.selection_end.min(units.len()).max(start);
            let multiline = control.kind == "textarea";
            let mut offset = 0usize;
            let text_color = if placeholder {
                Color(117, 124, 132, 255)
            } else {
                ink
            };
            let lines = if multiline {
                value.split('\n').collect::<Vec<_>>()
            } else {
                vec![value.as_str()]
            };
            let caret_line = if multiline && !placeholder {
                units[..end].iter().filter(|&&u| u == 10).count()
            } else {
                0
            };
            let visible_rows = (rect.height / line).floor().max(1.) as usize;
            let first_line = if multiline && control.focused {
                caret_line.saturating_sub(visible_rows - 1)
            } else {
                0
            };
            for (index, text) in lines.into_iter().enumerate() {
                let count = text.encode_utf16().count();
                if index < first_line {
                    offset += count + 1;
                    continue;
                }
                if index > first_line + visible_rows {
                    break;
                }
                let run = fonts.shape_text(text, size, &style.font, &style.font_set);
                let start_in = start.saturating_sub(offset).min(count);
                let end_in = end.saturating_sub(offset).min(count);
                let text_units = text.encode_utf16().collect::<Vec<_>>();
                let before_start = String::from_utf16_lossy(&text_units[..start_in]);
                let before_end = String::from_utf16_lossy(&text_units[..end_in]);
                let selection_x = fonts
                    .shape_text(&before_start, size, &style.font, &style.font_set)
                    .advance;
                let caret_x = fonts
                    .shape_text(&before_end, size, &style.font, &style.font_set)
                    .advance;
                let shift = if !multiline && control.focused {
                    (caret_x - rect.width + 3. * scale).max(0.)
                } else {
                    0.
                };
                let x = rect.x + scale - shift;
                let y = rect.y + (index - first_line) as f32 * line;
                if control.focused && !placeholder && end_in > start_in {
                    self.fill_rect(
                        Rect {
                            x: x + selection_x,
                            y,
                            width: caret_x - selection_x,
                            height: line,
                        },
                        Color(172, 211, 244, 255),
                    );
                }
                self.text(x, y + run.ascent, &run, text_color, fonts);
                if control.focused
                    && editable
                    && !control.read_only
                    && start == end
                    && !placeholder
                    && end >= offset
                    && end <= offset + count
                {
                    self.fill_rect(
                        Rect {
                            x: x + caret_x,
                            y,
                            width: scale.max(1.),
                            height: line,
                        },
                        ink,
                    );
                } else if placeholder && control.focused && editable {
                    self.fill_rect(
                        Rect {
                            x: rect.x + scale,
                            y,
                            width: scale.max(1.),
                            height: line,
                        },
                        ink,
                    );
                }
                offset += count + 1;
            }
        }
        if control.focused {
            let width = scale.max(1.);
            for border in [
                Rect {
                    height: width,
                    ..rect
                },
                Rect {
                    y: rect.y + rect.height - width,
                    height: width,
                    ..rect
                },
                Rect { width, ..rect },
                Rect {
                    x: rect.x + rect.width - width,
                    width,
                    ..rect
                },
            ] {
                self.fill_rect(border, accent);
            }
        }
        self.pop_state();
    }
    fn control_disc(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        self.transformed_disc(cx, cy, radius, color);
    }
}
