//! CSS tokenizer (a practical subset of CSS Syntax Level 3).

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Ident(String),
    /// A function name with its opening parenthesis already consumed.
    Function(String),
    AtKeyword(String),
    Hash {
        value: String,
        id: bool,
    },
    StringLiteral(String),
    Url(String),
    Delim(char),
    Number(f64),
    Percentage(f64),
    Dimension {
        value: f64,
        unit: String,
    },
    Whitespace,
    Colon,
    Semicolon,
    Comma,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Cdo,
    Cdc,
}

pub struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    pub fn next_token(&mut self) -> Option<Token> {
        let ch = self.peek()?;
        Some(match ch {
            '/' if self.starts_with("/*") => {
                self.consume_comment();
                Token::Whitespace
            }
            ch if ch.is_ascii_whitespace() => {
                while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                    self.bump();
                }
                Token::Whitespace
            }
            '"' | '\'' => self.consume_string(ch),
            '@' => {
                self.bump();
                if self.peek().is_some_and(is_name_start) {
                    Token::AtKeyword(self.consume_name())
                } else {
                    Token::Delim('@')
                }
            }
            '#' => {
                self.bump();
                if self.peek().is_some_and(is_name_char) {
                    let id = self.peek().is_some_and(is_name_start);
                    Token::Hash {
                        value: self.consume_name(),
                        id,
                    }
                } else {
                    Token::Delim('#')
                }
            }
            '<' if self.starts_with("<!--") => {
                self.pos += 4;
                Token::Cdo
            }
            '-' if self.starts_with("-->") => {
                self.pos += 3;
                Token::Cdc
            }
            '+' | '.' | '-' | '0'..='9' if self.starts_number() => {
                let number = self.consume_number();
                if self.peek() == Some('%') {
                    self.bump();
                    Token::Percentage(number)
                } else if self.peek().is_some_and(is_name_start) {
                    let unit = self.consume_name().to_ascii_lowercase();
                    Token::Dimension {
                        value: number,
                        unit,
                    }
                } else {
                    Token::Number(number)
                }
            }
            '-' if self
                .peek_nth(1)
                .is_some_and(|c| is_name_start(c) || c == '-') =>
            {
                self.consume_ident()
            }
            '-' => {
                self.bump();
                Token::Delim('-')
            }
            '_' | 'a'..='z' | 'A'..='Z' | '\\' | '\u{80}'..=char::MAX => self.consume_ident(),
            ':' => {
                self.bump();
                Token::Colon
            }
            ';' => {
                self.bump();
                Token::Semicolon
            }
            ',' => {
                self.bump();
                Token::Comma
            }
            '{' => {
                self.bump();
                Token::LBrace
            }
            '}' => {
                self.bump();
                Token::RBrace
            }
            '(' => {
                self.bump();
                Token::LParen
            }
            ')' => {
                self.bump();
                Token::RParen
            }
            '[' => {
                self.bump();
                Token::LBracket
            }
            ']' => {
                self.bump();
                Token::RBracket
            }
            other => {
                self.bump();
                Token::Delim(other)
            }
        })
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.input[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn starts_with(&self, needle: &str) -> bool {
        self.input[self.pos..].starts_with(needle)
    }

    fn consume_comment(&mut self) {
        self.pos += 2; // /*
        if let Some(end) = self.input[self.pos..].find("*/") {
            self.pos += end + 2;
        } else {
            self.pos = self.input.len();
        }
    }

    fn starts_number(&self) -> bool {
        match self.peek() {
            Some('0'..='9') => true,
            Some('.') => self.peek_nth(1).is_some_and(|c| c.is_ascii_digit()),
            Some('+' | '-') => match self.peek_nth(1) {
                Some('0'..='9') => true,
                Some('.') => self.peek_nth(2).is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            },
            _ => false,
        }
    }

    fn consume_number(&mut self) -> f64 {
        let start = self.pos;
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
        if self.peek() == Some('.') && self.peek_nth(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let has_exponent = match self.peek_nth(1) {
                Some('0'..='9') => true,
                Some('+' | '-') => self.peek_nth(2).is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            };
            if has_exponent {
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            }
        }
        self.input[start..self.pos].parse().unwrap_or(0.0)
    }

    fn consume_ident(&mut self) -> Token {
        let name = self.consume_name();
        if self.peek() == Some('(') {
            if name.eq_ignore_ascii_case("url") {
                let saved = self.pos;
                self.bump(); // '('
                let mut look = self.pos;
                while let Some(c) = self.input[look..].chars().next() {
                    if c.is_ascii_whitespace() {
                        look += c.len_utf8();
                    } else {
                        break;
                    }
                }
                let next = self.input[look..].chars().next();
                if matches!(next, Some('"' | '\'')) {
                    return Token::Function(name);
                }
                let rest = &self.input[self.pos..];
                if let Some(end) = rest.find(')') {
                    let raw = rest[..end].trim();
                    let value = decode_escapes(raw);
                    self.pos += end + 1;
                    return Token::Url(value);
                }
                self.pos = saved;
                return Token::Ident(name);
            }
            self.bump();
            return Token::Function(name);
        }
        Token::Ident(name)
    }

    fn consume_name(&mut self) -> String {
        let mut out = String::new();
        while let Some(ch) = self.peek() {
            if is_name_char(ch) {
                out.push(ch);
                self.bump();
            } else if ch == '\\' && self.peek_nth(1).is_some_and(|c| c != '\n') {
                self.bump();
                out.push(self.consume_escape());
            } else {
                break;
            }
        }
        out
    }

    fn consume_escape(&mut self) -> char {
        let Some(ch) = self.peek() else {
            return '\u{FFFD}';
        };
        if ch.is_ascii_hexdigit() {
            let mut value: u32 = 0;
            let mut digits = 0;
            while digits < 6 {
                let Some(c) = self.peek() else { break };
                let Some(digit) = c.to_digit(16) else { break };
                value = value * 16 + digit;
                self.bump();
                digits += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                self.bump();
            }
            char::from_u32(value).unwrap_or('\u{FFFD}')
        } else {
            self.bump();
            ch
        }
    }

    fn consume_string(&mut self, quote: char) -> Token {
        self.bump();
        let mut out = String::new();
        while let Some(ch) = self.peek() {
            if ch == quote {
                self.bump();
                return Token::StringLiteral(out);
            }
            if ch == '\\' {
                self.bump();
                match self.peek() {
                    Some('\n') => {
                        self.bump();
                    }
                    Some(_) => out.push(self.consume_escape()),
                    None => {}
                }
                continue;
            }
            if ch == '\n' {
                // Unterminated string; stop at the newline.
                return Token::StringLiteral(out);
            }
            out.push(ch);
            self.bump();
        }
        Token::StringLiteral(out)
    }
}

fn is_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || !ch.is_ascii() || ch == '\\'
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || !ch.is_ascii() || ch == '\\'
}

fn decode_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some(next) = chars.peek().copied() else {
            break;
        };
        if next.is_ascii_hexdigit() {
            let mut value = 0u32;
            let mut digits = 0;
            while digits < 6 {
                let Some(c) = chars.peek().copied() else {
                    break;
                };
                let Some(digit) = c.to_digit(16) else { break };
                value = value * 16 + digit;
                chars.next();
                digits += 1;
            }
            if chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                chars.next();
            }
            out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
        } else {
            out.push(chars.next().unwrap_or('\u{FFFD}'));
        }
    }
    out
}

/// Convenience helper for tests and small snippets.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokenizer = Tokenizer::new(input);
    let mut out = Vec::new();
    while let Some(token) = tokenizer.next_token() {
        out.push(token);
    }
    out
}
