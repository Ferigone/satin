//! Persistent lexical framing for the shared HTML tokenizer. Completed markup
//! lexemes use its existing semantic states exactly once; incomplete lexemes,
//! references, raw end tags and script escapes survive input/write boundaries.
use super::{
    decode_entities, named_entity_prefix,
    tokenizer::{Content, Token, Tokenizer, space},
};
use std::collections::VecDeque;

const TEXT_BATCH: usize = 1024;

#[derive(Clone, Copy)]
enum TagState {
    Name,
    BeforeAttr,
    AttrName,
    AfterAttr,
    BeforeValue,
    Single,
    Double,
    AfterValue,
    Unquoted,
    Slash,
}
#[derive(Clone, Copy)]
enum CommentState {
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
#[derive(Clone, Copy)]
enum Scan {
    Data,
    Open,
    EndOpen,
    Declaration,
    Tag(TagState),
    Comment(CommentState),
    Doctype,
    Bogus,
    Cdata,
}
#[derive(Clone, Copy)]
enum Entity {
    None,
    Amp,
    Named,
    Number,
    HexStart,
    Decimal,
    Hex,
}

pub(super) struct IncrementalTokenizer {
    pub content: Content,
    pub allow_cdata: bool,
    scan: Scan,
    markup: String,
    text: String,
    entity: Entity,
    entity_start: usize,
    decode_text: bool,
    raw_candidate: String,
    script_escaped: bool,
    script_double: bool,
    script_dashes: u8,
    cdata_tail: String,
    output: VecDeque<Token>,
    finished: bool,
}
impl IncrementalTokenizer {
    pub fn new() -> Self {
        Self {
            content: Content::Data,
            allow_cdata: false,
            scan: Scan::Data,
            markup: String::new(),
            text: String::new(),
            entity: Entity::None,
            entity_start: 0,
            decode_text: true,
            raw_candidate: String::new(),
            script_escaped: false,
            script_double: false,
            script_dashes: 0,
            cdata_tail: String::new(),
            output: VecDeque::new(),
            finished: false,
        }
    }
    pub fn pop(&mut self) -> Option<Token> {
        self.output.pop_front()
    }
    pub fn pending_bytes(&self) -> usize {
        self.markup.len() + self.text.len() + self.raw_candidate.len() + self.cdata_tail.len()
    }

    fn text_char(&mut self, c: char, decode: bool, replace_null: bool) {
        self.decode_text = decode;
        let c = if replace_null && c == '\0' {
            '\u{fffd}'
        } else {
            c
        };
        let offset = self.text.len();
        self.text.push(c);
        if !decode {
            self.entity = Entity::None;
            return;
        }
        self.entity = if c == '&' {
            self.entity_start = offset;
            Entity::Amp
        } else {
            match self.entity {
                Entity::Amp if c == '#' => Entity::Number,
                Entity::Amp | Entity::Named
                    if c.is_ascii_alphanumeric()
                        && named_entity_prefix(&self.text[self.entity_start..]) =>
                {
                    Entity::Named
                }
                Entity::Number if matches!(c, 'x' | 'X') => Entity::HexStart,
                Entity::Number | Entity::Decimal if c.is_ascii_digit() => Entity::Decimal,
                Entity::HexStart | Entity::Hex if c.is_ascii_hexdigit() => Entity::Hex,
                _ => Entity::None,
            }
        };
    }
    fn text_str(&mut self, source: &str, decode: bool, replace_null: bool) {
        for c in source.chars() {
            self.text_char(c, decode, replace_null);
        }
    }
    fn flush(&mut self, force: bool) {
        let end = if force || matches!(self.entity, Entity::None) {
            self.text.len()
        } else {
            self.entity_start
        };
        if end == 0 {
            return;
        }
        let mut text = if end == self.text.len() {
            std::mem::take(&mut self.text)
        } else {
            self.text.drain(..end).collect::<String>()
        };
        if self.decode_text && text.contains('&') {
            text = decode_entities(&text, false);
        }
        if force {
            self.entity = Entity::None;
        } else {
            self.entity_start = self.entity_start.saturating_sub(end);
        }
        if !text.is_empty() {
            self.output.push_back(Token::Characters(text));
        }
    }
    pub fn partial(&mut self) -> Option<Token> {
        self.flush(false);
        self.pop()
    }
    fn frame(&mut self) {
        self.flush(true);
        let mut tokenizer = Tokenizer::new(&self.markup);
        tokenizer.allow_cdata = self.allow_cdata;
        let token = tokenizer.next();
        // EOF here belongs to an ignored/incomplete lexeme, never the input
        // stream. Only finish() below emits the tree's EOF token.
        if !matches!(token, Token::Eof) {
            self.output.push_back(token);
        }
        self.markup.clear();
        self.scan = Scan::Data;
    }

