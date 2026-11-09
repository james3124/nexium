//! CSS stylesheet parser producing rules, declarations and component values.

use crate::engine::dom::{ComplexSelector, parse_selector_list};

use super::tokenizer::{Token, Tokenizer};

#[derive(Clone, Debug, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug)]
pub enum Rule {
    Style(StyleRule),
    Media(MediaRule),
    Import(ImportRule),
    Supports(SupportsRule),
}

#[derive(Clone, Debug)]
pub struct ImportRule {
    pub url: String,
    pub media: Vec<MediaQuery>,
}

#[derive(Clone, Debug)]
pub struct SupportsRule {
    pub condition: SupportsCondition,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug)]
pub enum SupportsCondition {
    Declaration {
        name: String,
        value: Vec<ComponentValue>,
    },
    And(Vec<SupportsCondition>),
    Or(Vec<SupportsCondition>),
    Not(Box<SupportsCondition>),
    Never,
}

#[derive(Clone, Debug)]
pub struct StyleRule {
    pub selectors: Vec<ComplexSelector>,
    pub declarations: Vec<Declaration>,
}

#[derive(Clone, Debug)]
pub struct MediaRule {
    pub queries: Vec<MediaQuery>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Default)]
pub struct MediaQuery {
    pub negated: bool,
    pub media_type: Option<String>,
    pub conditions: Vec<MediaCondition>,
    pub unsupported: bool,
}

#[derive(Clone, Debug)]
pub struct MediaCondition {
    pub feature: MediaFeature,
    pub kind: MediaKind,
    pub value: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaFeature {
    Width,
    Height,
    Orientation,
    AspectRatio,
    ColorScheme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Min,
    Max,
    Exact,
}

impl MediaQuery {
    pub fn matches(&self, width: f32, height: f32) -> bool {
        if self.unsupported {
            return false;
        }
        if let Some(media_type) = &self.media_type
            && !matches!(media_type.as_str(), "all" | "screen")
        {
            return false;
        }
        let satisfies = self.conditions.iter().all(|condition| {
            let actual = match condition.feature {
                MediaFeature::Width => width,
                MediaFeature::Height => height,
                MediaFeature::Orientation => {
                    if width > height {
                        2.0
                    } else {
                        1.0
                    }
                }
                MediaFeature::AspectRatio => {
                    if height > 0.0 {
                        width / height
                    } else {
                        1.0
                    }
                }
                MediaFeature::ColorScheme => 1.0,
            };
            match condition.kind {
                MediaKind::Min => actual >= condition.value,
                MediaKind::Max => actual <= condition.value,
                MediaKind::Exact => {
                    if matches!(condition.feature, MediaFeature::AspectRatio) {
                        (actual - condition.value).abs() < 0.01
                    } else {
                        (actual - condition.value).abs() < 0.5
                    }
                }
            }
        });
        if self.negated { !satisfies } else { satisfies }
    }
}

impl MediaRule {
    pub fn matches(&self, width: f32, height: f32) -> bool {
        self.queries
            .iter()
            .any(|query| query.matches(width, height))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Vec<ComponentValue>,
    pub important: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ComponentValue {
    Ident(String),
    Function {
        name: String,
        args: Vec<ComponentValue>,
    },
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
    Comma,
    LParen,
    RParen,
    LBracket,
    RBracket,
}

pub fn parse_stylesheet(input: &str) -> Stylesheet {
    let tokens = tokenize(input);
    let mut parser = Parser { tokens, pos: 0 };
    Stylesheet {
        rules: parser.parse_rule_list(false),
    }
}

/// Parse a declaration list (e.g. the contents of a `style` attribute).
pub fn parse_declaration_list(input: &str) -> Vec<Declaration> {
    let tokens = tokenize(input);
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse_declarations()
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokenizer = Tokenizer::new(input);
    let mut tokens = Vec::new();
    while let Some(token) = tokenizer.next_token() {
        tokens.push(token);
    }
    tokens
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        token
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(Token::Whitespace)) {
            self.pos += 1;
        }
    }

    fn parse_rule_list(&mut self, inside_block: bool) -> Vec<Rule> {
        let mut rules = Vec::new();
        loop {
            self.skip_whitespace();
            while matches!(self.peek(), Some(Token::Semicolon)) {
                self.pos += 1;
                self.skip_whitespace();
            }
            let Some(token) = self.peek() else {
                break;
            };
            if inside_block && matches!(token, Token::RBrace) {
                break;
            }
            match token.clone() {
                Token::AtKeyword(name) => {
                    self.pos += 1;
                    if let Some(rule) = self.parse_at_rule(&name) {
                        rules.push(rule);
                    }
                }
                Token::RBrace => {
                    // Stray closing brace at top level: skip it.
                    self.pos += 1;
                }
                _ => {
                    if let Some(rule) = self.parse_style_rule() {
                        rules.push(rule);
                    }
                }
            }
        }
        rules
    }

