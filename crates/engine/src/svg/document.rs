//! Small, inert SVG XML front end. It deliberately rejects DTDs and vocabulary
//! namespaces that the DOM arena cannot represent instead of repairing XML as
//! HTML or fetching entity/stylesheet resources.
use super::{MAX_SVG_DEPTH, MAX_SVG_NODES};
use crate::dom::{AttributeNamespace, Document, Namespace, NodeId, NodeKind};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SVG_DOCUMENT_SOURCE: usize = 4 * 1024 * 1024;
pub const MAX_SVG_DOCUMENT_STRINGS: usize = 8 * 1024 * 1024;
pub const MAX_SVG_DOCUMENT_ATTRIBUTES: usize = 256;
const MAX_WORK: usize = 16 * 1024 * 1024;
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
const XLINK: &str = "http://www.w3.org/1999/xlink";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgDocumentErrorKind {
    Invalid,
    Unsupported,
    Limit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvgDocumentError {
    pub kind: SvgDocumentErrorKind,
    /// Byte offset in the source after XML newline normalization.
    pub offset: usize,
    pub message: &'static str,
}
impl std::fmt::Display for SvgDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SVG XML {:?} at {}: {}",
            self.kind, self.offset, self.message
        )
    }
}
impl std::error::Error for SvgDocumentError {}
type Result<T> = std::result::Result<T, SvgDocumentError>;

struct Frame {
    id: NodeId,
    qualified: String,
    bindings: Vec<(String, String)>,
}
struct Parser<'a> {
    source: &'a str,
    offset: usize,
    work: usize,
    strings: usize,
    doc: Document,
    stack: Vec<Frame>,
    root_seen: bool,
}

/// Parse the bounded, namespace-aware XML subset used by standalone SVG image
/// resources. Inline SVG must continue to use the HTML foreign-content parser.
/// DTD/entity declarations and external resources are never processed.
pub fn parse_document(source: &str) -> Result<Document> {
    if source.len() > MAX_SVG_DOCUMENT_SOURCE {
        return Err(SvgDocumentError {
            kind: SvgDocumentErrorKind::Limit,
            offset: 0,
            message: "source exceeds 4 MiB",
        });
    }
    if let Some((offset, _)) = source.char_indices().find(|(_, c)| !xml_char(*c)) {
        return Err(SvgDocumentError {
            kind: SvgDocumentErrorKind::Invalid,
            offset,
            message: "character is forbidden in XML 1.0",
        });
    }
    let normalized = source
        .strip_prefix('\u{feff}')
        .unwrap_or(source)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    Parser {
        source: &normalized,
        offset: 0,
        work: source.len(),
        strings: 0,
        doc: Document::default(),
        stack: vec![],
        root_seen: false,
    }
    .parse()
}

