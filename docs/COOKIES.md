# Cookies and network sessions in alpha.4

A Rust `CookieStore` serves navigation, subresources, Fetch, and `document.cookie` within each profile. It owns cookie handling for the HTTP and TLS transport. The store releases its lock before network operations and JavaScript calls.

The store records name and value bytes, domain, path, flags, creation and expiration times, and a partition key. HTTP preserves raw bytes. `document.cookie` encodes and decodes using UTF-8. Matching covers host-only and `Domain` cookies, path boundaries, `Secure`, `HttpOnly`, the `__Secure-`, `__Host-`, `__Http-`, and `__Host-Http-` prefixes, and schemeful SameSite. An absent SameSite attribute defaults to Lax with a two-minute exception for recently created cookies during POST navigation. Cross-site redirect chains retain their tainted context on return to the original site.

A pinned Public Suffix List resolves registrable domains and sites and rejects public suffixes. It includes ICANN and private entries, wildcards, and exceptions. `Partitioned` requires `Secure` and keys cookie data by the client's top-level site. This stage handles top-level documents. Full iframe integration remains open. [Public Suffix List data, license, and hash](../crates/engine/data/publicsuffix/README.md)

Fetch applies `omit`, `same-origin`, and `include` credentials modes to cookie reads and writes. Preflight requests run without cookies. `same-origin` stops sending cookies after a redirect taints the response. The network layer processes `Set-Cookie` before CORS checks and filters it from JavaScript-visible headers. Once cancellation or a deadline has been observed, late responses are discarded for cookie storage.

| Budget | Limit |
| --- | ---: |
| Name and value per cookie | 4,096 bytes |
| Value per attribute | 1,024 bytes |
| Store | 3,000 entries and 16 MiB of estimated data |
| Domain and partition | 180 entries |
| Maximum lifetime | 400 days |

The store removes expired entries and evicts excess entries when writing. Reload validates all budgets, prefixes, keys, and canonical domains before atomically replacing the active store.

Persistent cookies are written atomically to `Cookies.v1.dpapi` under the profile lock. Windows DPAPI uses the current account and requires no interactive prompt. Session cookies expire when the app exits. Alpha.4 uses its own cookie file, which alpha.3 ignores on rollback. Headless mode uses ephemeral storage by default. An explicit `--profile` uses the same lock and protection as the desktop browser. Tests use isolated profiles.

Remaining work covers site-data controls, full third-party and permission policies, HTTP authentication, expiration adjustment using the server's `Date` header, `Priority`, automatic suffix list updates, and iframe integration. Content-process isolation and binding cookie storage to a specific executable require separate mechanisms. Subresource loading and Fetch block active mixed content and use the actual document URL to check file access. A mutable base URL only affects URL resolution.

Coverage includes the cookie store, `cookies_transport`, the app profile, and a local synthetic form and session scenario. The scenario checks POST/303, HttpOnly, Fetch, and reopening the profile. [Form and session verifier](../tools/verify-forms.mjs)

The cookie and storage behavior draws on HTTP WG, Fetch, HTML, the Public Suffix List, and Windows API documentation. [RFC 6265bis draft](https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html) · [Fetch cookies](https://fetch.spec.whatwg.org/#cookies) · [HTML `document.cookie`](https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie) · [Public Suffix List](https://publicsuffix.org/list/) · [DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)