    fn parse_at_rule(&mut self, name: &str) -> Option<Rule> {
        let prelude_start = self.pos;
        let mut depth = 0usize;
        let mut end = None;
        for index in self.pos..self.tokens.len() {
            match &self.tokens[index] {
                Token::Function(_) | Token::LParen => depth += 1,
                Token::RParen => depth = depth.saturating_sub(1),
                Token::Semicolon if depth == 0 => {
                    end = Some(index);
                    break;
                }
                Token::LBrace if depth == 0 => {
                    end = Some(index);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            self.pos = self.tokens.len();
            return None;
        };

        let prelude = self.tokens[prelude_start..end].to_vec();
        self.pos = end;

        if name.eq_ignore_ascii_case("import") {
            let mut url = None;
            let mut media_start = prelude.len();
            for (index, token) in prelude.iter().enumerate() {
                if let Token::Url(value) = token {
                    url = Some(value.clone());
                    media_start = index + 1;
                    break;
                }
            }
            if url.is_none() {
                for (index, token) in prelude.iter().enumerate() {
                    if let Token::StringLiteral(value) = token {
                        url = Some(value.clone());
                        media_start = index + 1;
                        while matches!(prelude.get(media_start), Some(Token::RParen)) {
                            media_start += 1;
                        }
                        break;
                    }
                }
            }
            if matches!(self.peek(), Some(Token::Semicolon)) {
                self.pos += 1;
            }
            return url.map(|url| {
                Rule::Import(ImportRule {
                    url,
                    media: parse_media_prelude(&prelude[media_start..]),
                })
            });
        }

        if name.eq_ignore_ascii_case("supports") {
            if !matches!(self.peek(), Some(Token::LBrace)) {
                if matches!(self.peek(), Some(Token::Semicolon)) {
                    self.pos += 1;
                }
                return None;
            }
            self.pos += 1;
            let rules = self.parse_rule_list(true);
            self.expect_rbrace();
            return Some(Rule::Supports(SupportsRule {
                condition: parse_supports_prelude(&prelude),
                rules,
            }));
        }

        if name.eq_ignore_ascii_case("media") {
            let opening = matches!(self.peek(), Some(Token::LBrace));
            if !opening {
                self.pos += 1; // semicolon
                return None;
            }
            self.pos += 1;
            let rules = self.parse_rule_list(true);
            self.expect_rbrace();
            let queries = parse_media_prelude(&prelude);
            if queries.is_empty() {
                return None;
            }
            return Some(Rule::Media(MediaRule { queries, rules }));
        }

        // Unsupported at-rules: skip a declaration or a whole block.
        match self.peek() {
            Some(Token::Semicolon) => self.pos += 1,
            Some(Token::LBrace) => {
                self.pos += 1;
                self.skip_block();
            }
            _ => {}
        }
        None
    }

    fn skip_block(&mut self) {
        let mut depth = 1usize;
        while let Some(token) = self.bump() {
            match token {
                Token::LBrace => depth += 1,
                Token::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    fn expect_rbrace(&mut self) {
        if matches!(self.peek(), Some(Token::RBrace)) {
            self.pos += 1;
        }
    }

    fn parse_style_rule(&mut self) -> Option<Rule> {
        let start = self.pos;
        let mut depth = 0usize;
        let mut brace = None;
        for index in self.pos..self.tokens.len() {
            match &self.tokens[index] {
                Token::Function(_) | Token::LParen => depth += 1,
                Token::RParen => depth = depth.saturating_sub(1),
                Token::LBrace if depth == 0 => {
                    brace = Some(index);
                    break;
                }
                Token::RBrace if depth == 0 => break,
                _ => {}
            }
        }
        let Some(brace) = brace else {
            // Malformed rule without a block: skip to a safe point.
            self.pos = start;
            self.bump();
            return None;
        };
        let selector_tokens = self.tokens[start..brace].to_vec();
        self.pos = brace + 1;

        let declarations = self.parse_declarations();
        self.expect_rbrace();

        let selector_text = tokens_to_text(&selector_tokens);
        let selectors = match parse_selector_list(selector_text.trim()) {
            Ok(selectors) => selectors,
            Err(_) => return None,
        };
        if selectors.is_empty() {
            return None;
        }
        Some(Rule::Style(StyleRule {
            selectors,
            declarations,
        }))
    }

    fn parse_declarations(&mut self) -> Vec<Declaration> {
        let mut declarations = Vec::new();
        loop {
            self.skip_whitespace();
            while matches!(self.peek(), Some(Token::Semicolon)) {
                self.pos += 1;
                self.skip_whitespace();
            }
            let Some(token) = self.peek().cloned() else {
                break;
            };
            if matches!(token, Token::RBrace) {
                break;
            }

            // Property name.
            let name = match token {
                Token::Ident(name) => {
                    self.pos += 1;
                    name
                }
                _ => {
                    // Skip junk until the next declaration boundary.
                    self.skip_to_declaration_end();
                    continue;
                }
            };
            self.skip_whitespace();
            if !matches!(self.peek(), Some(Token::Colon)) {
                self.skip_to_declaration_end();
                continue;
            }
            self.pos += 1;

            // Value tokens until `;` or `}` at top level.
            let value_start = self.pos;
            let mut depth = 0usize;
            let mut end = self.tokens.len();
            for index in self.pos..self.tokens.len() {
                match &self.tokens[index] {
                    Token::Function(_) | Token::LParen | Token::LBracket => depth += 1,
                    Token::RParen | Token::RBracket => depth = depth.saturating_sub(1),
                    Token::Semicolon if depth == 0 => {
                        end = index;
                        break;
                    }
                    Token::RBrace if depth == 0 => {
                        end = index;
                        break;
                    }
                    _ => {}
                }
            }
            let value_tokens = self.tokens[value_start..end].to_vec();
            self.pos = end;
            if matches!(self.peek(), Some(Token::Semicolon)) {
                self.pos += 1;
            }

            let mut values = parse_component_values(&value_tokens);
            let important = take_important(&mut values);
            let name = if name.starts_with("--") {
                name
            } else {
                name.to_ascii_lowercase()
            };
            declarations.push(Declaration {
                name,
                value: values,
                important,
            });
        }
        declarations
    }

    fn skip_to_declaration_end(&mut self) {
        let mut depth = 0usize;
        while let Some(token) = self.peek() {
            match token {
                Token::Function(_) | Token::LParen | Token::LBracket => depth += 1,
                Token::RParen | Token::RBracket => depth = depth.saturating_sub(1),
                Token::Semicolon | Token::RBrace if depth == 0 => break,
                _ => {}
            }
            self.pos += 1;
        }
        if matches!(self.peek(), Some(Token::Semicolon)) {
            self.pos += 1;
        }
    }
}

fn parse_component_values(tokens: &[Token]) -> Vec<ComponentValue> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].clone() {
            Token::Function(name) => {
                i += 1;
                let (args, next) = parse_function_args(tokens, i);
                i = next;
                out.push(ComponentValue::Function { name, args });
            }
            token => {
                out.push(token_to_component(token));
                i += 1;
            }
        }
    }
    out
}

fn parse_function_args(tokens: &[Token], mut i: usize) -> (Vec<ComponentValue>, usize) {
    let mut out = Vec::new();
    let mut depth = 0usize;
    while i < tokens.len() {
        match tokens[i].clone() {
            Token::RParen if depth == 0 => {
                i += 1;
                break;
            }
            Token::RParen => {
                depth -= 1;
                out.push(ComponentValue::RParen);
                i += 1;
            }
            Token::LParen => {
                depth += 1;
                out.push(ComponentValue::LParen);
                i += 1;
            }
            Token::Function(name) => {
                i += 1;
                let (args, next) = parse_function_args(tokens, i);
                i = next;
                out.push(ComponentValue::Function { name, args });
            }
            token => {
                out.push(token_to_component(token));
                i += 1;
            }
        }
    }
    (out, i)
}

fn token_to_component(token: Token) -> ComponentValue {
    match token {
        Token::Ident(v) => ComponentValue::Ident(v),
        Token::Function(name) => ComponentValue::Function {
            name,
            args: Vec::new(),
        },
        Token::AtKeyword(v) => ComponentValue::Ident(format!("@{v}")),
        Token::Hash { value, id } => ComponentValue::Hash { value, id },
        Token::StringLiteral(v) => ComponentValue::StringLiteral(v),
        Token::Url(v) => ComponentValue::Url(v),
        Token::Delim(c) => ComponentValue::Delim(c),
        Token::Number(n) => ComponentValue::Number(n),
        Token::Percentage(n) => ComponentValue::Percentage(n),
        Token::Dimension { value, unit } => ComponentValue::Dimension { value, unit },
        Token::Whitespace => ComponentValue::Whitespace,
        Token::Colon => ComponentValue::Colon,
        Token::Semicolon => ComponentValue::Delim(';'),
        Token::Comma => ComponentValue::Comma,
        Token::LBrace => ComponentValue::Delim('{'),
        Token::RBrace => ComponentValue::Delim('}'),
        Token::LParen => ComponentValue::LParen,
        Token::RParen => ComponentValue::RParen,
        Token::LBracket => ComponentValue::LBracket,
        Token::RBracket => ComponentValue::RBracket,
        Token::Cdo => ComponentValue::Delim('<'),
        Token::Cdc => ComponentValue::Delim('>'),
    }
}

fn take_important(values: &mut Vec<ComponentValue>) -> bool {
    let significant: Vec<usize> = values
        .iter()
        .enumerate()
        .filter(|(_, value)| !matches!(value, ComponentValue::Whitespace))
        .map(|(index, _)| index)
        .collect();
    if significant.len() >= 2 {
        let bang = significant[significant.len() - 2];
        let keyword = significant[significant.len() - 1];
        let is_bang = matches!(values[bang], ComponentValue::Delim('!'));
        let is_important = matches!(
            &values[keyword],
            ComponentValue::Ident(text) if text.eq_ignore_ascii_case("important")
        );
        if is_bang && is_important {
            values.truncate(bang);
            return true;
        }
    }
    false
}

fn tokens_to_text(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Ident(value) => out.push_str(value),
            Token::Function(name) => {
                out.push_str(name);
                out.push('(');
            }
            Token::AtKeyword(name) => {
                out.push('@');
                out.push_str(name);
            }
            Token::Hash { value, .. } => {
                out.push('#');
                out.push_str(value);
            }
            Token::StringLiteral(value) => {
                out.push('"');
                out.push_str(&value.replace('"', "\\\""));
                out.push('"');
            }
            Token::Url(value) => {
                out.push_str("url(");
                out.push_str(value);
                out.push(')');
            }
            Token::Delim(c) => out.push(*c),
            Token::Number(n) => out.push_str(&format_number(*n)),
            Token::Percentage(n) => {
                out.push_str(&format_number(*n));
                out.push('%');
            }
            Token::Dimension { value, unit } => {
                out.push_str(&format_number(*value));
                out.push_str(unit);
            }
            Token::Whitespace => out.push(' '),
            Token::Colon => out.push(':'),
            Token::Semicolon => out.push(';'),
            Token::Comma => out.push(','),
            Token::LBrace => out.push('{'),
            Token::RBrace => out.push('}'),
            Token::LParen => out.push('('),
            Token::RParen => out.push(')'),
            Token::LBracket => out.push('['),
            Token::RBracket => out.push(']'),
            Token::Cdo | Token::Cdc => {}
        }
    }
    out
}

pub fn format_number(value: f64) -> String {
    if value.fract().abs() < f32::EPSILON as f64 {
        format!("{}", value as i64)
    } else {
        let text = format!("{value:.4}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn parse_media_prelude(tokens: &[Token]) -> Vec<MediaQuery> {
    let text = tokens_to_text(tokens);
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_single_media_query)
        .collect()
}

fn parse_single_media_query(input: &str) -> MediaQuery {
    let mut query = MediaQuery::default();
    let mut text = input.to_ascii_lowercase();
    text = text.trim().to_owned();

    if let Some(rest) = text.strip_prefix("not ") {
        query.negated = true;
        text = rest.trim().to_owned();
    }
    if let Some(rest) = text.strip_prefix("only ") {
        text = rest.trim().to_owned();
    }

    let mut depth = 0usize;
    let mut condition_start = None;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => {
                if depth == 0 {
                    condition_start = Some(index);
                }
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(start) = condition_start.take()
                {
                    let condition = &text[start + 1..index];
                    match parse_media_condition(condition) {
                        Some(condition) => query.conditions.push(condition),
                        None => query.unsupported = true,
                    }
                }
            }
            _ => {}
        }
    }