impl Parser<'_> {
    fn error(&self, kind: SvgDocumentErrorKind, message: &'static str) -> SvgDocumentError {
        SvgDocumentError {
            kind,
            offset: self.offset,
            message,
        }
    }
    fn invalid<T>(&self, message: &'static str) -> Result<T> {
        Err(self.error(SvgDocumentErrorKind::Invalid, message))
    }
    fn charge(&mut self, amount: usize) -> Result<()> {
        self.work = self.work.saturating_add(amount);
        if self.work > MAX_WORK {
            return Err(self.error(SvgDocumentErrorKind::Limit, "XML work budget exceeded"));
        }
        Ok(())
    }
    fn retain(&mut self, amount: usize) -> Result<()> {
        self.strings = self.strings.saturating_add(amount);
        if self.strings > MAX_SVG_DOCUMENT_STRINGS {
            return Err(self.error(SvgDocumentErrorKind::Limit, "XML string budget exceeded"));
        }
        self.charge(amount)
    }
    fn rest(&self) -> &str {
        &self.source[self.offset..]
    }
    fn ws(&mut self) {
        self.offset += self
            .rest()
            .bytes()
            .take_while(|b| matches!(b, b' ' | b'\t' | b'\n'))
            .count();
    }
    fn name(&mut self) -> Result<String> {
        let start = self.offset;
        let mut chars = self.rest().char_indices();
        let Some((_, first)) = chars.next() else {
            return self.invalid("missing XML name");
        };
        if !name_start(first) {
            return self.invalid("invalid XML name");
        }
        let mut length = first.len_utf8();
        for (index, c) in chars {
            if !name_char(c) {
                break;
            }
            length = index + c.len_utf8();
        }
        if length > 1024 {
            return Err(self.error(SvgDocumentErrorKind::Limit, "XML name exceeds 1024 bytes"));
        }
        self.offset += length;
        let name = self.source[start..self.offset].to_owned();
        let (prefix, local) = qualified(&name).ok_or_else(|| {
            self.error(
                SvgDocumentErrorKind::Invalid,
                "invalid namespace-qualified name",
            )
        })?;
        if local.is_empty() || prefix == Some("") {
            return self.invalid("empty namespace name component");
        }
        self.charge(length)?;
        Ok(name)
    }
    fn value(&mut self) -> Result<String> {
        let quote =
            self.rest().as_bytes().first().copied().ok_or_else(|| {
                self.error(SvgDocumentErrorKind::Invalid, "missing attribute value")
            })?;
        if !matches!(quote, b'\'' | b'"') {
            return self.invalid("XML attribute values must be quoted");
        }
        self.offset += 1;
        let length = self
            .rest()
            .find(char::from(quote))
            .ok_or_else(|| self.error(SvgDocumentErrorKind::Invalid, "unclosed attribute value"))?;
        let source = &self.source[self.offset..self.offset + length];
        if source.contains('<') {
            return self.invalid("literal < in attribute value");
        }
        let normalized = source.replace(['\t', '\n'], " ");
        let result = decode(&normalized).ok_or_else(|| {
            self.error(
                SvgDocumentErrorKind::Invalid,
                "invalid or undeclared entity reference",
            )
        })?;
        self.offset += length + 1;
        self.retain(result.len())?;
        Ok(result)
    }
    fn attributes(&mut self, terminator: &str) -> Result<BTreeMap<String, String>> {
        let mut attrs = BTreeMap::new();
        loop {
            let start = self.offset;
            self.ws();
            if self.rest().starts_with(terminator)
                || terminator == ">" && self.rest().starts_with("/>")
            {
                return Ok(attrs);
            }
            if start == self.offset {
                return self.invalid("attribute requires separating whitespace");
            }
            if attrs.len() >= MAX_SVG_DOCUMENT_ATTRIBUTES {
                return Err(self.error(SvgDocumentErrorKind::Limit, "too many attributes"));
            }
            let name = self.name()?;
            self.retain(name.len())?;
            self.ws();
            if !self.rest().starts_with('=') {
                return self.invalid("attribute requires =");
            }
            self.offset += 1;
            self.ws();
            let value = self.value()?;
            if attrs.insert(name, value).is_some() {
                return self.invalid("duplicate XML attribute");
            }
        }
    }
    fn resolve(&mut self, prefix: &str, bindings: &[(String, String)]) -> Result<Option<String>> {
        if prefix == "xml" {
            self.charge(XML.len())?;
            return Ok(Some(XML.into()));
        }
        for (name, uri) in bindings.iter().rev() {
            self.charge(1 + name.len())?;
            if name == prefix {
                self.charge(uri.len())?;
                return Ok(Some(uri.clone()));
            }
        }
        for index in (0..self.stack.len()).rev() {
            for binding in 0..self.stack[index].bindings.len() {
                let (name, _) = &self.stack[index].bindings[binding];
                self.charge(1 + name.len())?;
                if self.stack[index].bindings[binding].0 == prefix {
                    let length = self.stack[index].bindings[binding].1.len();
                    self.charge(length)?;
                    return Ok(Some(self.stack[index].bindings[binding].1.clone()));
                }
            }
        }
        Ok(None)
    }
    fn append(&mut self, kind: NodeKind) -> Result<NodeId> {
        if self.doc.nodes.len() >= MAX_SVG_NODES {
            return Err(self.error(SvgDocumentErrorKind::Limit, "too many XML nodes"));
        }
        let parent = self
            .stack
            .last()
            .map(|frame| frame.id)
            .unwrap_or(self.doc.root);
        Ok(self.doc.append(parent, kind))
    }
    fn start(&mut self) -> Result<()> {
        self.offset += 1;
        let name = self.name()?;
        let attrs = self.attributes(">")?;
        let empty = self.rest().starts_with("/>");
        if !empty && !self.rest().starts_with('>') {
            return self.invalid("unclosed start tag");
        }
        self.offset += if empty { 2 } else { 1 };
        if self.stack.len() >= MAX_SVG_DEPTH {
            return Err(self.error(SvgDocumentErrorKind::Limit, "XML element depth exceeds 64"));
        }
        if self.stack.is_empty() && self.root_seen {
            return self.invalid("multiple root elements");
        }
        let mut bindings = vec![];
        for (name, uri) in &attrs {
            let prefix = if name == "xmlns" {
                ""
            } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                prefix
            } else {
                continue;
            };
            if uri.len() > 4096 {
                return Err(self.error(
                    SvgDocumentErrorKind::Limit,
                    "namespace URI exceeds 4096 bytes",
                ));
            }
            if prefix == "xmlns"
                || uri == XMLNS
                || prefix == "xml" && uri != XML
                || prefix != "xml" && uri == XML
                || !prefix.is_empty() && uri.is_empty()
            {
                return self.invalid("reserved or empty namespace binding");
            }
            self.retain(prefix.len() + uri.len())?;
            bindings.push((prefix.into(), uri.clone()));
        }
        let (prefix, local) = qualified(&name).unwrap();
        if prefix == Some("xmlns") {
            return self.invalid("xmlns cannot be an element prefix");
        }
        let uri = self
            .resolve(prefix.unwrap_or(""), &bindings)?
            .unwrap_or_default();
        if prefix.is_some() && uri.is_empty() {
            return self.invalid("unbound element prefix");
        }
        let namespace = namespace(&uri).ok_or_else(|| {
            self.error(
                SvgDocumentErrorKind::Unsupported,
                "element namespace is not in the SVG DOM subset",
            )
        })?;
        if self.stack.is_empty() && (namespace != Namespace::Svg || local != "svg") {
            return Err(self.error(
                SvgDocumentErrorKind::Unsupported,
                "root must be an SVG-namespace svg element",
            ));
        }
        let mut namespaced = vec![];
        let mut expanded = BTreeSet::new();
        for name in attrs.keys() {
            let (prefix, local) = qualified(name).unwrap();
            let uri = if name == "xmlns" || prefix == Some("xmlns") {
                XMLNS.into()
            } else if let Some(prefix) = prefix {
                self.resolve(prefix, &bindings)?.ok_or_else(|| {
                    self.error(SvgDocumentErrorKind::Invalid, "unbound attribute prefix")
                })?
            } else {
                String::new()
            };
            if !expanded.insert((uri.clone(), local.to_owned())) {
                return self.invalid("duplicate expanded attribute name");
            }
            if !uri.is_empty() {
                let ns = match uri.as_str() {
                    XML => AttributeNamespace::Xml,
                    XMLNS => AttributeNamespace::Xmlns,
                    XLINK => AttributeNamespace::XLink,
                    _ => {
                        return Err(self.error(
                            SvgDocumentErrorKind::Unsupported,
                            "attribute namespace is not in the SVG DOM subset",
                        ));
                    }
                };
                self.retain(name.len())?;
                namespaced.push((name.clone(), ns));
            }
        }
        self.retain(local.len() + name.len())?;
        let node = self.append(NodeKind::Element {
            tag: local.into(),
            attrs,
        })?;
        if namespace != Namespace::Html {
            self.doc.namespaces.insert(node, namespace);
        }
        for (name, ns) in namespaced {
            self.doc.attribute_namespaces.insert((node, name), ns);
        }
        self.root_seen = true;
        if !empty {
            self.stack.push(Frame {
                id: node,
                qualified: name,
                bindings,
            });
        }
        Ok(())
    }
    fn parse(mut self) -> Result<Document> {
        while !self.rest().is_empty() {
            self.charge(1)?;
            if self.rest().starts_with("<!--") {
                self.offset += 4;
                let length = self.rest().find("-->").ok_or_else(|| {
                    self.error(SvgDocumentErrorKind::Invalid, "unclosed XML comment")
                })?;
                let content = &self.source[self.offset..self.offset + length];
                if content.contains("--") || content.ends_with('-') {
                    return self.invalid("-- is forbidden in XML comments");
                }
                self.retain(content.len())?;
                self.append(NodeKind::Comment(content.into()))?;
                self.offset += length + 3;
            } else if self.rest().starts_with("<![CDATA[") {
                if self.stack.is_empty() {
                    return self.invalid("CDATA outside the root element");
                }
                self.offset += 9;
                let length = self.rest().find("]]>").ok_or_else(|| {
                    self.error(SvgDocumentErrorKind::Invalid, "unclosed CDATA section")
                })?;
                let content = &self.source[self.offset..self.offset + length];
                self.retain(content.len())?;
                self.append(NodeKind::Text(content.into()))?;
                self.offset += length + 3;
            } else if self.rest().starts_with("<?") {
                let start = self.offset;
                self.offset += 2;
                let target = self.name()?;
                if target.eq_ignore_ascii_case("xml") {
                    if target != "xml" || start != 0 {
                        return self.invalid("XML declaration must be first");
                    }
                    let declaration_start = self.offset;
                    let attrs = self.attributes("?>")?;
                    let declaration = self.source[declaration_start..self.offset].trim_start();
                    if declaration.contains('&') {
                        return self.invalid("references are forbidden in XML declarations");
                    }
                    if !attrs.contains_key("version") || !declaration.starts_with("version") {
                        return self.invalid("XML declaration requires version first");
                    }
                    if attrs.get("version").map(String::as_str) != Some("1.0") {
                        return Err(self.error(
                            SvgDocumentErrorKind::Unsupported,
                            "only XML 1.0 is supported",
                        ));
                    }
                    if attrs.get("encoding").is_some_and(|s| {
                        !s.eq_ignore_ascii_case("utf-8") && !s.eq_ignore_ascii_case("utf8")
                    }) {
                        return Err(self.error(
                            SvgDocumentErrorKind::Unsupported,
                            "only decoded UTF-8 SVG sources are supported",
                        ));
                    }
                    if attrs
                        .keys()
                        .any(|name| !matches!(name.as_str(), "version" | "encoding" | "standalone"))
                        || attrs
                            .get("standalone")
                            .is_some_and(|s| !matches!(s.as_str(), "yes" | "no"))
                    {
                        return self.invalid("invalid XML declaration");
                    }
                    if let (Some(encoding), Some(standalone)) =
                        (declaration.find("encoding"), declaration.find("standalone"))
                        && encoding > standalone
                    {
                        return self.invalid("encoding must precede standalone");
                    }
                    self.offset += 2;
                } else {
                    if !self.rest().starts_with("?>") && !self.rest().starts_with([' ', '\t', '\n'])
                    {
                        return self.invalid("processing instruction requires whitespace");
                    }
                    let length = self.rest().find("?>").ok_or_else(|| {
                        self.error(
                            SvgDocumentErrorKind::Invalid,
                            "unclosed processing instruction",
                        )
                    })?;
                    self.offset += length + 2;
                }
            } else if self.rest().starts_with("<!DOCTYPE") || self.rest().starts_with("<!ENTITY") {
                return Err(self.error(
                    SvgDocumentErrorKind::Unsupported,
                    "DTD and entity declarations are not processed",
                ));
            } else if self.rest().starts_with("</") {
                self.offset += 2;
                let name = self.name()?;
                self.ws();
                if !self.rest().starts_with('>') {
                    return self.invalid("unclosed end tag");
                }
                self.offset += 1;
                if self.stack.pop().is_none_or(|frame| frame.qualified != name) {
                    return self.invalid("end tag does not match start tag");
                }
            } else if self.rest().starts_with('<') {
                self.start()?;
            } else {
                let length = self.rest().find('<').unwrap_or(self.rest().len());
                let content = &self.source[self.offset..self.offset + length];
                if content.contains("]]>") {
                    return self.invalid("]]> outside CDATA");
                }
                let text = decode(content).ok_or_else(|| {
                    self.error(
                        SvgDocumentErrorKind::Invalid,
                        "invalid or undeclared entity reference",
                    )
                })?;
                if self.stack.is_empty() {
                    if !content.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n')) {
                        return self.invalid("text outside root element");
                    }
                } else if !text.is_empty() {
                    self.retain(text.len())?;
                    self.append(NodeKind::Text(text))?;
                }
                self.offset += length;
            }
        }
        if !self.stack.is_empty() {
            return self.invalid("unclosed root element");
        }
        if !self.root_seen {
            return self.invalid("missing SVG root element");
        }
        Ok(self.doc)
    }
}

