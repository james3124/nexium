//! CSS selector parsing and matching for DOM queries and the style engine.
//!
//! Supported: type/universal selectors, `#id`, `.class`, attribute selectors
//! (`[attr]`, `[ns|attr]`, `[*|attr]`, `[|attr]`, `=`, `~=`, `|=`, `^=`, `$=`, `*=`),
//! descendant (` `), child
//! (`>`), adjacent-sibling (`+`) and general-sibling (`~`) combinators,
//! selector lists (`,`), and the pseudo-classes `:first-child`,
//! `:last-child`, `:only-child`, `:root`, `:empty`, `:link`, `:checked`,
//! `:nth-child()`, `:nth-last-child()` and `:not()`.
//!
//! Unsupported/dynamic pseudo-classes (e.g. `:hover`) parse successfully but
//! never match; rules using pseudo-elements match nothing.

use super::{AttributeNamespace, Document, DomError, Namespace, NodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    AdjacentSibling,
    GeneralSibling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeOp {
    Exists,
    Equals,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

/// Attribute namespace prefix in a selector (`[xlink|href]`, `[*|href]`,
/// `[|href]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeNamespaceSelector {
    /// `*|attr` — any namespace, including none.
    Any,
    /// `|attr` — attributes outside any namespace.
    NoNamespace,
    /// `xlink|attr`.
    XLink,
    /// `xml|attr`.
    Xml,
    /// `xmlns|attr`.
    Xmlns,
}

impl AttributeNamespaceSelector {
    fn namespace(self) -> Option<AttributeNamespace> {
        match self {
            AttributeNamespaceSelector::Any | AttributeNamespaceSelector::NoNamespace => None,
            AttributeNamespaceSelector::XLink => Some(AttributeNamespace::XLink),
            AttributeNamespaceSelector::Xml => Some(AttributeNamespace::Xml),
            AttributeNamespaceSelector::Xmlns => Some(AttributeNamespace::Xmlns),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeSelector {
    /// Attribute namespace prefix; `None` means an unprefixed selector, which
    /// keeps matching by qualified name (existing behaviour).
    pub namespace: Option<AttributeNamespaceSelector>,
    pub name: String,
    pub op: AttributeOp,
    pub value: String,
    pub case_insensitive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceSelector {
    Any,
    Html,
    Svg,
    MathMl,
    NoNamespace,
    /// Namespace prefix declared in `@namespace` with a URL that maps to no
    /// element namespace known to the engine; such selectors never match.
    Never,
}

/// Namespace bindings in effect while parsing a stylesheet's selectors.
#[derive(Clone, Copy)]
struct NamespaceContext<'a> {
    /// Prefixes declared with `@namespace prefix url(...)`.
    prefixes: &'a [(String, NamespaceSelector)],
    /// Default namespace declared with `@namespace url(...)`.
    default: Option<NamespaceSelector>,
}

const NO_NAMESPACES: NamespaceContext<'static> = NamespaceContext {
    prefixes: &[],
    default: None,
};

impl NamespaceContext<'_> {
    fn resolve(&self, prefix: &str) -> Option<NamespaceSelector> {
        let lower = prefix.to_ascii_lowercase();
        if let Some((_, selector)) = self.prefixes.iter().find(|(name, _)| *name == lower) {
            return Some(*selector);
        }
        match lower.as_str() {
            "*" => Some(NamespaceSelector::Any),
            "html" => Some(NamespaceSelector::Html),
            "svg" => Some(NamespaceSelector::Svg),
            "math" | "mathml" => Some(NamespaceSelector::MathMl),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nth {
    pub a: i32,
    pub b: i32,
}

impl Nth {
    /// Match a 1-based index against `an + b` for some n >= 0.
    pub fn matches(&self, index: usize) -> bool {
        let index = index as i32;
        if self.a == 0 {
            index == self.b
        } else {
            let diff = index - self.b;
            diff % self.a == 0 && diff / self.a >= 0
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoClass {
    FirstChild,
    LastChild,
    OnlyChild,
    Root,
    Empty,
    Link,
    Checked,
    Hover,
    NthChild(Nth),
    NthLastChild(Nth),
    Not(Vec<ComplexSelector>),
    /// Dynamic or unsupported pseudo-class; parses but never matches.
    Never,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompoundSelector {
    pub namespace: Option<NamespaceSelector>,
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attributes: Vec<AttributeSelector>,
    pub pseudo_classes: Vec<PseudoClass>,
    pub pseudo_element: Option<String>,
}

/// A complex selector: compounds joined right-to-left by combinators.
/// `parts[0].0` is always [`Combinator::Descendant`] and is never consulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComplexSelector {
    pub parts: Vec<(Combinator, CompoundSelector)>,
}

/// Selector specificity `(ids, classes/attributes, types)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity(pub u32, pub u32, pub u32);

impl ComplexSelector {
    pub fn specificity(&self) -> Specificity {
        let mut spec = Specificity::default();
        for (_, compound) in &self.parts {
            if compound.id.is_some() {
                spec.0 += 1;
            }
            spec.1 += compound.classes.len() as u32 + compound.attributes.len() as u32;
            if compound.tag.is_some() {
                spec.2 += 1;
            }
            for pseudo in &compound.pseudo_classes {
                match pseudo {
                    PseudoClass::Not(list) => {
                        if let Some(max) = list.iter().map(ComplexSelector::specificity).max() {
                            spec.0 += max.0;
                            spec.1 += max.1;
                            spec.2 += max.2;
                        }
                    }
                    _ => spec.1 += 1,
                }
            }
            if compound.pseudo_element.is_some() {
                spec.2 += 1;
            }
        }
        spec
    }
}

pub fn parse_selector_list(input: &str) -> Result<Vec<ComplexSelector>, DomError> {
    parse_selector_list_ctx(input, &NO_NAMESPACES)
}

/// Parse a selector list resolving namespace prefixes declared in a stylesheet
/// with `@namespace`. `prefixes` maps lowercased prefixes to namespaces and
/// `default` is the stylesheet's default namespace, applied to unprefixed type
/// selectors.
pub fn parse_selector_list_with_namespaces(
    input: &str,
    prefixes: &[(String, NamespaceSelector)],
    default: Option<NamespaceSelector>,
) -> Result<Vec<ComplexSelector>, DomError> {
    parse_selector_list_ctx(input, &NamespaceContext { prefixes, default })
}

fn parse_selector_list_ctx(
    input: &str,
    ctx: &NamespaceContext,
) -> Result<Vec<ComplexSelector>, DomError> {
    let groups = split_groups(input)?;
    let mut out = Vec::with_capacity(groups.len());
    for group in groups {
        out.push(parse_complex(group.trim(), ctx)?);
    }
    if out.is_empty() {
        return Err(DomError::InvalidSelector(input.to_owned()));
    }
    Ok(out)
}

fn split_groups(input: &str) -> Result<Vec<&str>, DomError> {
    let mut groups = Vec::new();
    let mut start = 0;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut quote: Option<char> = None;

    for (i, ch) in input.char_indices() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '[' => bracket_depth += 1,
                ']' => {
                    if bracket_depth == 0 {
                        return Err(DomError::InvalidSelector(input.to_owned()));
                    }
                    bracket_depth -= 1;
                }
                '(' => paren_depth += 1,
                ')' => {
                    if paren_depth == 0 {
                        return Err(DomError::InvalidSelector(input.to_owned()));
                    }
                    paren_depth -= 1;
                }
                ',' if bracket_depth == 0 && paren_depth == 0 => {
                    groups.push(&input[start..i]);
                    start = i + ch.len_utf8();
                }
                _ => {}
            },
        }
    }
    if bracket_depth != 0 || paren_depth != 0 || quote.is_some() {
        return Err(DomError::InvalidSelector(input.to_owned()));
    }
    groups.push(&input[start..]);
    if groups.iter().any(|g| g.trim().is_empty()) {
        return Err(DomError::InvalidSelector(input.to_owned()));
    }
    Ok(groups)
}

fn parse_complex(input: &str, ctx: &NamespaceContext) -> Result<ComplexSelector, DomError> {
    let invalid = || DomError::InvalidSelector(input.to_owned());
    if input.is_empty() {
        return Err(invalid());
    }

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;
    let mut parts: Vec<(Combinator, CompoundSelector)> = Vec::new();
    let mut pending: Option<Combinator> = None;
    let mut after_compound = false;

    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            if after_compound && pending.is_none() {
                pending = Some(Combinator::Descendant);
            }
            i += 1;
            continue;
        }
        if ch == '>' || ch == '+' || ch == '~' {
            if !after_compound {
                return Err(invalid());
            }
            // An explicit combinator overrides the descendant combinator
            // implied by preceding whitespace.
            pending = Some(match ch {
                '>' => Combinator::Child,
                '+' => Combinator::AdjacentSibling,
                _ => Combinator::GeneralSibling,
            });
            after_compound = false;
            i += 1;
            continue;
        }

        let (compound, next) = parse_compound(&chars, i, input, ctx)?;
        i = next;
        if parts.is_empty() {
            parts.push((Combinator::Descendant, compound));
        } else {
            let comb = pending.take().ok_or_else(invalid)?;
            parts.push((comb, compound));
        }
        after_compound = true;
    }

    if parts.is_empty() || pending.is_some() {
        return Err(invalid());
    }
    Ok(ComplexSelector { parts })
}

fn parse_compound(
    chars: &[char],
    start: usize,
    original: &str,
    ctx: &NamespaceContext,
) -> Result<(CompoundSelector, usize), DomError> {
    let invalid = || DomError::InvalidSelector(original.to_owned());
    let mut i = start;
    let mut compound = CompoundSelector::default();
    let mut consumed = false;

    // Optional namespace prefix: `ns|tag`, `*|tag` or `|tag`.
    if i < chars.len() && chars[i] == '|' {
        compound.namespace = Some(NamespaceSelector::NoNamespace);
        i += 1;
    } else if i < chars.len() && chars[i] == '*' && chars.get(i + 1) == Some(&'|') {
        compound.namespace = Some(NamespaceSelector::Any);
        i += 2;
    } else if i < chars.len() && is_ident_start(chars[i]) {
        let (name, next) = read_ident(chars, i);
        if chars.get(next) == Some(&'|') {
            compound.namespace = Some(ctx.resolve(&name).ok_or_else(invalid)?);
            i = next + 1;
        } else {
            compound.tag = Some(name);
            i = next;
            consumed = true;
        }
    }

    if compound.namespace.is_some() {
        if i < chars.len() && chars[i] == '*' {
            i += 1;
            consumed = true;
        } else if i < chars.len() && is_ident_start(chars[i]) {
            let (name, next) = read_ident(chars, i);
            compound.tag = Some(name);
            i = next;
            consumed = true;
        } else {
            return Err(invalid());
        }
    } else if !consumed {
        if i < chars.len() && chars[i] == '*' {
            i += 1;
            consumed = true;
            // An explicit universal selector is also resolved in the default
            // namespace; an implied universal (`.class`) is not.
            compound.namespace = ctx.default;
        } else if i < chars.len() && is_ident_start(chars[i]) {
            let (name, next) = read_ident(chars, i);
            compound.tag = Some(name);
            i = next;
            consumed = true;
        }
    }

    while i < chars.len() {
        match chars[i] {
            '#' => {
                i += 1;
                let (name, next) = read_ident(chars, i);
                if name.is_empty() {
                    return Err(invalid());
                }
                compound.id = Some(name);
                i = next;
                consumed = true;
            }
            '.' => {
                i += 1;
                let (name, next) = read_ident(chars, i);
                if name.is_empty() {
                    return Err(invalid());
                }
                compound.classes.push(name);
                i = next;
                consumed = true;
            }
            '[' => {
                let (attr, next) = parse_attribute_selector(chars, i + 1, original)?;
                compound.attributes.push(attr);
                i = next;
                consumed = true;
            }
            ':' => {
                let (pseudo, next) = parse_pseudo(chars, i, original, ctx)?;
                match pseudo {
                    ParsedPseudo::Class(class) => compound.pseudo_classes.push(class),
                    ParsedPseudo::Element(name) => compound.pseudo_element = Some(name),
                }
                i = next;
                consumed = true;
            }
            _ => break,
        }
    }

    if !consumed {
        return Err(invalid());
    }
    // An unprefixed type selector is interpreted in the stylesheet's default
    // namespace (`@namespace url(...)`), if one was declared.
    if compound.namespace.is_none() && compound.tag.is_some() {
        compound.namespace = ctx.default;
    }
    Ok((compound, i))
}

enum ParsedPseudo {
    Class(PseudoClass),
    Element(String),
}

fn parse_pseudo(
    chars: &[char],
    start: usize,
    original: &str,
    ctx: &NamespaceContext,
) -> Result<(ParsedPseudo, usize), DomError> {
    let invalid = || DomError::InvalidSelector(original.to_owned());
    let mut i = start + 1; // skip ':'
    let double_colon = i < chars.len() && chars[i] == ':';
    if double_colon {
        i += 1;
    }
    let (name, next) = read_ident(chars, i);
    if name.is_empty() {
        return Err(invalid());
    }
    let name = name.to_ascii_lowercase();
    i = next;

    if double_colon {
        return Ok((ParsedPseudo::Element(name), i));
    }

    if i < chars.len() && chars[i] == '(' {
        let (inner, next) = read_parenthesized(chars, i, original)?;
        i = next;
        let class = match name.as_str() {
            "not" => {
                let list = parse_selector_list_ctx(inner.trim(), ctx)?;
                PseudoClass::Not(list)
            }
            "nth-child" => PseudoClass::NthChild(parse_nth(&inner).ok_or_else(invalid)?),
            "nth-last-child" => PseudoClass::NthLastChild(parse_nth(&inner).ok_or_else(invalid)?),
            _ => PseudoClass::Never,
        };
        return Ok((ParsedPseudo::Class(class), i));
    }

    let class = match name.as_str() {
        "first-child" => PseudoClass::FirstChild,
        "last-child" => PseudoClass::LastChild,
        "only-child" => PseudoClass::OnlyChild,
        "root" => PseudoClass::Root,
        "empty" => PseudoClass::Empty,
        "link" | "any-link" => PseudoClass::Link,
        "checked" => PseudoClass::Checked,
        "hover" => PseudoClass::Hover,
        _ => PseudoClass::Never,
    };
    Ok((ParsedPseudo::Class(class), i))
}

fn read_parenthesized(
    chars: &[char],
    start_open: usize,
    original: &str,
) -> Result<(String, usize), DomError> {
    let mut depth = 0usize;
    let mut i = start_open;
    let mut out = String::new();
    while i < chars.len() {
        let ch = chars[i];
        if ch == '(' {
            depth += 1;
            if depth > 1 {
                out.push(ch);
            }
            i += 1;
            continue;
        }
        if ch == ')' {
            depth -= 1;
            i += 1;
            if depth == 0 {
                return Ok((out, i));
            }
            out.push(ch);
            continue;
        }
        if depth >= 1 {
            out.push(ch);
        }
        i += 1;
    }
    Err(DomError::InvalidSelector(original.to_owned()))
}

fn parse_nth(input: &str) -> Option<Nth> {
    let text = input.to_ascii_lowercase().replace(' ', "");
    if text == "odd" {
        return Some(Nth { a: 2, b: 1 });
    }
    if text == "even" {
        return Some(Nth { a: 2, b: 0 });
    }
    if let Some(index) = text.find('n') {
        let a_text = &text[..index];
        let b_text = &text[index + 1..];
        let a = match a_text {
            "" | "+" => 1,
            "-" => -1,
            other => other.parse().ok()?,
        };
        let b = if b_text.is_empty() {
            0
        } else {
            b_text.parse().ok()?
        };
        Some(Nth { a, b })
    } else {
        Some(Nth {
            a: 0,
            b: text.parse().ok()?,
        })
    }
}

fn parse_attribute_selector(
    chars: &[char],
    start: usize,
    original: &str,
) -> Result<(AttributeSelector, usize), DomError> {
    let invalid = || DomError::InvalidSelector(original.to_owned());
    let mut i = start;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }

    // Optional attribute namespace prefix: `ns|attr`, `*|attr` or `|attr`.
    // A `|` followed by `=` is the dash-match operator, not a namespace.
    let is_namespace_bar =
        |index: usize| chars.get(index) == Some(&'|') && chars.get(index + 1) != Some(&'=');
    let mut namespace = None;
    let mut name = String::new();
    if i < chars.len() && chars[i] == '*' && is_namespace_bar(i + 1) {
        namespace = Some(AttributeNamespaceSelector::Any);
        i += 2;
    } else if i < chars.len() && is_namespace_bar(i) {
        namespace = Some(AttributeNamespaceSelector::NoNamespace);
        i += 1;
    } else {
        let (first, next) = read_ident(chars, i);
        if first.is_empty() {
            return Err(invalid());
        }
        i = next;
        if is_namespace_bar(i) {
            namespace = Some(parse_attribute_namespace_prefix(&first).ok_or_else(invalid)?);
            i += 1;
        } else {
            name = first;
        }
    }

    if namespace.is_some() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let (local, next) = read_ident(chars, i);
        if local.is_empty() {
            return Err(invalid());
        }
        name = local;
        i = next;
    }

    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }

    if i < chars.len() && chars[i] == ']' {
        return Ok((
            AttributeSelector {
                namespace,
                name: name.to_ascii_lowercase(),
                op: AttributeOp::Exists,
                value: String::new(),
                case_insensitive: false,
            },
            i + 1,
        ));
    }

    let op = match chars.get(i).copied() {
        Some('=') => {
            i += 1;
            AttributeOp::Equals
        }
        Some('~') if chars.get(i + 1) == Some(&'=') => {
            i += 2;
            AttributeOp::Includes
        }
        Some('|') if chars.get(i + 1) == Some(&'=') => {
            i += 2;
            AttributeOp::DashMatch
        }
        Some('^') if chars.get(i + 1) == Some(&'=') => {
            i += 2;
            AttributeOp::Prefix
        }
        Some('$') if chars.get(i + 1) == Some(&'=') => {
            i += 2;
            AttributeOp::Suffix
        }
        Some('*') if chars.get(i + 1) == Some(&'=') => {
            i += 2;
            AttributeOp::Substring
        }
        _ => return Err(invalid()),
    };

    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= chars.len() {
        return Err(invalid());
    }

    let value = if chars[i] == '"' || chars[i] == '\'' {
        let quote = chars[i];
        i += 1;
        let mut value = String::new();
        while i < chars.len() && chars[i] != quote {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 1;
            }
            value.push(chars[i]);
            i += 1;
        }
        if i >= chars.len() {
            return Err(invalid());
        }
        i += 1;
        value
    } else {
        let (value, next) = read_ident(chars, i);
        if value.is_empty() {
            return Err(invalid());
        }
        i = next;
        value
    };

    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    let mut case_insensitive = false;
    if i < chars.len() && matches!(chars[i], 'i' | 'I' | 's' | 'S') {
        case_insensitive = chars[i] == 'i' || chars[i] == 'I';
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
    }
    if i >= chars.len() || chars[i] != ']' {
        return Err(invalid());
    }

    Ok((
        AttributeSelector {
            namespace,
            name: name.to_ascii_lowercase(),
            op,
            value,
            case_insensitive,
        },
        i + 1,
    ))
}

