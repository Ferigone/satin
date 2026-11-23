//! Import maps belong to the document, and each module resolves requests
//! against its own base URL (the response URL for fetched modules).
use serde_json::{Map, Value};
use std::{collections::HashMap, sync::Arc};
use url::Url;

const MAX_MAP_SOURCE: usize = 128 * 1024;
const MAX_RULES: usize = 1024;
const MAX_SCOPE_COUNT: usize = 128;
const MAX_MAP_BYTES: usize = 1024 * 1024;
const MAX_RESOLUTIONS: usize = 2048;

type SpecifierMap = HashMap<String, Option<Url>>;

#[derive(Default)]
pub(super) struct ImportMap {
    imports: SpecifierMap,
    scopes: HashMap<Url, SpecifierMap>,
    integrity: HashMap<Url, Arc<str>>,
    resolved: Vec<(Arc<Url>, String, bool)>,
}
impl ImportMap {
    pub(super) fn register(&mut self, source: &str, base: &Url) -> Result<(), String> {
        if source.len() > MAX_MAP_SOURCE {
            return Err("Import map source exceeds 128 KiB".into());
        }
        let json: Value =
            serde_json::from_str(source).map_err(|e| format!("Invalid import map JSON: {e}"))?;
        let object = json.as_object().ok_or("An import map must be an object")?;
        let imports_json = object_field(object, "imports")?;
        let scopes_json = object_field(object, "scopes")?;
        let integrity_json = object_field(object, "integrity")?;
        let raw_rules = imports_json.map_or(0, Map::len)
            + integrity_json.map_or(0, Map::len)
            + scopes_json
                .into_iter()
                .flat_map(Map::values)
                .filter_map(Value::as_object)
                .map(Map::len)
                .sum::<usize>();
        if raw_rules > MAX_RULES || scopes_json.is_some_and(|scopes| scopes.len() > MAX_SCOPE_COUNT)
        {
            return Err("Import map rule budget exceeded".into());
        }
        let mut imports = normalize(imports_json, base);
        let mut scopes = HashMap::new();
        if let Some(data) = scopes_json {
            for (key, value) in data {
                let rules = value
                    .as_object()
                    .ok_or("Import-map scope contents must be an object")?;
                if let Ok(scope) = base.join(key) {
                    scopes.insert(scope, normalize(Some(rules), base));
                }
            }
        }
        let mut integrity = HashMap::new();
        if let Some(data) = integrity_json {
            for (key, value) in data {
                if let (Some(url), Some(value)) = (as_url(key, base), value.as_str()) {
                    integrity.insert(url, Arc::from(value));
                }
            }
        }
        // Earlier exact rules persist. A late map cannot reroute a specifier
        // that has already been resolved, even via a prefix or scoped rule.
        imports.retain(|key, _| {
            !self.imports.contains_key(key)
                && !self.resolved.iter().any(|(_, specifier, prefix)| {
                    key.starts_with(specifier) || key_matches(key, specifier, *prefix)
                })
        });
        for (scope, rules) in &mut scopes {
            rules.retain(|key, _| {
                !self
                    .scopes
                    .get(scope)
                    .is_some_and(|old| old.contains_key(key))
                    && !self.resolved.iter().any(|(base, specifier, prefix)| {
                        scope_matches(scope, base) && key_matches(key, specifier, *prefix)
                    })
            });
        }
        integrity.retain(|url, _| !self.integrity.contains_key(url));
        let count = self.imports.len()
            + imports.len()
            + self.integrity.len()
            + integrity.len()
            + self.scopes.values().map(HashMap::len).sum::<usize>()
            + scopes.values().map(HashMap::len).sum::<usize>();
        let scope_count = self.scopes.len()
            + scopes
                .keys()
                .filter(|key| !self.scopes.contains_key(*key))
                .count();
        let bytes = map_bytes(&self.imports)
            + map_bytes(&imports)
            + self
                .scopes
                .iter()
                .chain(&scopes)
                .map(|(k, v)| k.as_str().len() + map_bytes(v))
                .sum::<usize>()
            + self
                .integrity
                .iter()
                .chain(&integrity)
                .map(|(k, v)| k.as_str().len() + v.len())
                .sum::<usize>();
        if count > MAX_RULES || scope_count > MAX_SCOPE_COUNT || bytes > MAX_MAP_BYTES {
            return Err("Import map rule or memory budget exceeded".into());
        }
        self.imports.extend(imports);
        self.integrity.extend(integrity);
        for (key, rules) in scopes {
            self.scopes.entry(key).or_default().extend(rules);
        }
        Ok(())
    }

    pub(super) fn resolve(&mut self, specifier: &str, base: &Arc<Url>) -> Result<Url, String> {
        if self.resolved.len() >= MAX_RESOLUTIONS {
            return Err("Module specifier budget exceeded".into());
        }
        let as_url = as_url(specifier, base);
        let normalized = as_url.as_ref().map_or(specifier, Url::as_str);
        let prefix_allowed = as_url.as_ref().is_none_or(|url| {
            matches!(
                url.scheme(),
                "ftp" | "file" | "http" | "https" | "ws" | "wss"
            )
        });
        let mut scopes = self
            .scopes
            .iter()
            .filter(|(scope, _)| scope_matches(scope, base))
            .collect::<Vec<_>>();
        scopes.sort_unstable_by_key(|(scope, _)| std::cmp::Reverse(scope.as_str().len()));
        let mut result = None;
        for map in scopes
            .into_iter()
            .map(|(_, map)| map)
            .chain(std::iter::once(&self.imports))
        {
            if let Some(url) = match_specifier(map, normalized, prefix_allowed)? {
                result = Some(url);
                break;
            }
        }
        self.resolved
            .push((base.clone(), normalized.to_owned(), prefix_allowed));
        result
            .or(as_url)
            .ok_or_else(|| format!("Unmapped module specifier {:?}", preview(specifier)))
    }