fn namespace(uri: &str) -> Option<Namespace> {
    [Namespace::Svg, Namespace::Html, Namespace::MathMl]
        .into_iter()
        .find(|ns| ns.uri() == uri)
}
fn qualified(name: &str) -> Option<(Option<&str>, &str)> {
    let mut parts = name.split(':');
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() || !nc_name(first) || second.is_some_and(|name| !nc_name(name)) {
        return None;
    }
    Some(second.map_or((None, first), |local| (Some(first), local)))
}
fn nc_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c != ':' && name_start(c))
        && chars.all(|c| c != ':' && name_char(c))
}
fn name_start(c: char) -> bool {
    matches!(c, ':' | '_' | 'A'..='Z' | 'a'..='z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}
fn name_char(c: char) -> bool {
    name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}')
}
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}
fn decode(source: &str) -> Option<String> {
    let mut result = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let end = rest.find(';')?;
        let reference = &rest[..end];
        let c = match reference {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let value = if let Some(number) = reference.strip_prefix("#x") {
                    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return None;
                    }
                    u32::from_str_radix(number, 16).ok()?
                } else {
                    let number = reference.strip_prefix('#')?;
                    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    number.parse::<u32>().ok()?
                };
                char::from_u32(value).filter(|c| xml_char(*c))?
            }
        };
        result.push(c);
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Some(result)
}
