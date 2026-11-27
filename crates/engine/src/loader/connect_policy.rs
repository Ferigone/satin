//! Enforced CSP source lists for network requests from classic Blob workers.
//!
//! Only expressions that can be matched confidently are accepted. Unknown
//! expressions never turn a restricted source list into an unrestricted one.
use crate::dom::{Document, Namespace, NodeId};
use reqwest::header::HeaderMap;
use std::sync::Arc;
use url::Url;

const MAX_POLICIES: usize = 32;
const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_SOURCES: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConnectPolicy {
    lists: Arc<Vec<Arc<[Source]>>>,
    blocked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    Any,
    SameOrigin,
    Scheme(String),
    Host {
        scheme: Option<String>,
        host: String,
        subdomains: bool,
        port: Port,
        path: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Port {
    Any,
    Default,
    Exact(u16),
}

impl ConnectPolicy {
    pub(super) fn from_headers(headers: &HeaderMap) -> Self {
        let mut policy = Self::default();
        for serialized in headers
            .get_all("content-security-policy")
            .iter()
            .flat_map(|value| value.as_bytes().split(|&byte| byte == b','))
        {
            policy.add(serialized);
        }
        policy
    }

    pub(crate) fn add_meta(&mut self, content: &str) {
        self.add(content.as_bytes());
    }

    pub(super) fn add_imported_script(&mut self, policy: &[u8]) {
        self.add_sources(policy, true);
    }

    pub(super) fn from_document(document: &Document) -> Self {
        let mut policy = Self::default();
        for meta in document.html_elements("meta") {
            if let Some(content) = Self::meta_content(document, meta) {
                policy.add_meta(content);
            }
        }
        policy
    }

    pub(crate) fn meta_content(document: &Document, id: NodeId) -> Option<&str> {
        let parent = document.nodes[id.0].parent?;
        (document.namespace(parent) == Namespace::Html
            && document.tag(parent) == "head"
            && std::iter::successors(Some(parent), |node| document.nodes[node.0].parent)
                .any(|node| node == document.root)
            && document
                .attr(id, "http-equiv")
                .is_some_and(|value| value.eq_ignore_ascii_case("content-security-policy")))
        .then(|| document.attr(id, "content"))
        .flatten()
    }

    pub(crate) fn intersect(&self, other: &Self) -> Self {
        if self.blocked || other.blocked {
            return Self {
                blocked: true,
                ..Self::default()
            };
        }
        if self.lists.is_empty() {
            return other.clone();
        }
        if other.lists.is_empty() {
            return self.clone();
        }
        let mut merged = self.clone();
        for list in other.lists.iter() {
            if !merged.lists.contains(list) {
                if merged.lists.len() == MAX_POLICIES {
                    return Self { blocked: true, ..Self::default() };
                }
                Arc::make_mut(&mut merged.lists).push(list.clone());
            }
        }
        merged
    }

    fn add(&mut self, policy: &[u8]) {
        self.add_sources(policy, false);
    }

    fn add_sources(&mut self, policy: &[u8], imported_script: bool) {
        if self.blocked {
            return;
        }
        if policy.len() > MAX_SOURCE_BYTES || !policy.is_ascii() {
            self.blocked = true;
            return;
        }
        let policy = std::str::from_utf8(policy).expect("ASCII CSP policy");
        let mut directive = None;
        let mut script_elem = None;
        let mut default = None;
        let primary = if imported_script { "script-src" } else { "connect-src" };
        for mut words in policy.split(';').map(str::split_ascii_whitespace) {
            let Some(name) = words.next() else {
                continue;
            };
            if name.eq_ignore_ascii_case(primary) {
                directive.get_or_insert(words);
            } else if imported_script && name.eq_ignore_ascii_case("script-src-elem") {
                script_elem.get_or_insert(words);
            } else if name.eq_ignore_ascii_case("default-src") {
                default.get_or_insert(words);
            }
        }
        let Some(words) = script_elem.or(directive).or(default) else {
            return;
        };
        // importScripts is non-parser-inserted and has no nonce or integrity
        // metadata. With strict-dynamic the source list is not used.
        if imported_script && words.clone().any(|word| word.eq_ignore_ascii_case("'strict-dynamic'")) {
            return;
        }
        if self.lists.len() == MAX_POLICIES {
            self.blocked = true;
            return;
        }
        let mut sources = Vec::new();
        for (index, word) in words.enumerate() {
            if index == MAX_SOURCES {
                self.blocked = true;
                return;
            }
            if let Some(source) = Source::parse(word) {
                sources.push(source);
            }
        }
        if sources.is_empty() {
            self.blocked = true;
        } else {
            Arc::make_mut(&mut self.lists).push(sources.into());
        }
    }

    pub(crate) fn allows(&self, client: &Url, target: &Url, redirected: bool) -> bool {
        !self.blocked
            && self.lists.iter().all(|sources| {
                sources
                    .iter()
                    .any(|source| source.matches(client, target, redirected))
            })
    }
}

fn valid_scheme(scheme: &str) -> bool {
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn valid_path(path: &str) -> bool {
    let path = path.as_bytes();
    path.iter().enumerate().all(|(index, &byte)| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
                    | b'/'
            )
            || (byte == b'%'
                && path
                    .get(index + 1..index + 3)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit)))
    })
}

