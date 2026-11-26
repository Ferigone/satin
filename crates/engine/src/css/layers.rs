//! Document-order cascade layers. A parent's own declarations follow its sublayers.
use super::{CssByte, component_boundaries, identifier, trim_boundary};
use std::collections::HashMap;

#[derive(Default)]
struct Layer {
    children: Vec<usize>,
    names: HashMap<String, usize>,
}

pub(super) struct Layers(Vec<Layer>);

pub(super) fn header(source: &str) -> Option<&str> {
    let rest = source.strip_prefix('@')?;
    let mut pos = 0;
    identifier(rest, &mut pos)?
        .eq_ignore_ascii_case("layer")
        .then(|| trim_boundary(&rest[pos..]))
}

fn path(source: &str) -> Option<Vec<String>> {
    let mut pos = 0;
    let mut result = vec![];
    loop {
        let token = &source.as_bytes()[pos..];
        let start = if token.first() == Some(&b'-') {
            &token[1..]
        } else {
            token
        };
        if !start.first().is_some_and(|b| {
            b.is_ascii_alphabetic() || *b >= 128 || matches!(*b, b'_' | b'-' | b'\\')
        }) {
            return None;
        }
        let name = identifier(source, &mut pos)?;
        if [
            "initial",
            "inherit",
            "unset",
            "revert",
            "revert-layer",
            "default",
        ]
        .iter()
        .any(|k| name.eq_ignore_ascii_case(k))
        {
            return None;
        }
        result.push(name);
        if pos == source.len() {
            return Some(result);
        }
        if source.as_bytes()[pos] != b'.' || result.len() >= 32 {
            return None;
        }
        pos += 1;
    }
}

pub(super) fn valid_block(names: &str) -> bool {
    names.is_empty() || path(names).is_some()
}

pub(super) fn order(names: &str) -> Option<Vec<Vec<String>>> {
    let mut paths = Vec::new();
    let mut start = 0;
    let ends = component_boundaries(names, b',')
        .filter_map(|(i, t)| matches!(t, CssByte::Unquoted(_)).then_some(i))
        .chain(std::iter::once(names.len()));
    for end in ends {
        if paths.len() == 8192 {
            return None;
        }
        let Some(parsed) = path(trim_boundary(&names[start..end])) else {
            return None;
        };
        paths.push(parsed);
        start = end + 1;
    }
    Some(paths)
}

impl Layers {
    pub(super) fn new() -> Self {
        Self(vec![Layer::default()])
    }

    fn child(&mut self, parent: usize, name: Option<String>) -> Option<usize> {
        if let Some(existing) = name.as_ref().and_then(|n| self.0[parent].names.get(n)) {
            return Some(*existing);
        }
        let id = self.0.len();
        if id >= 8192 {
            return None;
        }
        if let Some(name) = name {
            self.0[parent].names.insert(name, id);
        }
        self.0[parent].children.push(id);
        self.0.push(Layer::default());
        Some(id)
    }

    fn register(&mut self, parent: usize, names: Vec<String>) -> Option<usize> {
        names
            .into_iter()
            .try_fold(parent, |p, n| self.child(p, Some(n)))
    }

    pub(super) fn block(&mut self, parent: usize, names: &str) -> Option<usize> {
        if names.is_empty() {
            self.child(parent, None)
        } else {
            self.register(parent, path(names)?)
        }
    }

    pub(super) fn statement(&mut self, parent: usize, names: &str) {
        // A malformed member invalidates the entire order statement.
        let Some(paths) = order(names) else { return };
        for names in paths {
            self.register(parent, names);
        }
    }

    pub(super) fn ranks(&self) -> Vec<usize> {
        let mut ranks = vec![0; self.0.len()];
        // Rank zero is reserved for SVG presentation hints.
        let mut next = 1;
        let mut stack = vec![(0, false)];
        while let Some((id, visited)) = stack.pop() {
            if visited {
                ranks[id] = next;
                next += 1;
            } else {
                stack.push((id, true));
                stack.extend(
                    self.0[id]
                        .children
                        .iter()
                        .rev()
                        .map(|&child| (child, false)),
                );
            }
        }
        ranks[0] = usize::MAX;
        ranks
    }
}
