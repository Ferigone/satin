//! Scan ASCII and Latin-1 runs without decoding every byte into a scalar.
//! Returns how far a block or line comment may advance without a lexer event.

const LOW: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

#[inline]
fn has_byte(word: u64, needle: u8) -> bool {
    let x = word ^ (u64::from(needle) * LOW);
    x.wrapping_sub(LOW) & !x & HIGH != 0
}

pub(super) fn safe_prefix<const BLOCK: bool>(bytes: &[u8]) -> usize {
    let boundary = |b| matches!(b, b'\n' | b'\r') || BLOCK && b == b'*';
    let mut groups = bytes.chunks_exact(8);
    for (i, group) in groups.by_ref().enumerate() {
        let word = u64::from_ne_bytes(group.try_into().unwrap());
        if has_byte(word, b'\r') || has_byte(word, b'\n') || BLOCK && has_byte(word, b'*') {
            let within = group.iter().copied().position(boundary).unwrap_or(8);
            return i * 8 + within;
        }
    }
    let rest = groups.remainder();
    bytes.len() - rest.len() + rest.iter().copied().position(boundary).unwrap_or(rest.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_scan_agrees_with_scalar_at_all_offsets_and_tails() {
        for len in 0..67 {
            for position in 0..=len {
                for needle in [b'\r', b'\n', b'*', 0xe9, 0xff] {
                    let mut input = vec![0xb7; len];
                    if position < len {
                        input[position] = needle;
                    }
                    let expected_line = if matches!(needle, b'\r' | b'\n') { position } else { len };
                    let expected_block = if matches!(needle, b'\r' | b'\n' | b'*') { position } else { len };
                    assert_eq!(safe_prefix::<false>(&input), expected_line);
                    assert_eq!(safe_prefix::<true>(&input), expected_block);
                }
            }
        }
    }
}
