//! Browser-owned cookie storage. HTTP bytes are retained without Unicode loss;
//! document.cookie uses UTF-8 at its API boundary. No network or disk I/O here.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

const COOKIE_BYTES: usize = 4096;
const STORE_BYTES: usize = 16 * 1024 * 1024;
const STORE_COUNT: usize = 3000;
const DOMAIN_COUNT: usize = 180;
const MAX_AGE: i64 = 400 * 24 * 60 * 60;
pub const MAX_COOKIE_FILE_BYTES: usize = 40 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Site {
    scheme: String,
    domain: String,
}
impl Site {
    pub fn from_url(url: &Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = host(url)?;
        Some(Self {
            scheme: url.scheme().into(),
            domain: registrable_domain(&host),
        })
    }
}

#[derive(Clone, Debug)]
pub struct CookieContext {
    pub site_for_cookies: Option<Site>,
    pub top_level_navigation: bool,
    pub method: String,
    crossed_site: bool,
}
impl CookieContext {
    pub fn subresource(client: &Url) -> Self {
        Self {
            site_for_cookies: Site::from_url(client),
            top_level_navigation: false,
            method: "GET".into(),
            crossed_site: false,
        }
    }
    pub fn navigation(initiator: Option<&Url>, target: &Url, method: &str) -> Self {
        let site = Site::from_url(initiator.unwrap_or(target));
        let crossed_site = site.is_none() || site != Site::from_url(target);
        Self {
            site_for_cookies: site,
            top_level_navigation: true,
            method: method.into(),
            crossed_site,
        }
    }
    pub fn redirect_to(&mut self, target: &Url, method: &str) {
        self.crossed_site |= !self.same_site(target);
        self.method = method.into();
    }
    fn same_site(&self, target: &Url) -> bool {
        !self.crossed_site
            && self.site_for_cookies.is_some()
            && self.site_for_cookies == Site::from_url(target)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum SameSite {
    Default,
    Strict,
    Lax,
    None,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cookie {
    #[serde(with = "wire_bytes")]
    name: Vec<u8>,
    #[serde(with = "wire_bytes")]
    value: Vec<u8>,
    domain: String,
    path: String,
    host_only: bool,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
    expires: Option<i64>,
    created: i64,
    sequence: u64,
    accessed: u64,
    partition: Option<Site>,
}
impl Cookie {
    fn bytes(&self) -> usize {
        self.name.len()
            + self.value.len()
            + self.domain.len()
            + self.path.len()
            + 192
            + self
                .partition
                .as_ref()
                .map_or(0, |site| site.scheme.len() + site.domain.len())
    }
    fn same_key(&self, other: &Self) -> bool {
        self.name == other.name
            && self.domain == other.domain
            && self.path == other.path
            && self.host_only == other.host_only
            && self.partition == other.partition
    }
}
#[derive(Debug, Default)]
struct Jar {
    cookies: Vec<Cookie>,
    revision: u64,
    sequence: u64,
}
#[derive(Clone, Debug, Default)]
pub struct CookieStore(Arc<Mutex<Jar>>);

impl CookieStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn revision(&self) -> u64 {
        self.0.lock().unwrap().revision
    }
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().cookies.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn clear(&self) {
        let mut jar = self.0.lock().unwrap();
        if !jar.cookies.is_empty() {
            jar.cookies.clear();
            jar.revision = jar.revision.saturating_add(1);
        }
    }
    pub fn receive(&self, url: &Url, bytes: &[u8], context: &CookieContext) -> bool {
        self.receive_at(url, bytes, context, false, now())
    }
    pub fn set_document_cookie(&self, url: &Url, value: &str) -> bool {
        self.receive_at(
            url,
            value.as_bytes(),
            &CookieContext::subresource(url),
            true,
            now(),
        )
    }
    pub fn header(&self, url: &Url, context: &CookieContext) -> Vec<u8> {
        self.retrieve(url, context, false, now())
    }
    pub fn document_cookie(&self, url: &Url) -> String {
        String::from_utf8_lossy(&self.retrieve(url, &CookieContext::subresource(url), true, now()))
            .into_owned()
    }
    fn receive_at(
        &self,
        url: &Url,
        bytes: &[u8],
        context: &CookieContext,
        script: bool,
        clock: i64,
    ) -> bool {
        let Some(mut cookie) = parse_cookie(url, bytes, context, clock) else {
            return false;
        };
        if script && cookie.http_only {
            return false;
        }
        if cookie.same_site != SameSite::None
            && !context.same_site(url)
            && (script || !context.top_level_navigation)
        {
            return false;
        }
        let mut jar = self.0.lock().unwrap();
        purge(&mut jar, clock);
        if !secure_origin(url)
            && !cookie.secure
            && jar.cookies.iter().any(|old| {
                old.secure
                    && old.name == cookie.name
                    && old.partition == cookie.partition
                    && (domain_match(&old.domain, &cookie.domain)
                        || domain_match(&cookie.domain, &old.domain))
                    && path_match(&cookie.path, &old.path)
            })
        {
            return false;
        }
        let old = jar.cookies.iter().position(|old| old.same_key(&cookie));
        if let Some(index) = old {
            if script && jar.cookies[index].http_only {
                return false;
            }
            cookie.created = jar.cookies[index].created;
            cookie.sequence = jar.cookies[index].sequence;
            jar.cookies.remove(index);
        } else {
            jar.sequence = jar.sequence.saturating_add(1);
            cookie.sequence = jar.sequence;
        }
        if cookie.expires.is_none_or(|expiry| expiry > clock) {
            jar.sequence = jar.sequence.saturating_add(1);
            cookie.accessed = jar.sequence;
            jar.cookies.push(cookie);
            enforce_limits(&mut jar);
        }
        jar.revision = jar.revision.saturating_add(1);
        true
    }
    fn retrieve(&self, url: &Url, context: &CookieContext, script: bool, clock: i64) -> Vec<u8> {
        let Some(host) = host(url) else { return vec![] };
        let mut jar = self.0.lock().unwrap();
        purge(&mut jar, clock);
        let mut selected = jar
            .cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                (if c.host_only {
                    c.domain == host
                } else {
                    domain_match(&host, &c.domain) && !is_public_suffix(&c.domain)
                }) && path_match(url.path(), &c.path)
                    && (!c.secure || secure_origin(url))
                    && (!script || !c.http_only)
                    && c.partition
                        .as_ref()
                        .is_none_or(|site| Some(site) == context.site_for_cookies.as_ref())
                    && (context.same_site(url)
                        || c.same_site == SameSite::None
                        || (!script
                            && context.top_level_navigation
                            && c.same_site != SameSite::Strict
                            && (matches!(
                                context.method.as_str(),
                                "GET" | "HEAD" | "OPTIONS" | "TRACE"
                            ) || (c.same_site == SameSite::Default
                                && (0..=120).contains(&clock.saturating_sub(c.created))))))
            })
            .map(|(i, c)| (i, c.path.len(), c.sequence))
            .collect::<Vec<_>>();
        selected.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        let size = selected
            .iter()
            .map(|&(i, _, _)| jar.cookies[i].name.len() + jar.cookies[i].value.len() + 3)
            .sum();
        let mut bytes = Vec::with_capacity(size);
        for (i, _, _) in selected {
            if !bytes.is_empty() {
                bytes.extend_from_slice(b"; ");
            }
            jar.sequence = jar.sequence.saturating_add(1);
            let sequence = jar.sequence;
            let cookie = &mut jar.cookies[i];
            cookie.accessed = sequence;
            if !cookie.name.is_empty() {
                bytes.extend_from_slice(&cookie.name);
                bytes.push(b'=');
            }
            bytes.extend_from_slice(&cookie.value);
        }
        bytes
    }
    /// Persistent cookies only: session cookies end with this application session.
    /// The caller protects and atomically writes these bytes under the profile lock.
    pub fn export_persistent(&self) -> Result<Vec<u8>, String> {
        #[derive(Serialize)]
        struct File<'a> {
            schema: u32,
            cookies: Vec<&'a Cookie>,
        }
        let mut jar = self.0.lock().unwrap();
        purge(&mut jar, now());
        serde_json::to_vec(&File {
            schema: 1,
            cookies: jar.cookies.iter().filter(|c| c.expires.is_some()).collect(),
        })
        .map_err(|error| error.to_string())
    }
    pub fn restore_persistent(&self, bytes: &[u8]) -> Result<(), String> {
        #[derive(Deserialize)]
        struct File {
            schema: u32,
            cookies: Vec<Cookie>,
        }
        if bytes.len() > MAX_COOKIE_FILE_BYTES {
            return Err("Cookie file exceeds its size budget".into());
        }
        let file: File =
            serde_json::from_slice(bytes).map_err(|_| "Invalid cookie file".to_string())?;
        if file.schema != 1 || file.cookies.len() > STORE_COUNT {
            return Err("Unsupported cookie file schema/count".into());
        }
        let clock = now();
        let mut candidate = Jar::default();
        for mut c in file.cookies {
            if !stored_valid(&c) {
                return Err("Invalid stored cookie".into());
            }
            if c.expires.is_some_and(|expiry| expiry > clock) {
                c.expires = c
                    .expires
                    .map(|expiry| expiry.min(clock.saturating_add(MAX_AGE)));
                if candidate.cookies.iter().any(|old| old.same_key(&c)) {
                    return Err("Duplicate stored cookie".into());
                }
                candidate.sequence = candidate.sequence.max(c.sequence).max(c.accessed);
                candidate.cookies.push(c);
            }
        }
        if candidate.cookies.iter().map(Cookie::bytes).sum::<usize>() > STORE_BYTES {
            return Err("Stored cookie bytes exceed budget".into());
        }
        let mut domain_counts = BTreeMap::new();
        for cookie in &candidate.cookies {
            let count = domain_counts
                .entry((&cookie.domain, &cookie.partition))
                .or_insert(0usize);
            *count += 1;
            if *count > DOMAIN_COUNT {
                return Err("Stored cookie domain count exceeds budget".into());
            }
        }
        let mut jar = self.0.lock().unwrap();
        candidate.revision = jar.revision.saturating_add(1);
        *jar = candidate;
        Ok(())
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}
fn purge(jar: &mut Jar, clock: i64) {
    let before = jar.cookies.len();
    jar.cookies
        .retain(|c| c.expires.is_none_or(|expiry| expiry > clock));
    if before != jar.cookies.len() {
        jar.revision = jar.revision.saturating_add(1);
    }
}
fn enforce_limits(jar: &mut Jar) {
    loop {
        let mut counts = BTreeMap::new();
        for cookie in &jar.cookies {
            *counts
                .entry((&cookie.domain, &cookie.partition))
                .or_insert(0usize) += 1;
        }
        let over_domain = counts
            .into_iter()
            .find(|(_, count)| *count > DOMAIN_COUNT)
            .map(|((domain, partition), _)| (domain.clone(), partition.clone()));
        let over = jar.cookies.len() > STORE_COUNT
            || jar.cookies.iter().map(Cookie::bytes).sum::<usize>() > STORE_BYTES;
        if over_domain.is_none() && !over {
            break;
        }
        let victim = jar
            .cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                over_domain
                    .as_ref()
                    .is_none_or(|(domain, key)| c.domain == *domain && c.partition == *key)
            })
            .min_by_key(|(_, c)| (c.secure, c.accessed))
            .map(|(i, _)| i)
            .unwrap();
        jar.cookies.remove(victim);
    }
}
fn host(url: &Url) -> Option<String> {
    matches!(url.scheme(), "http" | "https")
        .then(|| url.host_str().map(str::to_ascii_lowercase))
        .flatten()
}
fn is_ip(value: &str) -> bool {
    value
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok()
}
fn domain_match(host: &str, domain: &str) -> bool {
    host == domain
        || (!is_ip(host)
            && host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.')))
}
fn path_match(request: &str, path: &str) -> bool {
    request == path
        || request
            .strip_prefix(path)
            .is_some_and(|rest| path.ends_with('/') || rest.starts_with('/'))
}
pub(crate) fn secure_origin(url: &Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host.eq_ignore_ascii_case("localhost.")
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }))
}
fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !matches!(b, b' ' | b'\t'))
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}
fn control(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|&b| b <= 8 || (10..=31).contains(&b) || b == 127)
}
fn wire_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}
fn parse_cookie(url: &Url, bytes: &[u8], context: &CookieContext, clock: i64) -> Option<Cookie> {
    let request_host = host(url)?;
    if bytes.len() > 64 * 1024 || control(bytes) {
        return None;
    }
    let mut parts = bytes.split(|&b| b == b';');
    let pair = parts.next()?;
    let (name, value) = if let Some(index) = pair.iter().position(|&b| b == b'=') {
        (&pair[..index], &pair[index + 1..])
    } else {
        (&[][..], pair)
    };
    let (name, value) = (trim(name), trim(value));
    if name.len() + value.len() > COOKIE_BYTES || name.is_empty() && value.is_empty() {
        return None;
    }
    let mut domain = None;
    let mut path = None;
    let mut secure = false;
    let mut http_only = false;
    let mut same_site = SameSite::Default;
    let mut expiry = None;
    let mut age = None;
    let mut partitioned = false;
    for part in parts {
        let index = part.iter().position(|&b| b == b'=').unwrap_or(part.len());
        let key = trim(&part[..index]);
        let value = trim(part.get(index + 1..).unwrap_or_default());
        if value.len() > 1024 {
            continue;
        }
        if key.eq_ignore_ascii_case(b"domain") {
            domain = Some(value);
        } else if key.eq_ignore_ascii_case(b"path") {
            path = Some(value);
        } else if key.eq_ignore_ascii_case(b"secure") {
            secure = true;
        } else if key.eq_ignore_ascii_case(b"httponly") {
            http_only = true;
        } else if key.eq_ignore_ascii_case(b"partitioned") {
            partitioned = true;
        } else if key.eq_ignore_ascii_case(b"expires") {
            if let Some(value) = cookie_date(value) {
                expiry = Some(value.min(clock.saturating_add(MAX_AGE)));
            }
        } else if key.eq_ignore_ascii_case(b"max-age") {
            if let Some(value) = max_age(value) {
                age = Some(value);
            }
        } else if key.eq_ignore_ascii_case(b"samesite") {
            same_site = if value.eq_ignore_ascii_case(b"strict") {
                SameSite::Strict
            } else if value.eq_ignore_ascii_case(b"lax") {
                SameSite::Lax
            } else if value.eq_ignore_ascii_case(b"none") {
                SameSite::None
            } else {
                SameSite::Default
            };
        }
    }
    let had_domain = domain.is_some_and(|value| !value.is_empty());
    let mut domain = domain.unwrap_or_default();
    if domain.first() == Some(&b'.') {
        domain = &domain[1..];
    }
    if !domain.is_ascii() {
        return None;
    }
    let mut domain = String::from_utf8(domain.to_vec())
        .ok()?
        .to_ascii_lowercase();
    if !domain.is_empty() && is_public_suffix(&domain) {
        if domain != request_host {
            return None;
        }
        domain.clear();
    }
    let host_only = domain.is_empty();
    if host_only {
        domain = request_host.clone();
    } else if !domain_match(&request_host, &domain) {
        return None;
    }
    let default_path = url
        .path()
        .rfind('/')
        .filter(|&index| index > 0)
        .map(|index| &url.path()[..index])
        .unwrap_or("/");
    let explicit_root_path = path == Some(b"/".as_slice());
    let path = path
        .filter(|value| value.first() == Some(&b'/'))
        .map(wire_string)
        .unwrap_or_else(|| default_path.into());
    if secure && !secure_origin(url) || same_site == SameSite::None && !secure {
        return None;
    }
    let lower = name.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    if (lower.starts_with(b"__secure-")
        || lower.starts_with(b"__host-")
        || lower.starts_with(b"__http-"))
        && !secure
    {
        return None;
    }
    if lower.starts_with(b"__host-") && (!host_only || had_domain || !explicit_root_path) {
        return None;
    }
    if (lower.starts_with(b"__http-") || lower.starts_with(b"__host-http-")) && !http_only {
        return None;
    }
    if name.is_empty()
        && [b"__secure-".as_slice(), b"__host-", b"__http-"]
            .iter()
            .any(|prefix| {
                value
                    .get(..prefix.len())
                    .is_some_and(|v| v.eq_ignore_ascii_case(prefix))
            })
    {
        return None;
    }
    let partition = if partitioned {
        if !secure {
            return None;
        }
        Some(context.site_for_cookies.clone()?)
    } else {
        None
    };
    let expires = age
        .map(|age| {
            if age <= 0 {
                0
            } else {
                clock.saturating_add(age.min(MAX_AGE))
            }
        })
        .or(expiry);
    Some(Cookie {
        name: name.to_vec(),
        value: value.to_vec(),
        domain,
        path,
        host_only,
        secure,
        http_only,
        same_site,
        expires,
        created: clock,
        sequence: 0,
        accessed: 0,
        partition,
    })
}
fn max_age(bytes: &[u8]) -> Option<i64> {
    let negative = bytes.first() == Some(&b'-');
    let digits = if negative { &bytes[1..] } else { bytes };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = digits.iter().fold(0i64, |n, b| {
        n.saturating_mul(10).saturating_add((b - b'0') as i64)
    });
    Some(if negative { -value } else { value })
}
fn stored_valid(c: &Cookie) -> bool {
    let lower = c
        .name
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let nameless_prefix = c.name.is_empty()
        && [b"__secure-".as_slice(), b"__host-", b"__http-"]
            .iter()
            .any(|prefix| {
                c.value
                    .get(..prefix.len())
                    .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
            });
    let prefix_valid = !nameless_prefix
        && (!(lower.starts_with(b"__secure-")
            || lower.starts_with(b"__host-")
            || lower.starts_with(b"__http-"))
            || c.secure)
        && (!lower.starts_with(b"__host-") || (c.host_only && c.path == "/"))
        && (!(lower.starts_with(b"__http-") || lower.starts_with(b"__host-http-")) || c.http_only);
    let valid_pair = c.name.len() + c.value.len() <= COOKIE_BYTES
        && !(c.name.is_empty() && c.value.is_empty())
        && !control(&c.name)
        && !control(&c.value)
        && !c.name.iter().any(|b| matches!(b, b';' | b'='))
        && !c.value.contains(&b';');
    let domain_valid = !c.domain.is_empty()
        && c.domain.len() <= 253
        && c.domain.is_ascii()
        && !c.domain.starts_with('.')
        && (is_ip(&c.domain)
            || url::Host::parse(&c.domain)
                .ok()
                .is_some_and(|host| host.to_string() == c.domain))
        && (c.host_only || !is_public_suffix(&c.domain));
    valid_pair
        && domain_valid
        && prefix_valid
        && c.path.starts_with('/')
        && c.path.len() <= 2048
        && !control(c.path.as_bytes())
        && (c.same_site != SameSite::None || c.secure)
        && c.partition.as_ref().is_none_or(|site| {
            c.secure
                && matches!(site.scheme.as_str(), "http" | "https")
                && !site.domain.is_empty()
                && site.domain.len() <= 253
                && site.domain.is_ascii()
                && url::Host::parse(&site.domain)
                    .ok()
                    .is_some_and(|host| host.to_string() == site.domain)
                && registrable_domain(&site.domain) == site.domain
        })
}
fn cookie_date(bytes: &[u8]) -> Option<i64> {
    let tokens = bytes
        .split(|&b| {
            b == 9
                || (0x20..=0x2f).contains(&b)
                || (0x3b..=0x40).contains(&b)
                || (0x5b..=0x60).contains(&b)
                || (0x7b..=0x7e).contains(&b)
        })
        .filter(|t| !t.is_empty());
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in tokens {
        if time.is_none() {
            let parts = token.split(|&b| b == b':').collect::<Vec<_>>();
            if parts.len() == 3
                && parts[0].iter().all(u8::is_ascii_digit)
                && parts[1].iter().all(u8::is_ascii_digit)
                && let (Some(h), Some(m), Some(s)) = (
                    decimal_prefix(parts[0], 1, 2),
                    decimal_prefix(parts[1], 1, 2),
                    decimal_prefix(parts[2], 1, 2),
                )
            {
                time = Some((h, m, s));
                continue;
            }
        }
        if day.is_none()
            && let Some(value) = decimal_prefix(token, 1, 2)
        {
            day = Some(value);
            continue;
        }
        if month.is_none() && token.len() >= 3 {
            month = [
                b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct",
                b"nov", b"dec",
            ]
            .iter()
            .position(|name| token[..3].eq_ignore_ascii_case(*name))
            .map(|m| m + 1);
            if month.is_some() {
                continue;
            }
        }
        if year.is_none() {
            year = decimal_prefix(token, 2, 4);
        }
    }
    let (hour, minute, second) = time?;
    let (day, month, mut year) = (day?, month?, year?);
    if (70..=99).contains(&year) {
        year += 1900;
    } else if year <= 69 {
        year += 2000;
    }
    if year < 1601 || hour > 23 || minute > 59 || second > 59 || day == 0 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let lengths = [
        31,
        28 + usize::from(leap),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day > lengths[month - 1] {
        return None;
    }
    let y = year as i64 - 1;
    let days = 365 * (year as i64 - 1970) + (y / 4 - y / 100 + y / 400)
        - (1969 / 4 - 1969 / 100 + 1969 / 400)
        + lengths[..month - 1].iter().sum::<usize>() as i64
        + day as i64
        - 1;
    Some(days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64)
}
fn decimal_prefix(bytes: &[u8], min: usize, max: usize) -> Option<usize> {
    let n = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if !(min..=max).contains(&n) {
        return None;
    }
    Some(
        bytes[..n]
            .iter()
            .fold(0usize, |n, b| n * 10 + (b - b'0') as usize),
    )
}

struct Suffixes {
    exact: BTreeSet<String>,
    wildcard: BTreeSet<String>,
    exceptions: BTreeSet<String>,
}
fn suffixes() -> &'static Suffixes {
    static PSL: OnceLock<Suffixes> = OnceLock::new();
    PSL.get_or_init(|| {
        let mut result = Suffixes {
            exact: BTreeSet::new(),
            wildcard: BTreeSet::new(),
            exceptions: BTreeSet::new(),
        };
        for line in include_str!("../data/publicsuffix/public_suffix_list.dat")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
        {
            let (target, rule) = if let Some(rule) = line.strip_prefix('!') {
                (&mut result.exceptions, rule)
            } else if let Some(rule) = line.strip_prefix("*.") {
                (&mut result.wildcard, rule)
            } else {
                (&mut result.exact, line)
            };
            if let Ok(url::Host::Domain(domain)) = url::Host::parse(rule) {
                target.insert(domain);
            }
        }
        result
    })
}
fn suffix_labels(domain: &str) -> usize {
    if is_ip(domain) {
        return 0;
    }
    let rules = suffixes();
    let labels = domain.split('.').collect::<Vec<_>>();
    let mut best = 1;
    for index in 0..labels.len() {
        let suffix = labels[index..].join(".");
        if rules.exceptions.contains(&suffix) {
            return labels.len() - index - 1;
        }
        if rules.exact.contains(&suffix) {
            best = best.max(labels.len() - index);
        }
        if index > 0 && rules.wildcard.contains(&suffix) {
            best = best.max(labels.len() - index + 1);
        }
    }
    best
}
fn is_public_suffix(domain: &str) -> bool {
    !is_ip(domain) && domain.split('.').count() <= suffix_labels(domain)
}
fn registrable_domain(domain: &str) -> String {
    if is_ip(domain) {
        return domain.into();
    }
    let labels = domain.split('.').collect::<Vec<_>>();
    labels[labels.len().saturating_sub(suffix_labels(domain) + 1)..].join(".")
}
mod wire_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::wire_string(value))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        String::deserialize(deserializer)?
            .chars()
            .map(|c| {
                u8::try_from(c as u32).map_err(|_| serde::de::Error::custom("non-byte cookie data"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }
    fn put(store: &CookieStore, address: &str, value: &[u8], clock: i64) -> bool {
        let address = url(address);
        store.receive_at(
            &address,
            value,
            &CookieContext::subresource(&address),
            false,
            clock,
        )
    }
    fn get(store: &CookieStore, address: &str, clock: i64) -> String {
        let address = url(address);
        wire_string(&store.retrieve(
            &address,
            &CookieContext::subresource(&address),
            false,
            clock,
        ))
    }
    #[test]
    fn host_only_domain_path_and_order_are_independent() {
        let store = CookieStore::new();
        assert!(put(
            &store,
            "https://a.example.com/shop/page",
            b"id=root; Path=/",
            100
        ));
        assert!(put(
            &store,
            "https://a.example.com/shop/page",
            b"id=nested; Path=/shop",
            101
        ));
        assert!(put(
            &store,
            "https://a.example.com/shop/page",
            b"shared=yes; Domain=.example.com; Path=/",
            102
        ));
        assert_eq!(
            get(&store, "https://a.example.com/shop/cart", 103),
            "id=nested; id=root; shared=yes"
        );
        assert_eq!(
            get(&store, "https://a.example.com/shopping", 103),
            "id=root; shared=yes"
        );
        assert_eq!(get(&store, "https://b.example.com/shop", 103), "shared=yes");
        assert_eq!(get(&store, "https://example.com.evil.test/shop", 103), "");
        assert!(put(
            &store,
            "https://a.example.com/shop/page",
            b"id=replaced; Path=/",
            104
        ));
        assert_eq!(
            get(&store, "https://a.example.com/shop", 105),
            "id=nested; id=replaced; shared=yes"
        );
    }
    #[test]
    fn pinned_suffix_rules_include_private_wildcard_exception_and_idn() {
        assert_eq!(registrable_domain("a.b.co.uk"), "b.co.uk");
        assert_eq!(registrable_domain("a.github.io"), "a.github.io");
        assert!(is_public_suffix("github.io"));
        assert!(is_public_suffix("b.ck"));
        assert!(!is_public_suffix("www.ck"));
        assert_eq!(registrable_domain("a.www.ck"), "www.ck");
        assert_eq!(
            Site::from_url(&url("https://a.example.com:9000")),
            Site::from_url(&url("https://b.example.com:3000"))
        );
        assert_ne!(
            Site::from_url(&url("http://example.com")),
            Site::from_url(&url("https://example.com"))
        );
        let store = CookieStore::new();
        assert!(!put(
            &store,
            "https://a.co.uk/",
            b"bad=1; Domain=co.uk",
            100
        ));
        assert!(!put(
            &store,
            "https://a.github.io/",
            b"bad=1; Domain=github.io",
            100
        ));
        assert!(!put(
            &store,
            "https://example.com/",
            b"bad=1; Domain=evil.com",
            100
        ));
        assert!(!put(
            &store,
            "https://example.com/",
            b"bad=1; Domain=example.com.",
            100
        ));
        assert!(!put(
            &store,
            "https://xn--bcher-kva.example/",
            b"bad=1; Domain=b\xfccher.example",
            100
        ));
        assert!(put(
            &store,
            "https://xn--bcher-kva.example/",
            b"ok=1; Domain=xn--bcher-kva.example",
            100
        ));
        assert!(put(
            &store,
            "http://127.0.0.1/",
            b"ip=1; Domain=127.0.0.1",
            100
        ));
        assert_eq!(get(&store, "http://127.0.0.2/", 100), "");
    }
    #[test]
    fn security_prefixes_and_script_http_only_cannot_be_bypassed() {
        let store = CookieStore::new();
        let address = url("https://example.com/");
        assert!(!put(&store, "http://example.com/", b"a=1; Secure", 100));
        assert!(!put(&store, "https://example.com/", b"__SeCuRe-a=1", 100));
        assert!(!put(
            &store,
            "https://example.com/",
            b"__Host-a=1; Secure",
            100
        ));
        assert!(!put(
            &store,
            "https://example.com/",
            b"__Host-a=1; Secure; Path=/; Domain=example.com",
            100
        ));
        assert!(!put(
            &store,
            "https://example.com/",
            b"__Http-a=1; Secure",
            100
        ));
        assert!(!put(
            &store,
            "https://example.com/",
            b"=__Host-a=1; Secure; Path=/",
            100
        ));
        assert!(store.receive_at(
            &address,
            b"__Host-Http-a=1; Secure; HttpOnly; Path=/",
            &CookieContext::subresource(&address),
            false,
            100
        ));
        assert!(!store.receive_at(
            &address,
            b"__Host-Http-a=2; Secure; HttpOnly; Path=/",
            &CookieContext::subresource(&address),
            true,
            101
        ));
        assert!(
            store
                .retrieve(&address, &CookieContext::subresource(&address), true, 102)
                .is_empty()
        );
        assert_eq!(get(&store, "https://example.com/", 102), "__Host-Http-a=1");
    }
    #[test]
    fn insecure_cookie_cannot_overlay_secure_path_or_delete_http_only() {
        let store = CookieStore::new();
        assert!(put(
            &store,
            "https://example.com/",
            b"sid=secure; Secure; Path=/login",
            100
        ));
        assert!(!put(
            &store,
            "http://example.com/login",
            b"sid=bad; Path=/login/child",
            101
        ));
        assert!(put(&store, "http://example.com/", b"sid=root; Path=/", 101));
        assert_eq!(
            get(&store, "https://example.com/login", 102),
            "sid=secure; sid=root"
        );
        let address = url("https://example.com/");
        assert!(put(
            &store,
            address.as_str(),
            b"private=yes; HttpOnly; Path=/",
            100
        ));
        assert!(!store.receive_at(
            &address,
            b"private=no; Max-Age=0; Path=/",
            &CookieContext::subresource(&address),
            true,
            101
        ));
        assert!(get(&store, address.as_str(), 102).contains("private=yes"));
    }
    #[test]
    fn same_site_lax_default_grace_and_redirect_taint_are_enforced() {
        let store = CookieStore::new();
        let address = url("https://a.example/");
        for value in [
            b"strict=1; SameSite=Strict".as_slice(),
            b"lax=1; SameSite=Lax",
            b"default=1",
            b"none=1; SameSite=None; Secure",
        ] {
            assert!(put(&store, address.as_str(), value, 100));
        }
        assert!(!put(
            &store,
            address.as_str(),
            b"invalid=1; SameSite=None",
            100
        ));
        let cross = url("https://other.test/");
        let mut context = CookieContext::subresource(&cross);
        assert_eq!(
            wire_string(&store.retrieve(&address, &context, false, 101)),
            "none=1"
        );
        context = CookieContext::navigation(Some(&cross), &address, "GET");
        assert_eq!(
            wire_string(&store.retrieve(&address, &context, false, 101)),
            "lax=1; default=1; none=1"
        );
        context.method = "POST".into();
        assert_eq!(
            wire_string(&store.retrieve(&address, &context, false, 101)),
            "default=1; none=1"
        );
        assert_eq!(
            wire_string(&store.retrieve(&address, &context, false, 221)),
            "none=1"
        );
        context = CookieContext::subresource(&address);
        context.redirect_to(&cross, "GET");
        context.redirect_to(&address, "GET");
        assert_eq!(
            wire_string(&store.retrieve(&address, &context, false, 101)),
            "none=1"
        );
        assert!(!store.receive_at(
            &address,
            b"cross=lax",
            &CookieContext::subresource(&cross),
            false,
            101
        ));
        assert!(store.receive_at(
            &address,
            b"cross=none; SameSite=None; Secure",
            &CookieContext::subresource(&cross),
            false,
            101
        ));
    }
    #[test]
    fn partitioned_storage_is_keyed_by_top_level_site() {
        let store = CookieStore::new();
        let tracker = url("https://tracker.test/");
        let a = CookieContext::subresource(&url("https://a.example/"));
        let b = CookieContext::subresource(&url("https://b.example/"));
        assert!(store.receive_at(
            &tracker,
            b"chip=a; Partitioned; SameSite=None; Secure",
            &a,
            false,
            100
        ));
        assert!(store.receive_at(
            &tracker,
            b"chip=b; Partitioned; SameSite=None; Secure",
            &b,
            false,
            100
        ));
        assert_eq!(
            wire_string(&store.retrieve(&tracker, &a, false, 101)),
            "chip=a"
        );
        assert_eq!(
            wire_string(&store.retrieve(&tracker, &b, false, 101)),
            "chip=b"
        );
        assert!(!store.receive_at(&tracker, b"bad=1; Partitioned", &a, false, 100));
    }
    #[test]
    fn expiry_max_age_precedence_and_lenient_date_formats() {
        assert_eq!(
            cookie_date(b"Wed, 09 Jun 2021 10:18:14 GMT"),
            Some(1623233894)
        );
        assert_eq!(
            cookie_date(b"Wednesday, 09-Jun-21 10:18:14 GMT"),
            Some(1623233894)
        );
        assert_eq!(cookie_date(b"Wed Jun  9 10:18:14 2021"), Some(1623233894));
        assert_eq!(cookie_date(b"31 Apr 2021 10:18:14 GMT"), None);
        assert_eq!(cookie_date(b"09 Jun 2021 10:99:14 GMT"), None);
        let store = CookieStore::new();
        assert!(put(
            &store,
            "https://example.test/a",
            b"a=1; Max-Age=10; Expires=Wed, 09 Jun 2021 10:18:14 GMT",
            100
        ));
        assert_eq!(get(&store, "https://example.test/", 109), "a=1");
        assert_eq!(get(&store, "https://example.test/", 110), "");
        assert!(put(
            &store,
            "https://example.test/",
            b"a=2; Max-Age=999999999999999999999999",
            100
        ));
        assert_eq!(get(&store, "https://example.test/", 100 + MAX_AGE), "");
        assert!(put(
            &store,
            "https://example.test/",
            b"a=3; Max-Age=10; Max-Age=bad",
            100
        ));
        assert_eq!(get(&store, "https://example.test/", 111), "");
    }
    #[test]
    fn wire_bytes_nameless_pairs_and_utf8_document_boundary_are_preserved() {
        let store = CookieStore::new();
        let address = url("https://example.test/");
        assert!(store.set_document_cookie(&address, "word=żółw; Path=/"));
        assert!(store.document_cookie(&address).contains("word=żółw"));
        assert!(put(&store, address.as_str(), b"raw=\xff\x80", now()));
        assert!(
            store
                .header(&address, &CookieContext::subresource(&address))
                .ends_with(b"raw=\xff\x80")
        );
        assert!(put(&store, address.as_str(), b"plain-token", now()));
        assert!(store.document_cookie(&address).ends_with("plain-token"));
        assert!(!put(
            &store,
            address.as_str(),
            b"evil=one\nCookie:two",
            now()
        ));
        assert!(!put(&store, address.as_str(), b" ", now()));
    }
    #[test]
    fn persistent_round_trip_excludes_session_and_rejects_corruption_atomically() {
        let store = CookieStore::new();
        let address = url("https://example.test/");
        assert!(store.receive(
            &address,
            b"session=1",
            &CookieContext::subresource(&address)
        ));
        assert!(store.receive(
            &address,
            b"persist=\xff; Max-Age=3600; HttpOnly",
            &CookieContext::subresource(&address)
        ));
        let bytes = store.export_persistent().unwrap();
        let restored = CookieStore::new();
        restored.restore_persistent(&bytes).unwrap();
        assert_eq!(
            restored.header(&address, &CookieContext::subresource(&address)),
            b"persist=\xff"
        );
        assert!(restored.document_cookie(&address).is_empty());
        let previous = restored.header(&address, &CookieContext::subresource(&address));
        assert!(restored.restore_persistent(b"{invalid").is_err());
        assert_eq!(
            restored.header(&address, &CookieContext::subresource(&address)),
            previous
        );
    }
    #[test]
    fn cookie_count_size_eviction_and_revisions_are_bounded() {
        let store = CookieStore::new();
        let address = url("https://example.test/");
        let context = CookieContext::subresource(&address);
        assert!(!store.receive(&address, &vec![b'a'; COOKIE_BYTES + 1], &context));
        for i in 0..(DOMAIN_COUNT + 20) {
            assert!(store.receive(&address, format!("k{i}={i}").as_bytes(), &context));
        }
        assert_eq!(store.len(), DOMAIN_COUNT);
        let bytes = store.header(&address, &context);
        assert!(!bytes.starts_with(b"k0="));
        let revision = store.revision();
        store.clear();
        assert!(store.revision() > revision);
        assert!(store.is_empty());
    }
}