    let prelude = text.split('(').next().unwrap_or("").trim();
    let media_type = prelude
        .split_whitespace()
        .next()
        .filter(|word| *word != "and")
        .map(str::to_owned);
    if let Some(media_type) = media_type {
        if !matches!(media_type.as_str(), "all" | "screen" | "print") {
            query.unsupported = true;
        }
        query.media_type = Some(media_type);
    }

    query
}

fn parse_media_condition(input: &str) -> Option<MediaCondition> {
    let (name, value) = input.split_once(':')?;
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim().to_ascii_lowercase();
    let (kind, feature) = if let Some(rest) = name.strip_prefix("min-") {
        (MediaKind::Min, rest)
    } else if let Some(rest) = name.strip_prefix("max-") {
        (MediaKind::Max, rest)
    } else {
        (MediaKind::Exact, name.as_str())
    };
    match feature {
        "width" => Some(MediaCondition {
            feature: MediaFeature::Width,
            kind,
            value: parse_media_length(&value)?,
        }),
        "height" => Some(MediaCondition {
            feature: MediaFeature::Height,
            kind,
            value: parse_media_length(&value)?,
        }),
        "aspect-ratio" => {
            let (width, height) = value.split_once('/')?;
            let width: f32 = width.trim().parse().ok()?;
            let height: f32 = height.trim().parse().ok()?;
            if height == 0.0 {
                return None;
            }
            Some(MediaCondition {
                feature: MediaFeature::AspectRatio,
                kind,
                value: width / height,
            })
        }
        "orientation" => {
            let value = match value.as_str() {
                "portrait" => 1.0,
                "landscape" => 2.0,
                _ => return None,
            };
            Some(MediaCondition {
                feature: MediaFeature::Orientation,
                kind: MediaKind::Exact,
                value,
            })
        }
        "prefers-color-scheme" => {
            let value = match value.as_str() {
                "light" => 1.0,
                "dark" => 0.0,
                _ => return None,
            };
            Some(MediaCondition {
                feature: MediaFeature::ColorScheme,
                kind: MediaKind::Exact,
                value,
            })
        }
        _ => None,
    }
}