fn parse_attribute_namespace_prefix(prefix: &str) -> Option<AttributeNamespaceSelector> {
    match prefix.to_ascii_lowercase().as_str() {
        "xlink" => Some(AttributeNamespaceSelector::XLink),
        "xml" => Some(AttributeNamespaceSelector::Xml),
        "xmlns" => Some(AttributeNamespaceSelector::Xmlns),
        _ => None,
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == '-' || !ch.is_ascii()
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || !ch.is_ascii()
}

fn read_ident(chars: &[char], start: usize) -> (String, usize) {
    let mut end = start;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    (chars[start..end].iter().collect(), end)
}

pub(crate) fn parse_and_query_one(
    doc: &Document,
    root: NodeId,
    selector: &str,
) -> Result<Option<NodeId>, DomError> {
    let list = parse_selector_list(selector)?;
    Ok(doc
        .descendants(root)
        .into_iter()
        .find(|&id| doc.is_element(id) && list.iter().any(|s| matches_complex(doc, id, s))))
}

pub(crate) fn parse_and_query_all(
    doc: &Document,
    root: NodeId,
    selector: &str,
) -> Result<Vec<NodeId>, DomError> {
    let list = parse_selector_list(selector)?;
    Ok(doc
        .descendants(root)
        .into_iter()
        .filter(|&id| doc.is_element(id) && list.iter().any(|s| matches_complex(doc, id, s)))
        .collect())
}

pub(crate) fn parse_and_match(
    doc: &Document,
    id: NodeId,
    selector: &str,
) -> Result<bool, DomError> {
    if !doc.is_element(id) {
        return Ok(false);
    }
    let list = parse_selector_list(selector)?;
    Ok(list.iter().any(|s| matches_complex(doc, id, s)))
}

/// Match an element for a generated-content pseudo-element (`before`/`after`).
/// The selector must target that pseudo-element on its final compound.
pub fn matches_complex_pseudo(
    doc: &Document,
    id: NodeId,
    selector: &ComplexSelector,
    pseudo: &str,
) -> bool {
    let mut selector = selector.clone();
    let Some((_, compound)) = selector.parts.last_mut() else {
        return false;
    };
    let matches_pseudo = compound
        .pseudo_element
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(pseudo));
    if !matches_pseudo {
        return false;
    }
    compound.pseudo_element = None;
    matches_complex(doc, id, &selector)
}

