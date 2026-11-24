//! Locale-specific number symbols and compact decimal affixes from CLDR.
use super::{Decimal, Digits, Locale, PluralRules, plural_name};
use std::{collections::BTreeMap, sync::OnceLock};
mod data {
    include!("../../cldr/number_patterns.rs");
}
static BYTES: &[u8] = include_bytes!("../../cldr/number-patterns.bin");

type Table = BTreeMap<String, String>;
static TABLES: OnceLock<Vec<OnceLock<Table>>> = OnceLock::new();

pub(super) struct Patterns {
    table: &'static Table,
    system: String,
    digits: Option<[char; 10]>,
}

impl Patterns {
    pub(super) fn new(locale: &Locale, system: &str) -> Self {
        let mut id = locale.id.clone();
        let original = id.to_string();
        if data::LOCALES
            .binary_search_by_key(&original.as_str(), |e| e.0)
            .is_err()
        {
            icu_locale::LocaleExpander::new_extended().maximize(&mut id);
        }
        let tag = id.to_string();
        let mut key = tag.as_str();
        let index = loop {
            if let Ok(i) = data::LOCALES.binary_search_by_key(&key, |e| e.0) {
                break data::LOCALES[i].1;
            }
            key = key.rsplit_once('-').map_or("en", |(prefix, _)| prefix);
        };
        let groups =
            TABLES.get_or_init(|| (0..data::BLOCKS.len()).map(|_| OnceLock::new()).collect());
        let table = groups[index].get_or_init(|| {
            let (start, end) = data::BLOCKS[index];
            let bytes = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                &BYTES[start..end],
                128 * 1024,
            )
            .expect("generated CLDR number patterns");
            std::str::from_utf8(&bytes)
                .expect("CLDR UTF-8")
                .lines()
                .map(|row| {
                    let (k, v) = row.split_once('\t').expect("generated CLDR row");
                    (k.to_owned(), v.to_owned())
                })
                .collect()
        });
        let digits = data::DIGITS
            .binary_search_by_key(&system, |e| e.0)
            .ok()
            .map(|i| {
                let mut chars = data::DIGITS[i].1.chars();
                std::array::from_fn(|_| chars.next().expect("ten CLDR digits"))
            });
        Self {
            table,
            system: system.to_owned(),
            digits,
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        if self.system != "latn"
            && let Some(v) = self.table.get(&format!("{}:{key}", self.system))
        {
            return Some(v);
        }
        self.table.get(key).map(String::as_str)
    }

    pub(super) fn symbol<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }

    pub(super) fn shape_digits(&self, input: &mut String) {
        if self.system == "latn" {
            return;
        }
        if let Some(digits) = self.digits
            && input.bytes().any(|b| b.is_ascii_digit())
        {
            *input = input
                .chars()
                .map(|c| {
                    if c.is_ascii_digit() {
                        digits[c as usize - '0' as usize]
                    } else {
                        c
                    }
                })
                .collect();
        }
    }

    fn pattern(&self, width: &str, magnitude: i16, category: &str) -> Option<&str> {
        self.get(&format!("{width}.{magnitude:02}.{category}"))
    }

    fn scaling(&self, width: &str, magnitude: i16) -> Option<(i16, i16)> {
        // CLDR compact patterns use the last available magnitude for bigger values.
        for m in (3..=magnitude.min(30)).rev() {
            if let Some(p) = self.pattern(width, m, "other") {
                if p == "0" {
                    return None;
                }
                let zeros = p.chars().filter(|c| *c == '0').count() as i16;
                return (zeros > 0).then_some((m, m - zeros + 1));
            }
        }
        None
    }

    pub(super) fn compact<'a>(
        &'a self,
        width: &str,
        value: &mut Decimal,
        digits: &Digits,
        rules: &PluralRules,
    ) -> Option<&'a str> {
        let mag = value.nonzero_magnitude_start();
        let rounded = |scale: i16| {
            let mut d = value.clone();
            d.multiply_pow10(-scale);
            digits.round(&mut d);
            d
        };
        let (mut m, mut scale) = self.scaling(width, mag).unwrap_or((mag, 0));
        let mut d = rounded(scale);
        if d.nonzero_magnitude_start().saturating_add(scale) > mag {
            (m, scale) = self
                .scaling(width, mag.saturating_add(1))
                .unwrap_or((mag.saturating_add(1), 0));
            d = rounded(scale);
        }
        let category = plural_name(rules.category_for(&d));
        *value = d;
        if scale == 0 {
            return None;
        }
        self.pattern(width, m, category)
            .or_else(|| self.pattern(width, m, "other"))
    }
}

pub(super) fn affixes(
    pattern: &str,
) -> (
    Vec<(&'static str, String)>,
    bool,
    Vec<(&'static str, String)>,
) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut found = false;
    let mut quoted = false;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.next_if_eq(&'\'').is_none() {
                quoted = !quoted;
                continue;
            }
        } else if c == '0' && !quoted {
            found = true;
            continue;
        }
        let parts = if found { &mut after } else { &mut before };
        let kind = if c.is_whitespace() {
            "literal"
        } else {
            "compact"
        };
        if let Some((k, last)) = parts.last_mut()
            && *k == kind
        {
            String::push(last, c)
        } else {
            parts.push((kind, c.to_string()))
        }
    }
    (before, found, after)
}