fn parse_supports_prelude(tokens: &[Token]) -> SupportsCondition {
    parse_supports_text(tokens_to_text(tokens).trim())
}

fn parse_supports_text(input: &str) -> SupportsCondition {
    let input = strip_outer_parens(input);
    let lower = input.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("not ") {
        let offset = input.len() - rest.len();
        return SupportsCondition::Not(Box::new(parse_supports_text(&input[offset..])));
    }
    if let Some(parts) = split_top_level_word(input, "or") {
        return SupportsCondition::Or(parts.into_iter().map(parse_supports_text).collect());
    }
    if let Some(parts) = split_top_level_word(input, "and") {
        return SupportsCondition::And(parts.into_iter().map(parse_supports_text).collect());
    }
    if let Some((name, value)) = input.split_once(':') {
        let declarations = parse_declaration_list(&format!("{}:{};", name, value));
        if let Some(declaration) = declarations.into_iter().next()
            && !declaration.name.is_empty()
        {
            return SupportsCondition::Declaration {
                name: declaration.name,
                value: declaration.value,
            };
        }
    }
    SupportsCondition::Never
}

fn strip_outer_parens(mut input: &str) -> &str {
    loop {
        let trimmed = input.trim();
        if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
            return trimmed;
        }
        let mut depth = 0usize;
        let mut wraps = true;
        for (index, ch) in trimmed.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 && index + 1 < trimmed.len() {
                        wraps = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !wraps {
            return trimmed;
        }
        input = &trimmed[1..trimmed.len() - 1];
    }
}

fn split_top_level_word<'a>(input: &'a str, word: &str) -> Option<Vec<&'a str>> {
    let lower = input.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut depth = 0usize;
    let mut last = 0usize;
    let mut parts = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0
            && lower[index..].starts_with(word)
            && index > 0
            && bytes[index - 1] == b' '
            && bytes.get(index + word.len()) == Some(&b' ')
        {
            parts.push(input[last..index - 1].trim());
            last = index + word.len() + 1;
            index = last;
            continue;
        }
        index += 1;
    }
    if parts.is_empty() {
        None
    } else {
        parts.push(input[last..].trim());
        Some(parts)
    }
}

fn parse_media_length(input: &str) -> Option<f32> {
    let input = input.trim();
    let (number, unit) = if let Some(stripped) = input.strip_suffix("px") {
        (stripped, "px")
    } else if let Some(stripped) = input.strip_suffix("em") {
        (stripped, "em")
    } else if let Some(stripped) = input.strip_suffix("rem") {
        (stripped, "rem")
    } else {
        (input, "px")
    };
    let value: f32 = number.trim().parse().ok()?;
    Some(match unit {
        "em" | "rem" => value * 16.0,
        _ => value,
    })
}