/// Match a single element against an already-parsed complex selector.
pub fn matches_complex(doc: &Document, id: NodeId, selector: &ComplexSelector) -> bool {
    if !doc.is_element(id) {
        return false;
    }
    part_matches(doc, id, selector, selector.parts.len() - 1)
}

fn part_matches(doc: &Document, id: NodeId, selector: &ComplexSelector, index: usize) -> bool {
    if !compound_matches(doc, id, &selector.parts[index].1) {
        return false;
    }
    if index == 0 {
        return true;
    }
    match selector.parts[index].0 {
        Combinator::Child => doc
            .parent(id)
            .map(|p| part_matches(doc, p, selector, index - 1))
            .unwrap_or(false),
        Combinator::Descendant => {
            let mut current = doc.parent(id);
            while let Some(ancestor) = current {
                if part_matches(doc, ancestor, selector, index - 1) {
                    return true;
                }
                current = doc.parent(ancestor);
            }
            false
        }
        Combinator::AdjacentSibling => previous_element_sibling(doc, id)
            .map(|s| part_matches(doc, s, selector, index - 1))
            .unwrap_or(false),
        Combinator::GeneralSibling => {
            let mut current = previous_element_sibling(doc, id);
            while let Some(sibling) = current {
                if part_matches(doc, sibling, selector, index - 1) {
                    return true;
                }
                current = previous_element_sibling(doc, sibling);
            }
            false
        }
    }
}