    /// Returns true once a raw appropriate-end-tag candidate has switched to
    /// normal tag syntax, so the content state must become Data.
    fn raw(&mut self, c: char, name: &str, decode: bool, script: bool) -> bool {
        if self.raw_candidate.is_empty() {
            if c == '<' {
                self.flush(true);
                self.raw_candidate.push(c);
            } else {
                self.script_char(c, script);
                self.text_char(c, decode, true);
            }
            return false;
        }
        self.raw_candidate.push(c);
        let end = format!("</{name}");
        let comment = script && !self.script_escaped;
        let start_script = script && self.script_escaped && !self.script_double;
        let inner_end = script && self.script_double;
        let lower = self.raw_candidate.to_ascii_lowercase();
        let end_name = if inner_end { "</script" } else { end.as_str() };
        let possible_end = (inner_end || !name.is_empty())
            && (end_name.starts_with(&lower) || lower.starts_with(end_name));
        let possible_comment = comment && "<!--".starts_with(&lower);
        let possible_start =
            start_script && ("<script".starts_with(&lower) || lower.starts_with("<script"));
        if possible_comment && lower == "<!--" {
            self.script_escaped = true;
            self.script_dashes = 0;
            let value = std::mem::take(&mut self.raw_candidate);
            self.text_str(&value, false, true);
            return false;
        }
        if possible_end && lower.len() > end_name.len() {
            if lower.len() == end_name.len() + 1
                && c.is_ascii()
                && (space(c as u8) || matches!(c, '/' | '>'))
            {
                if inner_end {
                    self.script_double = false;
                    self.script_dashes = 0;
                    let value = std::mem::take(&mut self.raw_candidate);
                    self.text_str(&value, false, true);
                    return false;
                }
                self.flush(true);
                self.markup = std::mem::take(&mut self.raw_candidate);
                self.scan = Scan::Tag(if c == '/' {
                    TagState::Slash
                } else {
                    TagState::BeforeAttr
                });
                if c == '>' {
                    self.frame();
                }
                self.script_escaped = false;
                self.script_double = false;
                self.script_dashes = 0;
                return true;
            }
        } else if possible_end {
            return false;
        }
        if possible_start {
            if lower.len() <= 7 {
                return false;
            }
            if lower.len() == 8 && c.is_ascii() && (space(c as u8) || matches!(c, '/' | '>')) {
                self.script_double = true;
                self.script_dashes = 0;
                let value = std::mem::take(&mut self.raw_candidate);
                self.text_str(&value, false, true);
                return false;
            }
        }
        if possible_comment {
            return false;
        }
        let mut value = std::mem::take(&mut self.raw_candidate);
        // A mismatching '<' may itself start the next candidate.
        if c == '<' {
            value.pop();
            self.raw_candidate.push('<');
        }
        for c in value.chars() {
            self.script_char(c, script);
            self.text_char(c, decode, true);
        }
        false
    }
    fn script_char(&mut self, c: char, script: bool) {
        if !script {
            return;
        }
        if self.script_escaped && c == '>' && self.script_dashes >= 2 {
            self.script_escaped = false;
            self.script_double = false;
        }
        self.script_dashes = if c == '-' {
            (self.script_dashes + 1).min(2)
        } else {
            0
        };
    }

