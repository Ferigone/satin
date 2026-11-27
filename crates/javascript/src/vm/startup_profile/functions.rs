use super::{ACTIVE, Measurement};
use crate::lexer::SourceSpan;
use std::collections::HashMap;

type Site = [u64; 4];
const MAX_SITES: usize = 4096;

#[derive(Default)]
pub(super) struct Functions {
    indices: HashMap<Site, usize>,
    rows: Vec<(Site, u64, u64)>,
    stack: Vec<Option<usize>>,
    denied_stack: Vec<Site>,
    unassigned: u64,
}

impl Functions {
    fn push(&mut self, site: Site) {
        let index = self.indices.get(&site).copied().or_else(|| {
            if self.rows.len() >= MAX_SITES { return None; }
            let index = self.rows.len();
            self.rows.push((site, 0, 0));
            self.indices.insert(site, index);
            Some(index)
        });
        if let Some(index) = index { self.rows[index].1 += 1; }
        self.stack.push(index);
    }

    pub(super) fn charge(&mut self, units: u64) {
        if let Some(Some(index)) = self.stack.last() {
            self.rows[*index].2 += units;
        } else {
            self.unassigned += units;
        }
    }

    pub(super) fn finish(mut self, m: &mut Measurement) {
        self.rows.sort_unstable_by_key(|(site, _, work)| (std::cmp::Reverse(*work), *site));
        m.top_functions = self.rows.into_iter().take(24)
            .map(|([total,start,end,tag],calls,work)| [total,start,end,tag,calls,work])
            .collect();
        m.denied_stack = self.denied_stack;
        m.unassigned_work = self.unassigned;
    }
}

pub(crate) struct Frame(bool);

pub(crate) fn function(source: &SourceSpan) -> Frame {
    Frame(ACTIVE.with_borrow_mut(|state| {
        let Some(active) = state else { return false };
        active.functions.push(source.profile_site());
        true
    }))
}

impl Drop for Frame {
    fn drop(&mut self) {
        if self.0 {
            ACTIVE.with_borrow_mut(|state| {
                if let Some(active) = state { active.functions.stack.pop(); }
            });
        }
    }
}

pub(crate) fn record_denial() {
    ACTIVE.with_borrow_mut(|state| {
        let Some(active) = state else { return };
        let f = &mut active.functions;
        if f.denied_stack.is_empty() {
            f.denied_stack = f.stack.iter().rev().take(24).flatten().map(|i| f.rows[*i].0).collect();
        }
    });
}