fn compound_matches(doc: &Document, id: NodeId, compound: &CompoundSelector) -> bool {
    if compound.pseudo_element.is_some() {
        return false;
    }
    let Some(element) = doc.element(id) else {
        return false;
    };
    let namespace_matches = match compound.namespace {
        None | Some(NamespaceSelector::Any) => true,
        Some(NamespaceSelector::Html) => element.namespace == Namespace::Html,
        Some(NamespaceSelector::Svg) => element.namespace == Namespace::Svg,
        Some(NamespaceSelector::MathMl) => element.namespace == Namespace::MathMl,
        Some(NamespaceSelector::NoNamespace) | Some(NamespaceSelector::Never) => false,
    };
    if !namespace_matches {
        return false;
    }
    if let Some(tag) = &compound.tag {
        let matches = if element.namespace == Namespace::Html {
            element.name.eq_ignore_ascii_case(tag)
        } else {
            element.name == *tag
        };
        if !matches {
            return false;
        }
    }
    if let Some(wanted) = &compound.id
        && element.id() != Some(wanted.as_str())
    {
        return false;
    }
    if !compound.classes.is_empty() {
        let mut classes: Vec<&str> = element.classes().collect();
        for wanted in &compound.classes {
            if let Some(pos) = classes.iter().position(|c| c == wanted) {
                classes.remove(pos);
            } else {
                return false;
            }
        }
    }
    for attr in &compound.attributes {
        let value = match attr.namespace {
            None => element.get_attribute(&attr.name),
            Some(AttributeNamespaceSelector::Any) => element
                .attributes
                .iter()
                .find(|candidate| candidate.local_name().eq_ignore_ascii_case(&attr.name))
                .map(|candidate| candidate.value.as_str()),
            Some(AttributeNamespaceSelector::NoNamespace) => element
                .attributes
                .iter()
                .find(|candidate| {
                    candidate.prefix().is_none() && candidate.name.eq_ignore_ascii_case(&attr.name)
                })
                .map(|candidate| candidate.value.as_str()),
            Some(selector) => selector
                .namespace()
                .and_then(|namespace| doc.get_attribute_ns(id, namespace, &attr.name)),
        };
        let Some(value) = value else {
            return false;
        };
        let eq = |left: &str, right: &str| {
            if attr.case_insensitive {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        };
        let matched = match attr.op {
            AttributeOp::Exists => true,
            AttributeOp::Equals => eq(value, &attr.value),
            AttributeOp::Includes => value
                .split_ascii_whitespace()
                .any(|part| eq(part, &attr.value)),
            AttributeOp::DashMatch => {
                eq(value, &attr.value)
                    || (value.len() > attr.value.len()
                        && {
                            let prefix = &value[..attr.value.len()];
                            eq(prefix, &attr.value)
                        }
                        && value.as_bytes().get(attr.value.len()) == Some(&b'-'))
            }
            AttributeOp::Prefix => {
                if attr.case_insensitive {
                    value
                        .get(..attr.value.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&attr.value))
                } else {
                    value.starts_with(&attr.value)
                }
            }
            AttributeOp::Suffix => {
                if attr.case_insensitive {
                    value
                        .get(value.len().saturating_sub(attr.value.len())..)
                        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(&attr.value))
                } else {
                    value.ends_with(&attr.value)
                }
            }
            AttributeOp::Substring => {
                if attr.case_insensitive {
                    value
                        .to_ascii_lowercase()
                        .contains(&attr.value.to_ascii_lowercase())
                } else {
                    value.contains(&attr.value)
                }
            }
        };
        if !matched {
            return false;
        }
    }
    for pseudo in &compound.pseudo_classes {
        if !pseudo_matches(doc, id, pseudo) {
            return false;
        }
    }
    true
}