    pub fn push(&mut self, c: char) -> Option<Token> {
        let mut content = std::mem::replace(&mut self.content, Content::Data);
        if matches!(content, Content::Cdata) {
            self.scan = Scan::Cdata;
            content = Content::Data;
        }
        let mut end_raw = false;
        match self.scan {
            Scan::Data => match &content {
                Content::Rcdata(name) => end_raw = self.raw(c, name, true, false),
                Content::Rawtext(name) => end_raw = self.raw(c, name, false, false),
                Content::Script(name) => end_raw = self.raw(c, name, false, true),
                Content::Plaintext => self.text_char(c, false, true),
                _ => self.data(c),
            },
            Scan::Open => {
                self.markup.push(c);
                match c {
                    c if c.is_ascii_alphabetic() => self.scan = Scan::Tag(TagState::Name),
                    '/' => self.scan = Scan::EndOpen,
                    '!' => self.scan = Scan::Declaration,
                    '?' => self.scan = Scan::Bogus,
                    _ => {
                        self.markup.clear();
                        self.scan = Scan::Data;
                        self.text_char('<', true, false);
                        self.data(c);
                    }
                }
            }
            Scan::EndOpen => {
                self.markup.push(c);
                if c.is_ascii_alphabetic() {
                    self.scan = Scan::Tag(TagState::Name);
                } else if c == '>' {
                    self.markup.clear();
                    self.scan = Scan::Data;
                } else {
                    self.scan = Scan::Bogus;
                }
            }
            Scan::Declaration => {
                self.markup.push(c);
                let lower = self.markup.to_ascii_lowercase();
                if self.markup == "<!--" {
                    self.scan = Scan::Comment(CommentState::Start);
                } else if lower == "<!doctype" {
                    self.scan = Scan::Doctype;
                } else if self.allow_cdata && self.markup == "<![CDATA[" {
                    self.markup.clear();
                    self.scan = Scan::Cdata;
                } else if !"<!--".starts_with(&self.markup)
                    && !"<!doctype".starts_with(&lower)
                    && !(self.allow_cdata && "<![CDATA[".starts_with(&self.markup))
                {
                    self.scan = Scan::Bogus;
                    if c == '>' {
                        self.frame();
                    }
                }
            }
            Scan::Tag(state) => {
                self.markup.push(c);
                self.tag_char(c, state);
            }
            Scan::Comment(state) => {
                self.markup.push(c);
                self.comment_char(c, state);
            }
            Scan::Doctype | Scan::Bogus => {
                self.markup.push(c);
                if c == '>' {
                    self.frame();
                }
            }
            Scan::Cdata => {
                if c == ']' {
                    self.cdata_tail.push(c);
                    if self.cdata_tail.len() > 2 {
                        self.text_char(']', false, false);
                        self.cdata_tail.pop();
                    }
                } else if c == '>' && self.cdata_tail == "]]" {
                    self.cdata_tail.clear();
                    self.scan = Scan::Data;
                    self.flush(true);
                } else {
                    let tail = std::mem::take(&mut self.cdata_tail);
                    self.text_str(&tail, false, false);
                    self.text_char(c, false, false);
                }
            }
        }
        if !end_raw {
            self.content = content;
        }
        if self.text.len() >= TEXT_BATCH {
            self.flush(false);
        }
        self.pop()
    }
    fn data(&mut self, c: char) {
        if c == '<' {
            self.flush(true);
            self.markup.push(c);
            self.scan = Scan::Open;
        } else {
            self.text_char(c, true, false);
        }
    }
    fn tag_char(&mut self, c: char, mut state: TagState) {
        loop {
            let whitespace = c.is_ascii() && space(c as u8);
            let mut again = false;
            let mut end = false;
            state = match state {
                TagState::Name => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    '/' => TagState::Slash,
                    _ if whitespace => TagState::BeforeAttr,
                    _ => state,
                },
                TagState::BeforeAttr => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    '/' => TagState::Slash,
                    _ if whitespace => state,
                    _ => TagState::AttrName,
                },
                TagState::AttrName => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    '/' => TagState::Slash,
                    '=' => TagState::BeforeValue,
                    _ if whitespace => TagState::AfterAttr,
                    _ => state,
                },
                TagState::AfterAttr => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    '/' => TagState::Slash,
                    '=' => TagState::BeforeValue,
                    _ if whitespace => state,
                    _ => TagState::AttrName,
                },
                TagState::BeforeValue => match c {
                    '\'' => TagState::Single,
                    '"' => TagState::Double,
                    '>' => {
                        end = true;
                        state
                    }
                    _ if whitespace => state,
                    _ => TagState::Unquoted,
                },
                TagState::Single => {
                    if c == '\'' {
                        TagState::AfterValue
                    } else {
                        state
                    }
                }
                TagState::Double => {
                    if c == '"' {
                        TagState::AfterValue
                    } else {
                        state
                    }
                }
                TagState::AfterValue => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    '/' => TagState::Slash,
                    _ if whitespace => TagState::BeforeAttr,
                    _ => {
                        again = true;
                        TagState::BeforeAttr
                    }
                },
                TagState::Unquoted => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    _ if whitespace => TagState::BeforeAttr,
                    _ => state,
                },
                TagState::Slash => {
                    if c == '>' {
                        end = true;
                        state
                    } else {
                        again = true;
                        TagState::BeforeAttr
                    }
                }
            };
            if end {
                self.frame();
                return;
            }
            if !again {
                self.scan = Scan::Tag(state);
                return;
            }
        }
    }
    fn comment_char(&mut self, c: char, mut state: CommentState) {
        loop {
            let mut again = false;
            let mut end = false;
            state = match state {
                CommentState::Start => match c {
                    '-' => CommentState::StartDash,
                    '>' => {
                        end = true;
                        state
                    }
                    _ => {
                        again = true;
                        CommentState::Data
                    }
                },
                CommentState::StartDash => match c {
                    '-' => CommentState::End,
                    '>' => {
                        end = true;
                        state
                    }
                    _ => {
                        again = true;
                        CommentState::Data
                    }
                },
                CommentState::Data => match c {
                    '<' => CommentState::Less,
                    '-' => CommentState::EndDash,
                    _ => state,
                },
                CommentState::Less => match c {
                    '!' => CommentState::LessBang,
                    '<' => state,
                    _ => {
                        again = true;
                        CommentState::Data
                    }
                },
                CommentState::LessBang => {
                    if c == '-' {
                        CommentState::LessBangDash
                    } else {
                        again = true;
                        CommentState::Data
                    }
                }
                CommentState::LessBangDash => {
                    if c == '-' {
                        CommentState::LessBangDashDash
                    } else {
                        again = true;
                        CommentState::EndDash
                    }
                }
                CommentState::LessBangDashDash => {
                    again = true;
                    CommentState::End
                }
                CommentState::EndDash => {
                    if c == '-' {
                        CommentState::End
                    } else {
                        again = true;
                        CommentState::Data
                    }
                }
                CommentState::End => match c {
                    '>' => {
                        end = true;
                        state
                    }
                    '!' => CommentState::EndBang,
                    '-' => state,
                    _ => {
                        again = true;
                        CommentState::Data
                    }
                },
                CommentState::EndBang => match c {
                    '-' => CommentState::EndDash,
                    '>' => {
                        end = true;
                        state
                    }
                    _ => {
                        again = true;
                        CommentState::Data
                    }
                },
            };
            if end {
                self.frame();
                return;
            }
            if !again {
                self.scan = Scan::Comment(state);
                return;
            }
        }
    }
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if matches!(self.scan, Scan::Cdata) {
            let tail = std::mem::take(&mut self.cdata_tail);
            self.text_str(&tail, false, false);
        }
        let candidate = std::mem::take(&mut self.raw_candidate);
        if !candidate.is_empty() {
            self.text_str(&candidate, matches!(self.content, Content::Rcdata(_)), true);
        }
        self.flush(true);
        if !self.markup.is_empty() {
            let mut tokenizer = Tokenizer::new(&self.markup);
            tokenizer.allow_cdata = self.allow_cdata;
            loop {
                let token = tokenizer.next();
                if matches!(token, Token::Eof) {
                    break;
                }
                self.output.push_back(token);
            }
            self.markup.clear();
        }
        self.output.push_back(Token::Eof);
    }
}
