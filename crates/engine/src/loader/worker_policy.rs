//! Enforced document policy for the supported Blob worker source scheme.
use reqwest::header::HeaderMap;

const SOURCE_DIRECTIVES: [&[u8]; 4] = [b"worker-src", b"child-src", b"script-src", b"default-src"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlobWorkerPolicy(bool);

impl Default for BlobWorkerPolicy {
    fn default() -> Self {
        Self(true)
    }
}

impl BlobWorkerPolicy {
    pub(super) fn from_headers(headers: &HeaderMap) -> Self {
        Self(
            headers
                .get_all("content-security-policy")
                .iter()
                .flat_map(|value| value.as_bytes().split(|&byte| byte == b','))
                .all(allows_blob_source),
        )
    }

    pub(crate) fn from_meta(content: &str) -> Self {
        Self(allows_blob_source(content.as_bytes()))
    }

    pub(crate) fn restrict(&mut self, policy: Self) {
        self.0 &= policy.0;
    }

    pub(crate) fn allows_blob(self) -> bool {
        self.0
    }
}

fn allows_blob_source(policy: &[u8]) -> bool {
    let mut matches = [None; SOURCE_DIRECTIVES.len()];
    for directive in policy
        .split(|&byte| byte == b';')
        .filter(|part| part.is_ascii())
    {
        let mut tokens = directive
            .split(u8::is_ascii_whitespace)
            .filter(|token| !token.is_empty());
        let Some(name) = tokens.next() else {
            continue;
        };
        let Some(index) = SOURCE_DIRECTIVES
            .iter()
            .position(|candidate| name.eq_ignore_ascii_case(candidate))
        else {
            continue;
        };
        // Keep the first occurrence of each directive. For HTTP documents a
        // Blob URL matches only the explicit blob: scheme source.
        matches[index]
            .get_or_insert_with(|| tokens.any(|token| token.eq_ignore_ascii_case(b"blob:")));
    }
    matches.into_iter().flatten().next().unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn allows(headers: &[&'static str]) -> bool {
        let mut map = HeaderMap::new();
        for header in headers {
            map.append("content-security-policy", HeaderValue::from_static(header));
        }
        BlobWorkerPolicy::from_headers(&map).allows_blob()
    }

    #[test]
    fn worker_policy_uses_the_first_directive_at_the_highest_priority() {
        assert!(allows(&["img-src 'none'"]));
        assert!(allows(&[
            "default-src 'none'; child-src 'none'; WORKER-SRC Blob:; worker-src 'none'"
        ]));
        assert!(allows(&["script-src 'none'; child-src blob:"]));
        assert!(allows(&["default-src 'none'; script-src blob:"]));
        assert!(!allows(&["default-src *"]));
        assert!(!allows(&["worker-src 'self' https: blob://*"]));
        assert!(!allows(&["worker-src; worker-src blob:"]));
    }

    #[test]
    fn worker_policy_intersects_enforced_policies() {
        assert!(allows(&["worker-src blob:", "img-src 'none'"]));
        assert!(!allows(&["worker-src blob:", "default-src 'none'"]));
        assert!(!allows(&["worker-src blob:, default-src 'none'"]));
    }
}