fn pseudo_matches(doc: &Document, id: NodeId, pseudo: &PseudoClass) -> bool {
    match pseudo {
        PseudoClass::FirstChild => element_index(doc, id) == Some(1),
        PseudoClass::LastChild => element_index_from_end(doc, id) == Some(1),
        PseudoClass::OnlyChild => {
            element_index(doc, id) == Some(1) && element_index_from_end(doc, id) == Some(1)
        }
        PseudoClass::Root => doc.parent(id) == Some(doc.document_node()),
        PseudoClass::Empty => doc
            .children(id)
            .iter()
            .all(|&child| match &doc.node(child).data {
                super::NodeData::Element(_) => false,
                super::NodeData::Text(text) => text.is_empty(),
                _ => true,
            }),
        PseudoClass::Link => {
            let tag = doc.tag_name(id);
            matches!(tag, Some("a" | "area" | "link")) && doc.get_attribute(id, "href").is_some()
        }
        PseudoClass::Checked => doc
            .element(id)
            .is_some_and(|el| el.has_attribute("checked") || el.has_attribute("selected")),
        PseudoClass::Hover => doc.hover_target() == Some(id),
        PseudoClass::NthChild(nth) => element_index(doc, id).is_some_and(|i| nth.matches(i)),
        PseudoClass::NthLastChild(nth) => {
            element_index_from_end(doc, id).is_some_and(|i| nth.matches(i))
        }
        PseudoClass::Not(list) => !list.iter().any(|s| matches_complex(doc, id, s)),
        PseudoClass::Never => false,
    }
}

