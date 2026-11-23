//! Web-font container and outline preflight before invoking infrastructure codecs.
use super::*;
use std::io::Read;
fn bad(message: &str) -> FontError {
    FontError(message.into())
}
fn u16be(data: &[u8], offset: usize) -> Result<u16, FontError> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| bad("Truncated font"))?;
    Ok(u16::from_be_bytes(bytes.try_into().unwrap()))
}
fn u32be(data: &[u8], offset: usize) -> Result<u32, FontError> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| bad("Truncated font"))?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}
fn var128(data: &[u8], offset: &mut usize) -> Result<usize, FontError> {
    let mut value = 0usize;
    for index in 0..5 {
        let b = *data
            .get(*offset)
            .ok_or_else(|| bad("Truncated WOFF directory"))?;
        *offset += 1;
        if index == 0 && b == 0x80 {
            return Err(bad("Invalid base128 value"));
        }
        value = value
            .checked_mul(128)
            .and_then(|v| v.checked_add(usize::from(b & 127)))
            .filter(|v| *v <= u32::MAX as usize)
            .ok_or_else(|| bad("WOFF integer overflow"))?;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err(bad("Invalid WOFF integer"))
}
fn var255(data: &[u8], offset: &mut usize) -> Result<usize, FontError> {
    let b = *data
        .get(*offset)
        .ok_or_else(|| bad("Truncated WOFF points"))?;
    *offset += 1;
    Ok(match b {
        253 => {
            let n = u16be(data, *offset)?;
            *offset += 2;
            usize::from(n)
        }
        254 => {
            let n = *data
                .get(*offset)
                .ok_or_else(|| bad("Truncated WOFF points"))?;
            *offset += 1;
            506 + usize::from(n)
        }
        255 => {
            let n = *data
                .get(*offset)
                .ok_or_else(|| bad("Truncated WOFF points"))?;
            *offset += 1;
            253 + usize::from(n)
        }
        n => usize::from(n),
    })
}

fn transformed_glyf(data: &[u8]) -> Result<(), FontError> {
    let count = usize::from(u16be(data, 4)?);
    let contours = u32be(data, 8)? as usize;
    let points = u32be(data, 12)? as usize;
    if count == 0 || contours != count * 2 || points > data.len() {
        return Err(bad("Invalid transformed glyph counts"));
    }
    let cs = data
        .get(36..36 + contours)
        .ok_or_else(|| bad("Truncated glyph contours"))?;
    let ps = data
        .get(36 + contours..36 + contours + points)
        .ok_or_else(|| bad("Truncated glyph points"))?;
    let mut offset = 0;
    let mut total = 0usize;
    for glyph in 0..count {
        let n = u16be(cs, glyph * 2)? as i16;
        if n > 0 {
            let mut current = 0;
            for _ in 0..n {
                current += var255(ps, &mut offset)?;
                if current > 8192 {
                    return Err(bad("Glyph point budget exceeded"));
                }
            }
            total += current;
            if total > 1_000_000 {
                return Err(bad("Font point budget exceeded"));
            }
        }
    }
    Ok(())
}
fn woff_preflight(data: &[u8], version: u8) -> Result<Option<(usize, usize)>, FontError> {
    if data.get(4..8) == Some(b"ttcf") {
        return Err(bad("Downloaded font collections are unsupported"));
    }
    if u32be(data, 8)? as usize != data.len() {
        return Err(bad("WOFF file length mismatch"));
    }
    let count = usize::from(u16be(data, 12)?);
    if count == 0 || count > 128 {
        return Err(bad("Font table count limit exceeded"));
    }
    if u32be(data, 16)? as usize > MAX_DECODED_FONT_BYTES {
        return Err(bad("Decoded font byte limit exceeded"));
    }
    let mut reconstructed = 12 + 16 * count;
    let mut stream = 0usize;
    let mut glyf = None;
    let mut offset = 48;
    for index in 0..count {
        let (tag, original, transformed) = if version == 1 {
            let entry = 44 + index * 20;
            let tag = data
                .get(entry..entry + 4)
                .ok_or_else(|| bad("Truncated WOFF table"))?;
            let source = u32be(data, entry + 4)? as usize;
            let compressed = u32be(data, entry + 8)? as usize;
            let original = u32be(data, entry + 12)? as usize;
            if compressed > original
                || source
                    .checked_add(compressed)
                    .is_none_or(|end| end > data.len())
            {
                return Err(bad("Invalid WOFF table bounds"));
            }
            (tag, original, None)
        } else {
            let flag = *data
                .get(offset)
                .ok_or_else(|| bad("Truncated WOFF directory"))?;
            offset += 1;
            let code = flag & 63;
            let format = flag >> 6;
            let tag = if code == 63 {
                let tag = data
                    .get(offset..offset + 4)
                    .ok_or_else(|| bad("Truncated WOFF tag"))?;
                offset += 4;
                tag
            } else if code == 10 {
                &b"glyf"[..]
            } else if code == 11 {
                &b"loca"[..]
            } else {
                &b"????"[..]
            };
            let original = var128(data, &mut offset)?;
            let has_transform = if matches!(tag, b"glyf" | b"loca") {
                format == 0
            } else {
                format != 0
            };
            let transformed = if has_transform {
                Some(var128(data, &mut offset)?)
            } else {
                None
            };
            (tag, original, transformed)
        };
        reconstructed = reconstructed
            .checked_add((original + 3) & !3)
            .filter(|n| *n <= MAX_DECODED_FONT_BYTES)
            .ok_or_else(|| bad("Decoded font table budget exceeded"))?;
        if version == 2 {
            let length = transformed.unwrap_or(original);
            if tag == b"glyf" && transformed.is_some() {
                glyf = Some((stream, length));
            }
            stream = stream
                .checked_add(length)
                .filter(|n| *n <= MAX_DECODED_FONT_BYTES)
                .ok_or_else(|| bad("Font transform stream limit exceeded"))?;
        }
    }
    Ok(glyf)
}
pub(super) fn decode(data: &[u8]) -> Result<Vec<u8>, FontError> {
    if data.len() > MAX_FONT_BYTES {
        return Err(bad("Encoded font byte limit exceeded"));
    }
    let signature = data.get(..4).ok_or_else(|| bad("Truncated font"))?;
    let output = if signature == b"wOFF" {
        woff_preflight(data, 1)?;
        wuff::decompress_woff1_with_custom_z(data, &mut |compressed, expected| {
            if expected > MAX_DECODED_FONT_BYTES {
                return Err("Font zlib output limit".into());
            }
            let mut output = Vec::with_capacity(expected.min(65536));
            flate2::read::ZlibDecoder::new(compressed)
                .take(expected as u64 + 1)
                .read_to_end(&mut output)?;
            if output.len() != expected {
                return Err("Font zlib length mismatch".into());
            }
            Ok(output)
        })
        .map_err(|error| bad(&format!("Invalid WOFF: {error}")))?
    } else if signature == b"wOF2" {
        let glyf = woff_preflight(data, 2)?;
        wuff::decompress_woff2_with_custom_brotli(data, &mut |compressed, expected| {
            if expected > MAX_DECODED_FONT_BYTES {
                return Err("Font Brotli output limit".into());
            }
            let mut output = Vec::with_capacity(expected.min(65536));
            brotli::Decompressor::new(compressed, 4096)
                .take(expected as u64 + 1)
                .read_to_end(&mut output)?;
            if output.len() != expected {
                return Err("Font Brotli length mismatch".into());
            }
            if let Some((offset, length)) = glyf {
                let table = output
                    .get(offset..offset + length)
                    .ok_or("Invalid glyph stream bounds")?;
                transformed_glyf(table)?;
            }
            Ok(output)
        })
        .map_err(|error| bad(&format!("Invalid WOFF2: {error}")))?
    } else if matches!(signature, b"\0\x01\0\0" | b"OTTO" | b"true") {
        data.to_vec()
    } else {
        return Err(bad("Unknown or unsupported font container"));
    };
    if output.len() > MAX_DECODED_FONT_BYTES {
        return Err(bad("Decoded font byte limit exceeded"));
    }
    Ok(output)
}

pub(super) struct OutlineProfile {
    pub costs: Vec<u32>,
    pub bounds: Vec<[i16; 4]>,
}

pub(super) fn outline_profile(
    data: &[u8],
    index: u32,
    web: bool,
) -> Result<OutlineProfile, FontError> {
    let base = if data.get(..4) == Some(b"ttcf") {
        let count = u32be(data, 8)?;
        if web || count > 64 || index >= count {
            return Err(bad("Font collection limit exceeded"));
        }
        u32be(data, 12 + index as usize * 4)? as usize
    } else {
        if index != 0 {
            return Err(bad("Invalid font face index"));
        }
        0
    };
    if u16be(data, base + 4)? > 128 {
        return Err(bad("Font table count limit exceeded"));
    }
    let face = rustybuzz::ttf_parser::Face::parse(data, index)
        .map_err(|_| bad("Invalid OpenType font"))?;
    let count = usize::from(face.number_of_glyphs());
    if count == 0 {
        return Err(bad("Empty font"));
    }
    let table = |tag: &[u8; 4]| {
        face.raw_face()
            .table(rustybuzz::ttf_parser::Tag::from_bytes(tag))
    };
    let Some(glyf) = table(b"glyf") else {
        if web {
            return Err(bad("Downloaded CFF/CFF2 outlines are not yet supported"));
        }
        return Ok(OutlineProfile {
            costs: vec![4096; count],
            bounds: vec![],
        });
    };
    let head = table(b"head").ok_or_else(|| bad("Missing head table"))?;
    let loca = table(b"loca").ok_or_else(|| bad("Missing loca table"))?;
    let long = u16be(head, 50)? == 1;
    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..=count {
        let offset = if long {
            u32be(loca, i * 4)? as usize
        } else {
            usize::from(u16be(loca, i * 2)?) * 2
        };
        if offset > glyf.len() || offsets.last().is_some_and(|&last| last > offset) {
            return Err(bad("Invalid glyph offsets"));
        }
        offsets.push(offset);
    }
    let mut own = vec![0u32; count];
    let mut children = vec![Vec::new(); count];
    let mut bounds = vec![[0; 4]; count];
    for glyph in 0..count {
        let data = &glyf[offsets[glyph]..offsets[glyph + 1]];
        if data.is_empty() {
            continue;
        }
        let contours = u16be(data, 0)? as i16;
        for (index, value) in bounds[glyph].iter_mut().enumerate() {
            *value = u16be(data, 2 + index * 2)? as i16;
        }
        if contours >= 0 {
            let contours = contours as usize;
            if contours == 0 {
                continue;
            }
            let points = usize::from(u16be(data, 10 + (contours - 1) * 2)?) + 1;
            if points > 8192 {
                return Err(bad("Glyph outline point limit exceeded"));
            }
            own[glyph] = (points + contours) as u32;
        } else {
            let mut offset = 10;
            loop {
                if children[glyph].len() >= 64 {
                    return Err(bad("Glyph component limit exceeded"));
                }
                let flags = u16be(data, offset)?;
                let id = usize::from(u16be(data, offset + 2)?);
                if id >= count {
                    return Err(bad("Invalid component glyph"));
                }
                children[glyph].push(id);
                offset += 4 + if flags & 1 != 0 { 4 } else { 2 };
                offset += if flags & 128 != 0 {
                    8
                } else if flags & 64 != 0 {
                    4
                } else if flags & 8 != 0 {
                    2
                } else {
                    0
                };
                if offset > data.len() {
                    return Err(bad("Truncated glyph component"));
                }
                if flags & 32 == 0 {
                    break;
                }
            }
        }
    }
    let mut variations = vec![0u32; count];
    if let Some(gvar) = table(b"gvar") {
        let long = u16be(gvar, 14)? & 1 != 0;
        let glyphs = usize::from(u16be(gvar, 12)?);
        let start = u32be(gvar, 16)? as usize;
        if glyphs != count {
            return Err(bad("Invalid variable glyph count"));
        }
        for (i, cost) in variations.iter_mut().enumerate() {
            let a = if long {
                u32be(gvar, 20 + i * 4)? as usize
            } else {
                usize::from(u16be(gvar, 20 + i * 2)?) * 2
            };
            let b = if long {
                u32be(gvar, 24 + i * 4)? as usize
            } else {
                usize::from(u16be(gvar, 22 + i * 2)?) * 2
            };
            let variation = gvar
                .get(start.saturating_add(a)..start.saturating_add(b))
                .ok_or_else(|| bad("Invalid glyph variation offsets"))?;
            if variation.is_empty() {
                continue;
            }
            let tuples = u32::from(u16be(variation, 0)? & 0x0fff);
            let points = own[i] + children[i].len() as u32 + 4;
            *cost = tuples
                .saturating_mul(points)
                .saturating_add(variation.len() as u32);
            if web && *cost > 262144 {
                return Err(bad("Glyph variation work limit exceeded"));
            }
        }
    }
    let mut status = vec![0u8; count];
    let mut costs = vec![0u32; count];
    let mut points_cost = vec![0u32; count];
    let mut depths = vec![0u8; count];
    let mut total = 0usize;
    for glyph in 0..count {
        if status[glyph] != 0 {
            continue;
        }
        let mut stack = vec![(glyph, 0)];
        status[glyph] = 1;
        while let Some((node, next)) = stack.pop() {
            if next < children[node].len() {
                let child = children[node][next];
                stack.push((node, next + 1));
                match status[child] {
                    1 => return Err(bad("Cyclic glyph components")),
                    0 => {
                        if stack.len() >= 16 {
                            return Err(bad("Glyph component depth limit exceeded"));
                        }
                        status[child] = 1;
                        stack.push((child, 0));
                    }
                    _ => {}
                }
            } else {
                let depth = 1 + children[node]
                    .iter()
                    .map(|&child| depths[child])
                    .max()
                    .unwrap_or(0);
                if depth > 16 {
                    return Err(bad("Glyph component depth limit exceeded"));
                }
                depths[node] = depth;
                let points = children[node]
                    .iter()
                    .try_fold(own[node] + 1, |sum, &child| {
                        sum.checked_add(points_cost[child])
                    })
                    .filter(|n| *n <= 32768)
                    .ok_or_else(|| bad("Expanded glyph outline limit exceeded"))?;
                points_cost[node] = points;
                costs[node] = children[node]
                    .iter()
                    .fold(own[node] + 1 + variations[node], |sum, &child| {
                        sum.saturating_add(costs[child])
                    });
                if web && costs[node] > 1_000_000 {
                    return Err(bad("Expanded variable glyph work limit exceeded"));
                }
                status[node] = 2;
                total += points as usize;
                if web && total > 2_000_000 {
                    return Err(bad("Aggregate glyph outline limit exceeded"));
                }
            }
        }
    }
    Ok(OutlineProfile { costs, bounds })
}
