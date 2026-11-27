//! Effective decisions for the CSP checks supported by the engine.
use crate::dom::{Document, Namespace, NodeId};
use super::ConnectPolicy;
use reqwest::header::HeaderMap;

const SOURCE_DIRECTIVES: [&[u8]; 4] = [b"worker-src", b"child-src", b"script-src", b"default-src"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocumentPolicy {
    blob_worker_allowed: bool,
    wasm_compilation_allowed: bool,
    imported_scripts: ConnectPolicy,
    require_corp: bool,
}

impl Default for DocumentPolicy {
    fn default() -> Self {
        Self {
            blob_worker_allowed: true,
            wasm_compilation_allowed: true,
            imported_scripts: ConnectPolicy::default(),
            require_corp: false,
        }
    }
}

impl DocumentPolicy {
    pub(super) fn from_headers(headers: &HeaderMap) -> Self {
        let mut policy = Self::default();
        for value in headers.get_all("content-security-policy").iter() {
            policy.restrict_serialized(value.as_bytes());
        }
        policy.require_corp = headers.get_all("cross-origin-embedder-policy").iter()
            .any(|value| value.to_str().is_ok_and(|value| {
                value.split(';').next().is_some_and(|token| token.trim() == "require-corp")
            }));
        policy
    }

    pub(crate) fn from_meta_element(document: &Document, node: NodeId) -> Option<Self> {
        if document.tag(node) != "meta"
            || document.namespace(node) != Namespace::Html
            || !document
                .attr(node, "http-equiv")?
                .eq_ignore_ascii_case("content-security-policy")
        {
            return None;
        }
        let head = document.nodes[node.0].parent?;
        if document.namespace(head) != Namespace::Html
            || document.tag(head) != "head"
            || !std::iter::successors(Some(head), |id| document.nodes[id.0].parent)
                .any(|id| id == document.root)
        {
            return None;
        }
        Some(Self::from_meta(document.attr(node, "content")?))
    }

    fn from_meta(content: &str) -> Self {
        let mut policy = Self::default();
        // Match Chromium's recovery for comma-separated meta content:
        // keywords in a later policy must not authorize an earlier one.
        policy.restrict_serialized(content.as_bytes());
        policy
    }

    fn restrict_serialized(&mut self, policies: &[u8]) {
        for serialized in policies.split(|&byte| byte == b',') {
            let (blob, wasm) = Self::parse(serialized);
            self.blob_worker_allowed &= blob;
            self.wasm_compilation_allowed &= wasm;
            self.imported_scripts.add_imported_script(serialized);
        }
    }

    pub(crate) fn restrict(&mut self, policy: &Self) {
        self.blob_worker_allowed &= policy.blob_worker_allowed;
        self.wasm_compilation_allowed &= policy.wasm_compilation_allowed;
        self.imported_scripts = self.imported_scripts.intersect(&policy.imported_scripts);
        self.require_corp |= policy.require_corp;
    }

    pub(crate) fn allows_blob(&self) -> bool {
        self.blob_worker_allowed
    }

    pub(crate) fn allows_wasm_compilation(&self) -> bool {
        self.wasm_compilation_allowed
    }

    pub(crate) fn imported_scripts(&self) -> &ConnectPolicy {
        &self.imported_scripts
    }

    pub(crate) fn requires_corp(&self) -> bool {
        self.require_corp
    }

    fn parse(policy: &[u8]) -> (bool, bool) {
        let mut directives = [const { None }; SOURCE_DIRECTIVES.len()];
        for directive in policy.split(|&byte| byte == b';') {
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
            // The first directive wins, even with empty or unrecognized sources.
            directives[index].get_or_insert(tokens);
        }
        let [worker_src, child_src, script_src, default_src] = directives;
        (
            // For HTTP documents a Blob URL matches only the explicit blob: scheme source.
            worker_src
                .or(child_src)
                .or(script_src.clone())
                .or(default_src.clone())
                .is_none_or(|mut sources| {
                    sources.any(|token| token.eq_ignore_ascii_case(b"blob:"))
                }),
            script_src.or(default_src).is_none_or(|mut sources| {
                sources.any(|token| {
                    token.eq_ignore_ascii_case(b"'wasm-unsafe-eval'")
                        || token.eq_ignore_ascii_case(b"'unsafe-eval'")
                })
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn enforced(headers: &[&'static str]) -> DocumentPolicy {
        let mut map = HeaderMap::new();
        for header in headers {
            map.append("content-security-policy", HeaderValue::from_static(header));
        }
        DocumentPolicy::from_headers(&map)
    }

    fn allows(headers: &[&'static str]) -> bool {
        enforced(headers).allows_blob()
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

    #[test]
    fn wasm_policy_uses_script_src_before_default_src() {
        for content in [
            "",
            "worker-src 'none'; child-src 'none'; script-src-elem 'none'; script-src-attr 'none'",
            "default-src 'none'; SCRIPT-SRC 'WASM-unsafe-Eval'",
            "default-src 'unsafe-eval'",
            "script-src 'unsafe-eval'; script-src 'none'",
        ] {
            assert!(
                DocumentPolicy::from_meta(content).allows_wasm_compilation(),
                "{content}"
            );
        }
        for content in [
            "default-src 'none'",
            "script-src 'self' 'unsafe-inline' https:",
            "default-src 'unsafe-eval'; script-src",
            "script-src; script-src 'wasm-unsafe-eval'",
            "script-src wasm-unsafe-eval unsafe-eval 'wasm-unsafe-eval-extra'",
            "script-src 'none'; script-src-elem 'wasm-unsafe-eval'",
            "script-src 'none', script-src 'wasm-unsafe-eval'",
            "script-src 'none' \u{00e9}; script-src 'wasm-unsafe-eval'",
        ] {
            assert!(
                !DocumentPolicy::from_meta(content).allows_wasm_compilation(),
                "{content}"
            );
        }
    }

    #[test]
    fn malformed_source_tokens_do_not_discard_a_recognized_header_directive() {
        let mut headers = HeaderMap::new();
        headers.append(
            "content-security-policy",
            HeaderValue::from_bytes(b"worker-src 'none' \xff; script-src \xff").unwrap(),
        );
        let policy = DocumentPolicy::from_headers(&headers);
        assert!(!policy.allows_blob());
        assert!(!policy.allows_wasm_compilation());
    }

    #[test]
    fn wasm_policy_intersects_all_enforcing_headers_and_meta() {
        assert!(
            enforced(&["worker-src 'none'", "script-src 'unsafe-eval'"]).allows_wasm_compilation()
        );
        for headers in [
            &["script-src 'wasm-unsafe-eval'", "default-src 'none'"][..],
            &["default-src 'none', script-src 'wasm-unsafe-eval'"],
        ] {
            assert!(!enforced(headers).allows_wasm_compilation());
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            "content-security-policy-report-only",
            HeaderValue::from_static("script-src 'none'"),
        );
        let mut policy = DocumentPolicy::from_headers(&headers);
        assert_eq!(policy, DocumentPolicy::default());
        policy.restrict(&DocumentPolicy::from_meta("worker-src 'none'"));
        assert!(policy.allows_wasm_compilation());
        policy.restrict(&DocumentPolicy::from_meta(
            "script-src 'none'; worker-src blob:",
        ));
        policy.restrict(&DocumentPolicy::from_meta(
            "script-src 'unsafe-eval'; worker-src blob:",
        ));
        assert!(!policy.allows_wasm_compilation());
        assert!(!policy.allows_blob());
    }
}
