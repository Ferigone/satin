use std::{ops::Range, rc::Rc};

#[derive(Clone, Debug)]
enum SourceStorage {
    Ascii(Rc<[u8]>),
    // ECMAScript source text created from a JS string can contain isolated
    // UTF-16 surrogates. Keep their numeric values instead of replacing them.
    CodePoints(Rc<[u32]>),
}

#[derive(Clone, Debug)]
pub(crate) struct SourceSpan {
    buffer: SourceStorage,
    range: Range<usize>,
}

impl SourceSpan {
    pub(super) fn from_text(source: &str) -> Self {
        if source.is_ascii() {
            Self {
                buffer: SourceStorage::Ascii(Rc::from(source.as_bytes())),
                range: 0..source.len(),
            }
        } else {
            Self::from_points(source.chars().map(u32::from))
        }
    }

    pub(super) fn from_points(points: impl Iterator<Item = u32>) -> Self {
        let buffer: Rc<[u32]> = points.collect();
        let range = 0..buffer.len();
        Self { buffer: SourceStorage::CodePoints(buffer), range }
    }

    pub(crate) fn storage(&self) -> (usize, usize) {
        match &self.buffer {
            SourceStorage::Ascii(data) => (data.as_ptr() as usize, data.len()),
            SourceStorage::CodePoints(data) => (data.as_ptr() as usize, std::mem::size_of_val(&**data)),
        }
    }

    pub(crate) fn generated(text: &str) -> Rc<Self> {
        Rc::new(Self::from_text(text))
    }

    pub(crate) fn len(&self) -> usize {
        self.range.len()
    }

    pub(super) fn span(&self, range: Range<usize>) -> Rc<Self> {
        debug_assert!(range.end <= self.len());
        let base = self.range.start;
        Rc::new(Self { buffer: self.buffer.clone(), range: base + range.start..base + range.end })
    }

    pub(super) fn point(&self, index: usize) -> Option<u32> {
        if index >= self.len() {
            return None;
        }
        let index = self.range.start + index;
        Some(match &self.buffer {
            SourceStorage::Ascii(data) => u32::from(data[index]),
            SourceStorage::CodePoints(data) => data[index],
        })
    }

    pub(super) fn points(&self, range: Range<usize>) -> impl Iterator<Item = u32> + '_ {
        assert!(range.end <= self.len());
        let start = self.range.start + range.start;
        let end = self.range.start + range.end;
        let (ascii, wide): (&[u8], &[u32]) = match &self.buffer {
            SourceStorage::Ascii(data) => (&data[start..end], &[]),
            SourceStorage::CodePoints(data) => (&[], &data[start..end]),
        };
        ascii.iter().copied().map(u32::from).chain(wide.iter().copied())
    }

    pub(crate) fn utf16_len(&self, range: Range<usize>) -> usize {
        match &self.buffer {
            SourceStorage::Ascii(_) => range.len(),
            SourceStorage::CodePoints(_) => self.points(range).map(|p| if p > 0xffff { 2 } else { 1 }).sum(),
        }
    }

    pub(crate) fn utf16_range(&self, range: Range<usize>) -> Vec<u16> {
        let mut units = Vec::with_capacity(self.utf16_len(range.clone()));
        for point in self.points(range) {
            push_source_point(&mut units, point);
        }
        units
    }
}

pub(super) fn push_source_point(units: &mut Vec<u16>, point: u32) {
    if point <= 0xffff {
        units.push(point as u16);
    } else {
        let n = point - 0x10000;
        units.push((0xd800 + (n >> 10)) as u16);
        units.push((0xdc00 + (n & 1023)) as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_spans_share_one_byte_per_character() {
        let text = "function f() { return 42; }";
        let source = SourceSpan::from_text(text);
        let inner = source.span(15..24);
        assert_eq!(source.storage().1, text.len());
        assert_eq!(inner.storage(), source.storage());
        assert_eq!(inner.utf16_range(0..inner.len()), "return 42".encode_utf16().collect::<Vec<_>>());
        assert_eq!(inner.point(inner.len()), None);
    }

    #[test]
    fn wide_spans_preserve_lone_surrogates_and_astral_characters() {
        let source = SourceSpan::from_points([0x7b, 0x1f600, 0xd800, 0x7d].into_iter());
        let inner = source.span(1..3);
        assert_eq!(inner.utf16_range(0..2), [0xd83d, 0xde00, 0xd800]);
        assert_eq!(source.storage().1, 16);
    }
}