impl Source {
    fn parse(word: &str) -> Option<Self> {
        if word == "*" {
            return Some(Self::Any);
        }
        if word.eq_ignore_ascii_case("'self'") {
            return Some(Self::SameOrigin);
        }
        if let Some(scheme) = word.strip_suffix(':')
            && valid_scheme(scheme)
        {
            return Some(Self::Scheme(scheme.to_ascii_lowercase()));
        }
        let (scheme, rest) = if let Some((scheme, rest)) = word.split_once("://") {
            if !valid_scheme(scheme) {
                return None;
            }
            (Some(scheme.to_ascii_lowercase()), rest)
        } else {
            (None, word)
        };
        let (host_port, path) = rest
            .find('/')
            .map_or((rest, None), |i| (&rest[..i], Some(&rest[i..])));
        if path.is_some_and(|path| !valid_path(path)) {
            return None;
        }
        let end = host_port.find(':').unwrap_or(host_port.len());
        let (host, port) = host_port.split_at(end);
        let port = match port {
            "" => Port::Default,
            ":*" => Port::Any,
            port => {
                let digits = port.strip_prefix(':')?;
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                Port::Exact(digits.parse().ok()?)
            }
        };
        let (host, subdomains) = host
            .strip_prefix("*.")
            .map_or((host, false), |host| (host, true));
        if !(host == "*" && !subdomains
            || host.split('.').all(|label| {
                !label.is_empty()
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }))
        {
            return None;
        }
        Some(Self::Host {
            scheme,
            host: host.to_ascii_lowercase(),
            subdomains,
            port,
            path: path.map(str::to_owned),
        })
    }

    fn matches(&self, client: &Url, target: &Url, redirected: bool) -> bool {
        let scheme_matches = |scheme: &str| {
            scheme == target.scheme() || (scheme == "http" && target.scheme() == "https")
        };
        match self {
            Self::Any => matches!(target.scheme(), "http" | "https"),
            Self::SameOrigin => {
                matches!(client.scheme(), "http" | "https") && client.origin() == target.origin()
            }
            Self::Scheme(scheme) => scheme_matches(scheme),
            Self::Host {
                scheme,
                host,
                subdomains,
                port,
                path,
            } => {
                scheme_matches(scheme.as_deref().unwrap_or(client.scheme()))
                    && target.host_str().is_some_and(|candidate| {
                        if *subdomains {
                            candidate
                                .strip_suffix(host)
                                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
                        } else {
                            host == "*" || host == candidate
                        }
                    })
                    && match port {
                        Port::Any => true,
                        Port::Default => target.port().is_none(),
                        Port::Exact(port) => target.port_or_known_default() == Some(*port),
                    }
                    && (redirected
                        || path.as_ref().is_none_or(|path| {
                            if path.ends_with('/') {
                                target.path().starts_with(path)
                            } else {
                                target.path() == path
                            }
                        }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn only_enforced_connect_lists_intersect_with_the_first_directive_winning() {
        let mut headers = HeaderMap::new();
        headers.append(
            "content-security-policy",
            HeaderValue::from_static("default-src 'none'; CONNECT-SRC 'self'; connect-src *"),
        );
        headers.append(
            "content-security-policy",
            HeaderValue::from_static("worker-src blob:, connect-src https:"),
        );
        headers.append(
            "content-security-policy-report-only",
            HeaderValue::from_static("connect-src 'none'"),
        );
        let header = ConnectPolicy::from_headers(&headers);
        let client = Url::parse("https://a.test/start").unwrap();
        assert!(header.allows(&client, &client, false));
        assert!(!header.allows(&client, &Url::parse("https://b.test/").unwrap(), false));
        let mut meta = ConnectPolicy::default();
        meta.add_meta("default-src 'none'");
        assert!(!header.intersect(&meta).allows(&client, &client, false));
        assert!(header.allows(&client, &client, false));
    }

    #[test]
    fn host_path_and_port_rules_do_not_broaden_an_unrecognized_source() {
        let mut policy = ConnectPolicy::default();
        policy.add_meta("connect-src https://*.a.test:8443/shared/ 'unsafe-inline'");
        let client = Url::parse("https://a.test/start").unwrap();
        let allows =
            |url: &str, redirected| policy.allows(&client, &Url::parse(url).unwrap(), redirected);
        assert!(allows("https://child.a.test:8443/shared/file", false));
        assert!(!allows("https://child.a.test:8443/share/file", false));
        assert!(allows("https://child.a.test:8443/share/file", true));
        assert!(!allows("https://a.test:8443/shared/file", false));
        assert!(!allows("https://child.a.test/shared/file", true));
        assert!(!allows(
            "https://child.a.test.bad.test:8443/shared/file",
            true
        ));
        assert!(!allows("data:application/wasm,bytes", false));
        let mut invalid = ConnectPolicy::default();
        invalid.add_meta("connect-src https://a.test:+443 https://a.test/%zz/");
        assert!(!invalid.allows(&client, &client, true));
    }
}