    pub(super) fn integrity(&self, url: &Url) -> Option<Arc<str>> {
        self.integrity.get(url).cloned()
    }
}

fn as_url(specifier: &str, base: &Url) -> Option<Url> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        base.join(specifier).ok()
    } else {
        Url::parse(specifier).ok()
    }
}
fn object_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| format!("Import-map {field:?} must be an object"))
        })
        .transpose()
}
fn normalize(map: Option<&Map<String, Value>>, base: &Url) -> SpecifierMap {
    let mut normalized = HashMap::new();
    for (key, value) in map.into_iter().flatten() {
        if key.is_empty() {
            continue;
        }
        let key = as_url(key, base).map_or_else(|| key.clone(), |u| u.into());
        // Addresses use the same URL-like grammar as module specifiers.
        // An invalid address is a blocking entry, not a relative URL or a miss.
        let target = value
            .as_str()
            .and_then(|value| as_url(value, base))
            .filter(|url| !key.ends_with('/') || url.as_str().ends_with('/'));
        normalized.insert(key, target);
    }
    normalized
}
fn map_bytes(map: &SpecifierMap) -> usize {
    map.iter()
        .map(|(k, v)| k.len() + v.as_ref().map_or(0, |url| url.as_str().len()))
        .sum()
}
fn scope_matches(scope: &Url, base: &Url) -> bool {
    scope == base || (scope.as_str().ends_with('/') && base.as_str().starts_with(scope.as_str()))
}
fn key_matches(key: &str, specifier: &str, prefix_allowed: bool) -> bool {
    key == specifier || (prefix_allowed && key.ends_with('/') && specifier.starts_with(key))
}
fn match_specifier(
    map: &SpecifierMap,
    specifier: &str,
    prefix_allowed: bool,
) -> Result<Option<Url>, String> {
    let Some((key, target)) = map
        .iter()
        .filter(|(key, _)| key_matches(key, specifier, prefix_allowed))
        .max_by_key(|(key, _)| key.len())
    else {
        return Ok(None);
    };
    let target = target
        .as_ref()
        .ok_or_else(|| format!("Import map blocks module {:?}", preview(specifier)))?;
    if key == specifier {
        return Ok(Some(target.clone()));
    }
    let url = target
        .join(&specifier[key.len()..])
        .map_err(|_| "Invalid import-map prefix URL")?;
    if !url.as_str().starts_with(target.as_str()) {
        return Err("Module specifier escapes the import-map prefix".into());
    }
    Ok(Some(url))
}
fn preview(value: &str) -> String {
    value.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_non_url_addresses_block_fallback() {
        let base = Arc::new(Url::parse("https://example.test/app/index.html").unwrap());
        let scoped = Arc::new(base.join("scope/entry.mjs").unwrap());
        let mut map = ImportMap::default();
        map.register(
            r##"{
                "imports": {"pkg": "./global.mjs"},
                "scopes": {
                    "./scope/": {
                        "pkg": "bare.mjs",
                        "empty": "",
                        "query": "?module",
                        "fragment": "#module",
                        "relative": "./relative.mjs",
                        "absolute": "https://example.test/absolute.mjs"
                    }
                }
            }"##,
            &base,
        )
        .unwrap();
        assert_eq!(map.resolve("pkg", &base).unwrap().path(), "/app/global.mjs");
        for specifier in ["pkg", "empty", "query", "fragment"] {
            assert_eq!(
                map.resolve(specifier, &scoped).unwrap_err(),
                format!("Import map blocks module {specifier:?}")
            );
        }
        assert_eq!(
            map.resolve("relative", &scoped).unwrap().path(),
            "/app/relative.mjs"
        );
        assert_eq!(
            map.resolve("absolute", &scoped).unwrap().path(),
            "/absolute.mjs"
        );
    }

    #[test]
    fn scopes_normalization_prefix_and_already_resolved_rules() {
        let base = Arc::new(Url::parse("https://example.test/app/index.html").unwrap());
        let mut map = ImportMap::default();
        map.register(
            r#"{"imports":{"pkg/":"./lib/","./path/../entry":"/entry.js"},
            "scopes":{"./":{"pkg/":"./overrides/","pkg/bad":null}}}"#,
            &base,
        )
        .unwrap();
        assert_eq!(
            map.resolve("pkg/ok", &base).unwrap().path(),
            "/app/overrides/ok"
        );
        assert_eq!(map.resolve("./entry", &base).unwrap().path(), "/entry.js");
        assert!(map.resolve("pkg/bad", &base).is_err());
        assert!(map.resolve("pkg/%2e%2e/outside", &base).is_err());
        map.register(
            r#"{"scopes":{"./":{"pkg/ok":"/changed","new":"/added"}}}"#,
            &base,
        )
        .unwrap();
        assert_eq!(
            map.resolve("pkg/ok", &base).unwrap().path(),
            "/app/overrides/ok"
        );
        assert_eq!(map.resolve("new", &base).unwrap().path(), "/added");
    }
}
