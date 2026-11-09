//! HTML tokenizer.
//!
//! Implements the tokenization states needed by the tree builder: data,
//! tag/attribute parsing, comments, doctypes, character references, and the
//! RCDATA/RAWTEXT/script-data/PLAINTEXT modes. This is a pragmatic subset of
//! the WHATWG tokenizer aimed at real-world pages and malformed input.

use crate::engine::dom::Attribute;

use super::entities::{ENTITIES, MAX_ENTITY_LEN};

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
        force_quirks: bool,
    },
    StartTag {
        name: String,
        attrs: Vec<Attribute>,
        self_closing: bool,
    },
    EndTag {
        name: String,
    },
    Characters(String),
    Comment(String),
}

#[derive(Clone, Debug, PartialEq)]
enum RawtextMode {
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
}

#[derive(Clone, Debug)]
struct RawtextState {
    mode: RawtextMode,
    end_tag: String,
}

pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
    rawtext: Option<RawtextState>,
    eof_emitted: bool,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            rawtext: None,
            eof_emitted: false,
        }
    }

    /// Produce the next token, or `None` once the input is exhausted.
    pub fn next_token(&mut self) -> Option<Token> {
        if self.eof_emitted {
            return None;
        }
        if self.rawtext.is_some() {
            return self.rawtext_token();
        }
        self.data_token()
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn starts_with_ci(&self, needle: &str) -> bool {
        self.input[self.pos..]
            .get(..needle.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(needle))
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if is_html_space(ch) {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn data_token(&mut self) -> Option<Token> {
        loop {
            let Some(ch) = self.peek() else {
                self.eof_emitted = true;
                return None;
            };

            match ch {
                '<' => {
                    if self.starts_with_ci("<!--") {
                        return Some(self.consume_comment());
                    }
                    if self.starts_with_ci("<!doctype")
                        && self
                            .input
                            .get(self.pos + 9..)
                            .and_then(|s| s.chars().next())
                            .is_none_or(|c| is_html_space(c) || c == '>')
                    {
                        return Some(self.consume_doctype());
                    }
                    if self.starts_with_ci("<!") || self.starts_with_ci("<?") {
                        return Some(self.consume_bogus_comment());
                    }
                    if self.starts_with_ci("</") {
                        let after = self
                            .input
                            .get(self.pos + 2..)
                            .and_then(|s| s.chars().next());
                        match after {
                            Some(c) if c.is_ascii_alphabetic() => {
                                return Some(self.consume_end_tag());
                            }
                            _ => {
                                // `</>` and `</ ...` are bogus comments per spec.
                                return Some(self.consume_bogus_comment());
                            }
                        }
                    }
                    let after = self
                        .input
                        .get(self.pos + 1..)
                        .and_then(|s| s.chars().next());
                    if after.is_some_and(|c| c.is_ascii_alphabetic()) {
                        return Some(self.consume_start_tag());
                    }
                    self.bump();
                    return Some(Token::Characters("<".to_owned()));
                }
                '&' => {
                    if let Some((text, consumed)) = parse_char_ref_at(self.input, self.pos, false) {
                        self.pos += consumed;
                        return Some(Token::Characters(text));
                    }
                    self.bump();
                    return Some(Token::Characters("&".to_owned()));
                }
                _ => {
                    let mut text = String::new();
                    while let Some(c) = self.peek() {
                        if c == '<' || c == '&' {
                            break;
                        }
                        self.bump();
                        if c == '\0' {
                            text.push('\u{FFFD}');
                        } else {
                            text.push(c);
                        }
                    }
                    if !text.is_empty() {
                        return Some(Token::Characters(text));
                    }
                }
            }
        }
    }

    fn consume_comment(&mut self) -> Token {
        self.pos += 4; // <!--
        let rest = &self.input[self.pos..];
        if let Some(end) = rest.find("-->") {
            let data = rest[..end].to_owned();
            self.pos += end + 3;
            Token::Comment(data)
        } else {
            let data = rest.to_owned();
            self.pos = self.input.len();
            Token::Comment(data)
        }
    }

    fn consume_bogus_comment(&mut self) -> Token {
        // Consume `<!` or `<?`.
        self.bump();
        self.bump();
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '>' {
                break;
            }
            self.bump();
        }
        let data = self.input[start..self.pos].to_owned();
        self.bump(); // '>'
        Token::Comment(data)
    }

    fn consume_doctype(&mut self) -> Token {
        self.pos += 9; // <!DOCTYPE
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '>' {
                break;
            }
            self.bump();
        }
        let content = &self.input[start..self.pos];
        self.bump(); // '>'

        let mut parts = content.split_ascii_whitespace();
        let name = parts.next().unwrap_or("").to_ascii_lowercase();
        let rest: Vec<&str> = parts.collect();
        let mut public_id = String::new();
        let mut system_id = String::new();
        let mut force_quirks = content.is_empty();

        if let Some(keyword) = rest.first() {
            if keyword.eq_ignore_ascii_case("public") {
                let mut quoted = quoted_strings(&rest[1..]).into_iter();
                public_id = quoted.next().unwrap_or_default();
                system_id = quoted.next().unwrap_or_default();
                if public_id.is_empty() {
                    force_quirks = true;
                }
            } else if keyword.eq_ignore_ascii_case("system") {
                let mut quoted = quoted_strings(&rest[1..]).into_iter();
                system_id = quoted.next().unwrap_or_default();
                if system_id.is_empty() {
                    force_quirks = true;
                }
            }
        }

        Token::Doctype {
            name,
            public_id,
            system_id,
            force_quirks,
        }
    }

    fn consume_start_tag(&mut self) -> Token {
        self.bump(); // '<'
        let name = self.read_tag_name();
        let mut attrs: Vec<Attribute> = Vec::new();
        let mut self_closing = false;

        loop {
            self.skip_whitespace();
            match self.peek() {
                None => break,
                Some('>') => {
                    self.bump();
                    break;
                }
                Some('/') => {
                    self.bump();
                    if self.peek() == Some('>') {
                        self.bump();
                        self_closing = true;
                        break;
                    }
                    continue;
                }
                Some(_) => {
                    let mut attr_name = String::new();
                    while let Some(ch) = self.peek() {
                        if is_html_space(ch) || ch == '=' || ch == '>' || ch == '/' {
                            break;
                        }
                        self.bump();
                        attr_name.push(if ch == '\0' { '\u{FFFD}' } else { ch });
                    }
                    if attr_name.is_empty() {
                        // Stray '=' or similar; skip it to guarantee progress.
                        self.bump();
                        continue;
                    }
                    let attr_name = attr_name.to_ascii_lowercase();

                    self.skip_whitespace();
                    let mut value = String::new();
                    if self.peek() == Some('=') {
                        self.bump();
                        self.skip_whitespace();
                        match self.peek() {
                            Some(quote @ ('"' | '\'')) => {
                                self.bump();
                                while let Some(ch) = self.peek() {
                                    if ch == quote {
                                        self.bump();
                                        break;
                                    }
                                    if ch == '&'
                                        && let Some((text, consumed)) =
                                            parse_char_ref_at(self.input, self.pos, true)
                                    {
                                        self.pos += consumed;
                                        value.push_str(&text);
                                        continue;
                                    }
                                    self.bump();
                                    value.push(if ch == '\0' { '\u{FFFD}' } else { ch });
                                }
                            }
                            _ => {
                                while let Some(ch) = self.peek() {
                                    if is_html_space(ch) || ch == '>' {
                                        break;
                                    }
                                    if ch == '&'
                                        && let Some((text, consumed)) =
                                            parse_char_ref_at(self.input, self.pos, true)
                                    {
                                        self.pos += consumed;
                                        value.push_str(&text);
                                        continue;
                                    }
                                    self.bump();
                                    value.push(if ch == '\0' { '\u{FFFD}' } else { ch });
                                }
                            }
                        }
                    }

                    if !attrs.iter().any(|a| a.name == attr_name) {
                        attrs.push(Attribute::new(attr_name, value));
                    }
                }
            }
        }

        match name.as_str() {
            "title" | "textarea" => self.enter_rawtext(RawtextMode::Rcdata, &name),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                self.enter_rawtext(RawtextMode::Rawtext, &name)
            }
            "script" => self.enter_rawtext(RawtextMode::ScriptData, &name),
            "plaintext" => self.enter_rawtext(RawtextMode::Plaintext, &name),
            _ => {}
        }

        Token::StartTag {
            name,
            attrs,
            self_closing,
        }
    }

    fn enter_rawtext(&mut self, mode: RawtextMode, name: &str) {
        self.rawtext = Some(RawtextState {
            mode,
            end_tag: name.to_owned(),
        });
    }

    fn consume_end_tag(&mut self) -> Token {
        self.bump(); // '<'
        self.bump(); // '/'
        let name = self.read_tag_name();
        // Ignore attributes on end tags; consume up to '>'.
        while let Some(ch) = self.peek() {
            self.bump();
            if ch == '>' {
                break;
            }
        }
        Token::EndTag { name }
    }

    fn read_tag_name(&mut self) -> String {
        let mut name = String::new();
        while let Some(ch) = self.peek() {
            if is_html_space(ch) || ch == '/' || ch == '>' {
                break;
            }
            self.bump();
            name.push(if ch == '\0' {
                '\u{FFFD}'
            } else {
                ch.to_ascii_lowercase()
            });
        }
        name
    }

    fn rawtext_token(&mut self) -> Option<Token> {
        let state = self.rawtext.clone()?;

        if matches!(state.mode, RawtextMode::Plaintext) {
            if self.pos >= self.input.len() {
                self.eof_emitted = true;
                self.rawtext = None;
                return None;
            }
            let text = self.input[self.pos..].replace('\0', "\u{FFFD}");
            self.pos = self.input.len();
            self.rawtext = None;
            return Some(Token::Characters(text));
        }

        if let Some(index) = find_end_tag(self.input, self.pos, &state.end_tag) {
            let text = &self.input[self.pos..index];
            self.pos = index;
            self.rawtext = None;
            if text.is_empty() {
                return Some(self.consume_end_tag());
            }
            let decoded = if matches!(state.mode, RawtextMode::Rcdata) {
                decode_entities_in_text(text)
            } else {
                text.replace('\0', "\u{FFFD}")
            };
            return Some(Token::Characters(decoded));
        }

        // No matching end tag; emit the remainder as text.
        if self.pos >= self.input.len() {
            self.eof_emitted = true;
            self.rawtext = None;
            return None;
        }
        let text = &self.input[self.pos..];
        self.pos = self.input.len();
        self.rawtext = None;
        let decoded = if matches!(state.mode, RawtextMode::Rcdata) {
            decode_entities_in_text(text)
        } else {
            text.replace('\0', "\u{FFFD}")
        };
        Some(Token::Characters(decoded))
    }
}

