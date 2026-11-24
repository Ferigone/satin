//! Exact ECMAScript property aliases and Unicode 17.0 code-point sets.
//! Script extensions have a separate index from Script.
mod data {
    include!("data/property-index.rs");
}

const RANGES: &[u8] = include_bytes!("data/property-ranges.bin");

#[derive(Clone, Copy, Debug)]
pub(super) struct Property {
    ranges: &'static [[u8; 6]],
}

impl Property {
    pub(super) fn parse(name: &str) -> Option<Self> {
        let (index, value) = match name.split_once('=') {
            Some(("gc" | "General_Category", value)) => (data::CATEGORIES, value),
            Some(("sc" | "Script", value)) => (data::SCRIPTS, value),
            Some(("scx" | "Script_Extensions", value)) => (data::SCRIPT_EXTENSIONS, value),
            Some(_) => return None,
            None => {
                return Self::lookup(data::CATEGORIES, name)
                    .or_else(|| Self::lookup(data::BINARY, name));
            }
        };
        Self::lookup(index, value)
    }

    fn lookup(index: &[(&str, usize, usize)], name: &str) -> Option<Self> {
        let entry = index
            .binary_search_by(|(value, _, _)| value.cmp(&name))
            .ok()?;
        let (_, offset, count) = index[entry];
        let (ranges, _) = RANGES[offset..offset + count * 6].as_chunks::<6>();
        Some(Self { ranges })
    }

    pub(super) fn contains(self, point: u32) -> bool {
        self.ranges
            .binary_search_by(|&[a, b, c, d, e, f]| {
                let start = u32::from_be_bytes([0, a, b, c]);
                let end = u32::from_be_bytes([0, d, e, f]);
                if point < start {
                    std::cmp::Ordering::Greater
                } else if point > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}