fn element_index(doc: &Document, id: NodeId) -> Option<usize> {
    let parent = doc.parent(id)?;
    let mut index = 0;
    for &sibling in doc.children(parent) {
        if doc.is_element(sibling) {
            index += 1;
            if sibling == id {
                return Some(index);
            }
        }
    }
    None
}

fn element_index_from_end(doc: &Document, id: NodeId) -> Option<usize> {
    let parent = doc.parent(id)?;
    let mut index = 0;
    for &sibling in doc.children(parent).iter().rev() {
        if doc.is_element(sibling) {
            index += 1;
            if sibling == id {
                return Some(index);
            }
        }
    }
    None
}

fn previous_element_sibling(doc: &Document, id: NodeId) -> Option<NodeId> {
    let parent = doc.parent(id)?;
    let siblings = doc.children(parent);
    let pos = siblings.iter().position(|&s| s == id)?;
    siblings[..pos]
        .iter()
        .rev()
        .copied()
        .find(|&s| doc.is_element(s))
}

#[cfg(test)]
mod tests {
    use crate::engine::html::parse_html;

    #[test]
    fn hover_pseudo_matches_only_the_hover_target() {
        let mut doc = parse_html(r#"<div id="a"><p id="b">x</p></div>"#);
        let a = doc.select("#a").unwrap().unwrap();
        let b = doc.select("#b").unwrap().unwrap();

        // Nothing is hovered initially.
        assert!(!doc.matches(b, "p:hover").unwrap());
        assert!(!doc.matches(a, "div:hover").unwrap());

        doc.set_hover_target(Some(b));
        assert!(doc.matches(b, "p:hover").unwrap());
        assert!(doc.matches(b, "#a p:hover").unwrap());
        assert!(!doc.matches(a, "div:hover").unwrap());
        assert!(doc.matches(a, "div:not(:hover)").unwrap());

        doc.set_hover_target(None);
        assert!(!doc.matches(b, "p:hover").unwrap());
    }
}
