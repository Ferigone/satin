//! WHATWG HTML tokenization states; UTF-8 input is already newline-normalized.
use super::decode_entities;
use crate::dom::Attributes;

pub(super) fn space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

#[derive(Clone, Debug)]
pub(super) struct Tag {
    pub name: String,
    pub attrs: Attributes,
    pub self_closing: bool,
}

impl Tag {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            attrs: Attributes::new(),
            self_closing: false,
        }
    }
}

#[derive(Debug)]
pub(super) struct Doctype {
    pub name: String,
    pub public_id: Option<String>,
    pub system_id: Option<String>,
    pub force_quirks: bool,
}

#[derive(Debug)]
pub(super) enum Token {
    Characters(String),
    Start(Tag),
    End(String),
    Comment(String),
    Doctype(Doctype),
    Eof,
}

pub(super) enum Content {
    Data,
    Rcdata(String),
    Rawtext(String),
    Script(String),
    Plaintext,
    Cdata,
}

pub(super) struct Tokenizer<'a> {
    source: &'a str,
    pos: usize,
    pub content: Content,
    pub allow_cdata: bool,
}

impl<'a> Tokenizer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            content: Content::Data,
            allow_cdata: false,
        }
    }

    pub fn next(&mut self) -> Token {
        loop {
            let token = self.next_internal();
            // Ignored syntax has no token and cannot interrupt the tree
            // builder's pending table text or "ignore the next LF" rule.
            if !matches!(&token, Token::Characters(s) if s.is_empty()) {
                return token;
            }
        }
    }

    fn next_internal(&mut self) -> Token {
        if self.pos == self.source.len() {
            return Token::Eof;
        }
        match &self.content {
            Content::Cdata => return self.cdata(),
            Content::Plaintext => {
                let text = self.source[self.pos..].replace('\0', "\u{fffd}");
                self.pos = self.source.len();
                return Token::Characters(text);
            }
            Content::Rcdata(name) | Content::Rawtext(name) => {
                let decode = matches!(&self.content, Content::Rcdata(_));
                let end = self.raw_end(name).unwrap_or(self.source.len());
                if end > self.pos {
                    let text = self.source[self.pos..end].replace('\0', "\u{fffd}");
                    self.pos = end;
                    return Token::Characters(if decode {
                        decode_entities(&text, false)
                    } else {
                        text
                    });
                }
                self.content = Content::Data;
            }
            Content::Script(name) => {
                let end = self.script_end(name);
                if end > self.pos {
                    let text = self.source[self.pos..end].replace('\0', "\u{fffd}");
                    self.pos = end;
                    return Token::Characters(text);
                }
                self.content = Content::Data;
            }
            Content::Data => {}
        }
        let bytes = self.source.as_bytes();
        let start = self.pos;
        if bytes[start] != b'<' {
            self.pos += self.source[start..]
                .find('<')
                .unwrap_or(self.source.len() - start);
            return Token::Characters(decode_entities(&self.source[start..self.pos], false));
        }
        if self.source[start..].starts_with("<!--") {
            self.pos += 4;
            return Token::Comment(self.comment());
        }
        if self.starts_ci(start, "<!doctype") {
            self.pos += 9;
            return Token::Doctype(self.doctype());
        }
        if self.allow_cdata && self.source[start..].starts_with("<![CDATA[") {
            self.pos += 9;
            self.content = Content::Cdata;
            return self.cdata();
        }
        if bytes.get(start + 1) == Some(&b'!') {
            self.pos += 2;
            return Token::Comment(self.bogus_comment());
        }
        if bytes.get(start + 1) == Some(&b'?') {
            self.pos += 1;
            return Token::Comment(self.bogus_comment());
        }
        let closing = bytes.get(start + 1) == Some(&b'/');
        let name_start = start + if closing { 2 } else { 1 };
        if bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            self.pos = name_start;
            return self.tag(closing);
        }
        if closing {
            if bytes.get(name_start) == Some(&b'>') {
                self.pos = name_start + 1;
                // Skip in next()'s iterative loop, without native recursion.
                return Token::Characters(String::new());
            }
            if name_start < bytes.len() {
                self.pos = name_start;
                return Token::Comment(self.bogus_comment());
            }
        }
        self.pos += 1;
        Token::Characters("<".into())
    }

    fn cdata(&mut self) -> Token {
        let start = self.pos;
        let end = self.source[start..]
            .find("]]>")
            .map(|relative| start + relative)
            .unwrap_or(self.source.len());
        self.pos = (end + 3).min(self.source.len());
        self.content = Content::Data;
        Token::Characters(self.source[start..end].into())
    }

    fn starts_ci(&self, pos: usize, s: &str) -> bool {
        self.source
            .as_bytes()
            .get(pos..pos + s.len())
            .is_some_and(|b| b.eq_ignore_ascii_case(s.as_bytes()))
    }

    fn appropriate_end(&self, pos: usize, name: &str) -> bool {
        !name.is_empty()
            && self.starts_ci(pos, "</")
            && self.starts_ci(pos + 2, name)
            && self
                .source
                .as_bytes()
                .get(pos + 2 + name.len())
                .is_some_and(|&b| space(b) || matches!(b, b'/' | b'>'))
    }

    fn raw_end(&self, name: &str) -> Option<usize> {
        let mut pos = self.pos;
        while let Some(relative) = self.source[pos..].find('<') {
            pos += relative;
            if self.appropriate_end(pos, name) {
                return Some(pos);
            }
            pos += 1;
        }
        None
    }

    fn script_end(&self, appropriate_tag: &str) -> usize {
        // Script data escaped/double-escaped states matter even without execution:
        // <!-- <script> </script> --> keeps the inner end tag as script text.
        let bytes = self.source.as_bytes();
        let mut pos = self.pos;
        let mut escaped = false;
        let mut double = false;
        while pos < bytes.len() {
            if !double && self.appropriate_end(pos, appropriate_tag) {
                return pos;
            }
            if !escaped && self.source[pos..].starts_with("<!--") {
                escaped = true;
                pos += 4;
                continue;
            }
            if escaped && self.source[pos..].starts_with("-->") {
                escaped = false;
                double = false;
                pos += 3;
                continue;
            }
            if escaped
                && !double
                && self.starts_ci(pos, "<script")
                && bytes
                    .get(pos + 7)
                    .is_some_and(|&b| space(b) || matches!(b, b'/' | b'>'))
            {
                double = true;
                pos += 7;
                continue;
            }
            if double && self.appropriate_end(pos, "script") {
                double = false;
                pos += 8;
                continue;
            }
            pos += self.source[pos..].chars().next().unwrap().len_utf8();
        }
        bytes.len()
    }

    fn tag(&mut self, closing: bool) -> Token {
        let bytes = self.source.as_bytes();
        let start = self.pos;
        while self.pos < bytes.len()
            && !space(bytes[self.pos])
            && !matches!(bytes[self.pos], b'/' | b'>')
        {
            self.pos += 1;
        }
        let mut tag = Tag::new(
            &self.source[start..self.pos]
                .replace('\0', "\u{fffd}")
                .to_ascii_lowercase(),
        );
        while self.pos < bytes.len() {
            self.skip_space();
            match bytes.get(self.pos) {
                Some(b'>') => {
                    self.pos += 1;
                    return if closing {
                        Token::End(tag.name)
                    } else {
                        Token::Start(tag)
                    };
                }
                Some(b'/') => {
                    self.pos += 1;
                    if bytes.get(self.pos) == Some(&b'>') {
                        tag.self_closing = true;
                        self.pos += 1;
                        return if closing {
                            Token::End(tag.name)
                        } else {
                            Token::Start(tag)
                        };
                    }
                    continue;
                }
                None => break,
                _ => {}
            }
            let start = self.pos;
            // An initial '=' is an attribute-name character, as are quotes and '<'.
            self.pos += 1;
            while self.pos < bytes.len()
                && !space(bytes[self.pos])
                && !matches!(bytes[self.pos], b'/' | b'>' | b'=')
            {
                self.pos += 1;
            }
            let name = self.source[start..self.pos]
                .replace('\0', "\u{fffd}")
                .to_ascii_lowercase();
            self.skip_space();
            let mut value = String::new();
            if bytes.get(self.pos) == Some(&b'=') {
                self.pos += 1;
                self.skip_space();
                if let Some(q @ (b'\'' | b'"')) = bytes.get(self.pos).copied() {
                    self.pos += 1;
                    let start = self.pos;
                    while self.pos < bytes.len() && bytes[self.pos] != q {
                        self.pos += 1;
                    }
                    value = self.source[start..self.pos].replace('\0', "\u{fffd}");
                    if self.pos < bytes.len() {
                        self.pos += 1;
                    }
                } else {
                    let start = self.pos;
                    while self.pos < bytes.len()
                        && !space(bytes[self.pos])
                        && bytes[self.pos] != b'>'
                    {
                        self.pos += 1;
                    }
                    value = self.source[start..self.pos].replace('\0', "\u{fffd}");
                }
            }
            if let indexmap::map::Entry::Vacant(entry) = tag.attrs.entry(name) {
                entry.insert(decode_entities(&value, true));
            }
        }
        // EOF inside a tag does not emit an incomplete tag token.
        Token::Eof
    }

    fn skip_space(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(|&b| space(b))
        {
            self.pos += 1;
        }
    }

    fn bogus_comment(&mut self) -> String {
        let start = self.pos;
        self.pos += self.source[start..]
            .find('>')
            .unwrap_or(self.source.len() - start);
        let data = self.source[start..self.pos].replace('\0', "\u{fffd}");
        if self.pos < self.source.len() {
            self.pos += 1;
        }
        data
    }

    fn comment(&mut self) -> String {
        #[derive(Clone, Copy)]
        enum State {
            Start,
            StartDash,
            Data,
            Less,
            LessBang,
            LessBangDash,
            LessBangDashDash,
            EndDash,
            End,
            EndBang,
        }
        let mut state = State::Start;
        let mut out = String::new();
        while self.pos < self.source.len() {
            let c = self.source[self.pos..].chars().next().unwrap();
            let mut consume = true;
            match state {
                State::Start => match c {
                    '-' => state = State::StartDash,
                    '>' => {
                        self.pos += 1;
                        return out;
                    }
                    _ => {
                        state = State::Data;
                        consume = false;
                    }
                },
                State::StartDash => match c {
                    '-' => state = State::End,
                    '>' => {
                        self.pos += 1;
                        return out;
                    }
                    _ => {
                        out.push('-');
                        state = State::Data;
                        consume = false;
                    }
                },
                State::Data => match c {
                    '<' => {
                        out.push('<');
                        state = State::Less;
                    }
                    '-' => state = State::EndDash,
                    '\0' => out.push('\u{fffd}'),
                    _ => out.push(c),
                },
                State::Less => match c {
                    '!' => {
                        out.push('!');
                        state = State::LessBang;
                    }
                    '<' => out.push('<'),
                    _ => {
                        state = State::Data;
                        consume = false;
                    }
                },
                State::LessBang => {
                    if c == '-' {
                        state = State::LessBangDash;
                    } else {
                        state = State::Data;
                        consume = false;
                    }
                }
                State::LessBangDash => {
                    if c == '-' {
                        state = State::LessBangDashDash;
                    } else {
                        state = State::EndDash;
                        consume = false;
                    }
                }
                State::LessBangDashDash => {
                    state = State::End;
                    consume = false;
                }
                State::EndDash => {
                    if c == '-' {
                        state = State::End;
                    } else {
                        out.push('-');
                        state = State::Data;
                        consume = false;
                    }
                }
                State::End => match c {
                    '>' => {
                        self.pos += 1;
                        return out;
                    }
                    '!' => state = State::EndBang,
                    '-' => out.push('-'),
                    _ => {
                        out.push_str("--");
                        state = State::Data;
                        consume = false;
                    }
                },
                State::EndBang => match c {
                    '-' => {
                        out.push_str("--!");
                        state = State::EndDash;
                    }
                    '>' => {
                        self.pos += 1;
                        return out;
                    }
                    _ => {
                        out.push_str("--!");
                        state = State::Data;
                        consume = false;
                    }
                },
            }
            if consume {
                self.pos += c.len_utf8();
            }
        }
        out
    }

    fn doctype(&mut self) -> Doctype {
        let mut token = Doctype {
            name: String::new(),
            public_id: None,
            system_id: None,
            force_quirks: false,
        };
        self.skip_space();
        let start = self.pos;
        while let Some(&b) = self.source.as_bytes().get(self.pos) {
            if space(b) || b == b'>' {
                break;
            }
            self.pos += 1;
        }
        token.name = self.source[start..self.pos]
            .replace('\0', "\u{fffd}")
            .to_ascii_lowercase();
        if token.name.is_empty() {
            token.force_quirks = true;
        }
        self.skip_space();
        if self.source.as_bytes().get(self.pos) == Some(&b'>') {
            self.pos += 1;
            return token;
        }
        if self.pos == self.source.len() {
            token.force_quirks = true;
            return token;
        }
        let public = self.starts_ci(self.pos, "PUBLIC");
        let system = self.starts_ci(self.pos, "SYSTEM");
        if !public && !system {
            token.force_quirks = true;
            self.bogus_comment();
            return token;
        }
        self.pos += 6;
        self.skip_space();
        let Some(value) = self.quoted_identifier(&mut token.force_quirks) else {
            token.force_quirks = true;
            self.bogus_comment();
            return token;
        };
        if public {
            token.public_id = Some(value);
        } else {
            token.system_id = Some(value);
        }
        if token.force_quirks {
            return token;
        }
        self.skip_space();
        if self.pos == self.source.len() {
            token.force_quirks = true;
            return token;
        }
        if self.source.as_bytes()[self.pos] == b'>' {
            self.pos += 1;
            return token;
        }
        if public {
            if let Some(value) = self.quoted_identifier(&mut token.force_quirks) {
                token.system_id = Some(value);
            } else {
                token.force_quirks = true;
                self.bogus_comment();
                return token;
            }
            if token.force_quirks {
                return token;
            }
            self.skip_space();
            if self.pos == self.source.len() {
                token.force_quirks = true;
                return token;
            }
        }
        // Garbage after a system identifier is bogus-doctype, without forcing quirks.
        self.bogus_comment();
        token
    }

    fn quoted_identifier(&mut self, force: &mut bool) -> Option<String> {
        let q = self.source.as_bytes().get(self.pos).copied()?;
        if !matches!(q, b'\'' | b'"') {
            return None;
        }
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.source.len()
            && !matches!(self.source.as_bytes()[self.pos], b'>')
            && self.source.as_bytes()[self.pos] != q
        {
            self.pos += 1;
        }
        let value = self.source[start..self.pos].replace('\0', "\u{fffd}");
        if self.source.as_bytes().get(self.pos) != Some(&q) {
            *force = true;
        }
        if self.pos < self.source.len() {
            self.pos += 1;
        }
        Some(value)
    }
}
