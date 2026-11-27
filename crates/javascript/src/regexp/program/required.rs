use super::{Expr, Flags, Member};
use crate::{JsError, Vm};

const MAX_NEEDLE: usize = 16;
const SUBJECT_WINDOW: usize = 256;

#[derive(Debug)]
pub(super) struct Literal {
    bytes: [u8; MAX_NEEDLE],
    len: usize,
    ignore_case: bool,
}

impl Literal {
    pub(super) fn in_pattern(body: &Expr, flags: Flags) -> Option<Self> {
        let mut best = Self {
            bytes: [0; MAX_NEEDLE],
            len: 0,
            ignore_case: false,
        };
        best.visit(body, flags);
        (best.len >= 3).then_some(best)
    }

    fn visit(&mut self, expr: &Expr, flags: Flags) {
        match expr {
            Expr::Sequence(parts) => {
                let mut run = [0; MAX_NEEDLE];
                let mut len = 0;
                for part in parts {
                    if let Expr::Character(Member::Point(p)) = part
                        && *p < 128
                    {
                        if len < run.len() {
                            run[len] = *p as u8;
                            len += 1;
                        }
                    } else {
                        self.consider(&run[..len], flags);
                        len = 0;
                        self.visit(part, flags);
                    }
                }
                self.consider(&run[..len], flags);
            }
            Expr::Capture(_, body) => self.visit(body, flags),
            Expr::Modifiers { body, add, remove } => {
                self.visit(body, flags.with_modifiers(*add, *remove))
            }
            Expr::Repeat { body, minimum, .. } if *minimum > 0 => self.visit(body, flags),
            // No branch, optional repetition or lookaround supplies mandatory
            // consuming text for the complete match.
            _ => {}
        }
    }

    fn consider(&mut self, run: &[u8], flags: Flags) {
        if run.len() > self.len {
            self.len = run.len();
            self.ignore_case = flags.has(Flags::IGNORE_CASE);
            self.bytes[..self.len].copy_from_slice(run);
            if self.ignore_case {
                self.bytes[..self.len].make_ascii_lowercase();
            }
        }
    }

    pub(super) fn possible(&self, vm: &mut Vm, input: &[u16]) -> Result<bool, JsError> {
        // Limit the speculative scan. Long subjects stay with the ordinary
        // search so a quick positive match never incurs a full-input pass.
        if input.len() > SUBJECT_WINDOW {
            return Ok(true);
        }
        if input.len() < self.len {
            return Ok(false);
        }
        vm.charge_buffer_work(input.len())?;
        let mut text = [0u8; SUBJECT_WINDOW];
        for (out, &unit) in text.iter_mut().zip(input) {
            // Unicode folding can match an ASCII literal to a non-ASCII point.
            if unit >= 128 {
                return Ok(true);
            }
            let byte = unit as u8;
            *out = if self.ignore_case {
                byte.to_ascii_lowercase()
            } else {
                byte
            };
        }
        let windows = input.len() - self.len + 1;
        vm.charge_buffer_work(windows * self.len)?;
        let needle = &self.bytes[..self.len];
        Ok(text[..input.len()]
            .windows(self.len)
            .any(|part| part == needle))
    }
}
