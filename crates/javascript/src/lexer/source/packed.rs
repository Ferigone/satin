//! A code-point-addressed source buffer with an independently chosen cell
//! width for each page. A rare non-ASCII character widens only its page.
const SHIFT: usize = 8;
const UNITS: usize = 1 << SHIFT;
const OFFSET_BITS: u32 = 30;
const OFFSET_MASK: u32 = (1 << OFFSET_BITS) - 1;
const MIN_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(super) struct PackedSource {
    directory: Box<[u32]>,
    bytes: Box<[u8]>,
    len: usize,
}

impl PackedSource {
    pub(super) fn from_text(text: &str) -> Option<Self> {
        if text.len() < MIN_SOURCE_BYTES || text.len() > OFFSET_MASK as usize {
            return None;
        }
        let mut directory = Vec::with_capacity(text.len().div_ceil(UNITS));
        let mut source_ends = Vec::with_capacity(directory.capacity());
        let mut len = 0usize;
        let mut bytes = 0usize;
        let mut source_end = 0usize;
        while source_end < text.len() {
            let (taken, used, kind) = page(&text[source_end..]);
            let start = u32::try_from(bytes).ok()?;
            if start > OFFSET_MASK {
                return None;
            }
            directory.push(start | (kind << OFFSET_BITS));
            source_end += taken;
            source_ends.push(source_end as u32);
            len += used;
            bytes = bytes.checked_add(used * (1usize << kind))?;
        }
        // A uniformly wide input should keep its simple contiguous u32 path.
        // Charge the index in the decision, not just the encoded payload.
        if bytes.checked_add(directory.len() * size_of::<u32>())? > len * 3 {
            return None;
        }
        let mut payload = Vec::with_capacity(bytes);
        let mut start = 0;
        for (index, end) in source_ends.into_iter().map(|n| n as usize).enumerate() {
            let page = &text[start..end];
            start = end;
            let kind = directory[index] >> OFFSET_BITS;
            if kind == 0 && page.len() == (len - index * UNITS).min(UNITS) {
                // Encoded byte count equals scalar count, so it is ASCII.
                payload.extend_from_slice(page.as_bytes());
            } else {
                for point in page.chars().map(u32::from) {
                    match kind {
                        0 => payload.push(point as u8),
                        1 => payload.extend_from_slice(&(point as u16).to_le_bytes()),
                        _ => payload.extend_from_slice(&point.to_le_bytes()),
                    }
                }
            }
        }
        debug_assert_eq!(payload.len(), bytes);
        Some(Self { directory: directory.into_boxed_slice(), bytes: payload.into_boxed_slice(), len })
    }

    pub(super) fn len(&self) -> usize { self.len }

    pub(super) fn storage_bytes(&self) -> usize {
        size_of::<Self>() + size_of_val(self.directory.as_ref()) + self.bytes.len()
    }

    #[inline]
    pub(super) fn point(&self, index: usize) -> u32 {
        let entry = self.directory[index >> SHIFT];
        let cell = index & (UNITS - 1);
        let offset = (entry & OFFSET_MASK) as usize;
        match entry >> OFFSET_BITS {
            0 => u32::from(self.bytes[offset + cell]),
            1 => {
                let start = offset + cell * 2;
                u32::from(u16::from_le_bytes(self.bytes[start..start + 2].try_into().unwrap()))
            }
            _ => {
                let start = offset + cell * 4;
                u32::from_le_bytes(self.bytes[start..start + 4].try_into().unwrap())
            }
        }
    }
}

fn page(remaining: &str) -> (usize, usize, u32) {
    let prefix = &remaining.as_bytes()[..remaining.len().min(UNITS)];
    if prefix.is_ascii() {
        return (prefix.len(), prefix.len(), 0);
    }
    let mut bytes = 0;
    let mut count = 0;
    let mut kind = 0;
    for ch in remaining.chars().take(UNITS) {
        let p = u32::from(ch);
        kind = kind.max(if p <= 0xff { 0 } else if p <= 0xffff { 1 } else { 2 });
        bytes += ch.len_utf8();
        count += 1;
    }
    (bytes, count, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_wide_pages_do_not_widen_neighbors() {
        let source = format!("{}ł🙂{}é", "a".repeat(UNITS - 1), "b".repeat(90_000));
        let packed = PackedSource::from_text(&source).unwrap();
        assert_eq!(packed.len(), source.chars().count());
        assert!(packed.storage_bytes() < source.len() + 3_000);
        assert_eq!(packed.directory[0] >> OFFSET_BITS, 1);
        assert_eq!(packed.directory[1] >> OFFSET_BITS, 2);
        assert_eq!(packed.directory[2] >> OFFSET_BITS, 0);
        assert!(source.chars().enumerate().all(|(i, c)| packed.point(i) == u32::from(c)));
    }

    #[test]
    fn uniform_wide_input_stays_on_the_contiguous_path() {
        assert!(PackedSource::from_text(&"🙂".repeat(30_000)).is_none());
    }
}
