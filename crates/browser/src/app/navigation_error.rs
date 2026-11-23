use rust_browser_engine::{LoadedPage, dom::escape, loader::NavigationRequest};
use url::Url;

pub(super) const TITLE: &str = "Page could not load";

const STYLE: &str = "
body{margin:0;background:#1b1a1c;color:#f2f1f1;font:16px/1.5 sans-serif}
main{max-width:600px;margin:0 auto;padding:32px 24px}
.label{margin:0;color:#e9b59b;font-size:13px;font-weight:bold;letter-spacing:1px}
h1{margin:6px 0 8px;font-size:32px;line-height:1.2}
p{margin:0;color:#d5d1d2}
.actions{display:flex;align-items:center;flex-wrap:wrap;gap:8px 16px;margin:20px 0 24px}
a{color:#f0c5ae}
.retry{padding:9px 16px;border-radius:6px;
       background:#edbfa8;color:#241815;font-weight:bold;text-decoration:none}
h2{margin:0 0 6px;color:#d5d1d2;font-size:14px}
pre{margin:0;padding:12px 14px;border:1px solid #4c4548;border-radius:6px;background:#262427;
    color:#e0dcde;font:14px/1.5 monospace;white-space:pre-wrap;overflow-wrap:anywhere}
";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Recovery {
    #[default]
    Unavailable,
    Retryable,
    Failed,
    FailedWithRetry,
}

impl Recovery {
    pub(super) fn for_navigation(url: &Url, request: Option<&NavigationRequest>) -> Self {
        if matches!(url.scheme(), "http" | "https" | "file")
            && request.is_none_or(|request| request.method == "GET")
        {
            Self::Retryable
        } else {
            Self::Unavailable
        }
    }

    pub(super) fn failed(self) -> Self {
        match self {
            Self::Retryable | Self::FailedWithRetry => Self::FailedWithRetry,
            _ => Self::Failed,
        }
    }

    pub(super) fn is_error_page(self) -> bool {
        matches!(self, Self::Failed | Self::FailedWithRetry)
    }

    pub(super) fn is_retry_link(self, link: Option<&str>, url: &str) -> bool {
        self == Self::FailedWithRetry && link == Some(url)
    }

    pub(super) fn page(self, url: &str, error: &str) -> LoadedPage {
        let (guidance, retry) = if self == Self::FailedWithRetry {
            (
                "Check the address or connection, then try again. You can also press F5.",
                format!(
                    r#"<a class="retry" id="satin-retry" href="{}">Try again</a>"#,
                    escape(url)
                ),
            )
        } else {
            (
                "Use Back to return to the previous page, or check the address.",
                String::new(),
            )
        };
        LoadedPage::from_html(
            url,
            &format!(
                "<!doctype html><html lang='en'><meta charset='utf-8'><title>{TITLE}</title>\
                 <style>{STYLE}</style><main><p class='label'>SATIN</p><h1>{TITLE}</h1>\
                 <p>{guidance}</p><p class='actions'>{retry}<a href='about:home'>Start page</a></p>\
                 <h2>Technical details</h2><pre id='satin-diagnostic'>{}</pre></main></html>",
                escape(error)
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_browser_engine::{Fonts, Layout, layout::Viewport};

    #[test]
    fn retry_is_hit_testable_and_diagnostics_are_literal_and_wrap() {
        let url = Url::parse("http://localhost/failure?q=1&name=two'#target").unwrap();
        let error = format!("Certificate: <untrusted>&\"literal\"\n{}", "z".repeat(760));
        let recovery = Recovery::for_navigation(&url, None).failed();
        let page = recovery.page(url.as_str(), &error);
        let node = |id| {
            page.document
                .connected_nodes()
                .find(|&node| page.document.attr(node, "id") == Some(id))
                .unwrap()
        };
        assert_eq!(page.title(), TITLE);
        assert_eq!(page.document.text(node("satin-diagnostic")), error);
        assert_eq!(
            page.document.attr(node("satin-retry"), "href"),
            Some(url.as_str())
        );
        assert!(
            !page
                .document
                .connected_nodes()
                .any(|node| page.document.tag(node) == "untrusted")
        );

        let fonts = Fonts::new().unwrap();
        let viewport = Viewport {
            width: 640.,
            height: 255.,
            ..Default::default()
        };
        let layout = Layout::build_with_viewport(&page, &fonts, viewport);
        let button = layout.rect_for(node("satin-retry")).unwrap();
        assert!(button.height >= 36. && button.y + button.height <= viewport.height);
        assert!(recovery.is_retry_link(
            layout.link_at(button.x + button.width / 2., button.y + button.height / 2.),
            url.as_str()
        ));
        assert!(layout.content_width <= viewport.width + 1.);
        assert!(layout.rect_for(node("satin-diagnostic")).unwrap().height > 60.);
    }

    #[test]
    fn non_get_navigation_does_not_offer_retry_or_claim_the_request_was_replayed() {
        let url = Url::parse("https://example.test/form").unwrap();
        let mut request = NavigationRequest::get(url.as_str()).unwrap();
        request.method = "POST".into();
        let recovery = Recovery::for_navigation(&url, Some(&request)).failed();
        let page = recovery.page(url.as_str(), "Connection closed");
        assert!(recovery.is_error_page());
        assert!(!recovery.is_retry_link(Some(url.as_str()), url.as_str()));
        assert!(!page.document.serialize().contains("satin-retry"));
    }
}