fn is_html_space(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

fn quoted_strings(parts: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for part in parts {
        for ch in part.chars() {
            match quote {
                Some(q) => {
                    if ch == q {
                        out.push(std::mem::take(&mut current));
                        quote = None;
                    } else {
                        current.push(ch);
                    }
                }
                None => {
                    if ch == '"' || ch == '\'' {
                        quote = Some(ch);
                    }
                }
            }
        }
        if quote.is_some() {
            current.push(' ');
        }
    }
    out
}

/// Find `</name` (case-insensitive) followed by whitespace, `/` or `>`.
fn find_end_tag(input: &str, from: usize, name: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'/' {
            let rest = &input[i + 2..];
            if rest.len() >= name.len() && rest[..name.len()].eq_ignore_ascii_case(name) {
                let after = rest[name.len()..].chars().next();
                if after.is_none_or(|c| is_html_space(c) || c == '/' || c == '>') {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Decode character references in a plain text run (used for RCDATA).
pub fn decode_entities_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let Some(ch) = rest.chars().next() else { break };
        if ch == '&'
            && let Some((decoded, consumed)) = parse_char_ref_at(text, i, false)
        {
            out.push_str(&decoded);
            i += consumed;
            continue;
        }
        out.push(if ch == '\0' { '\u{FFFD}' } else { ch });
        i += ch.len_utf8();
    }
    out
}

/// Parse a character reference starting at `amp` (which must be `&`).
/// Returns the decoded text and the number of bytes consumed.
pub fn parse_char_ref_at(input: &str, amp: usize, in_attribute: bool) -> Option<(String, usize)> {
    if input.as_bytes().get(amp) != Some(&b'&') {
        return None;
    }
    let rest = &input[amp + 1..];
    let first = rest.chars().next()?;

    if first == '#' {
        let hex = rest[1..]
            .chars()
            .next()
            .is_some_and(|c| c == 'x' || c == 'X');
        let mut idx = if hex { 2 } else { 1 };
        let mut value: u32 = 0;
        let mut digits = 0usize;
        while let Some(ch) = rest[idx..].chars().next() {
            let Some(digit) = ch.to_digit(if hex { 16 } else { 10 }) else {
                break;
            };
            value = value
                .saturating_mul(if hex { 16 } else { 10 })
                .saturating_add(digit);
            idx += ch.len_utf8();
            digits += 1;
        }
        if digits == 0 {
            return None;
        }
        let mut consumed = 1 + idx;
        if rest[idx..].starts_with(';') {
            consumed += 1;
        }
        return Some((numeric_reference(value), consumed));
    }

    if !first.is_ascii_alphanumeric() {
        return None;
    }

    let mut boundaries = Vec::new();
    for (i, ch) in rest.char_indices() {
        let end = i + ch.len_utf8();
        if end > MAX_ENTITY_LEN {
            break;
        }
        boundaries.push(end);
    }

    for &len in boundaries.iter().rev() {
        let candidate = &rest[..len];
        let Ok(index) = ENTITIES.binary_search_by(|entry| entry.0.cmp(candidate)) else {
            continue;
        };
        let (name, value) = ENTITIES[index];
        let has_semicolon = name.ends_with(';');
        if !has_semicolon
            && in_attribute
            && let Some(next) = rest[len..].chars().next()
            && (next.is_ascii_alphanumeric() || next == '=')
        {
            return None;
        }
        return Some((value.to_owned(), 1 + len));
    }
    None
}

fn numeric_reference(value: u32) -> String {
    if value == 0 || value > 0x0010_FFFF || (0xD800..=0xDFFF).contains(&value) {
        return "\u{FFFD}".to_owned();
    }
    if let Some(replacement) = c1_replacement(value) {
        return replacement.to_owned();
    }
    char::from_u32(value)
        .map(|c| c.to_string())
        .unwrap_or_else(|| "\u{FFFD}".to_owned())
}

fn c1_replacement(value: u32) -> Option<&'static str> {
    Some(match value {
        0x80 => "€",
        0x82 => "‚",
        0x83 => "ƒ",
        0x84 => "„",
        0x85 => "…",
        0x86 => "†",
        0x87 => "‡",
        0x88 => "ˆ",
        0x89 => "‰",
        0x8A => "Š",
        0x8B => "‹",
        0x8C => "Œ",
        0x8E => "Ž",
        0x91 => "‘",
        0x92 => "’",
        0x93 => "“",
        0x94 => "”",
        0x95 => "•",
        0x96 => "–",
        0x97 => "—",
        0x98 => "˜",
        0x99 => "™",
        0x9A => "š",
        0x9B => "›",
        0x9C => "œ",
        0x9E => "ž",
        0x9F => "Ÿ",
        _ => return None,
    })
}
