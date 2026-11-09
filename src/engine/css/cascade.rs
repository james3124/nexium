//! Cascade: rule matching, specificity, importance, inheritance, variables and
//! computed-style construction.

use std::collections::HashMap;

use crate::engine::dom::{
    ComplexSelector, Document, NodeId, Specificity, matches_complex, matches_complex_pseudo,
};

use super::parser::{ComponentValue, Declaration, Rule, Stylesheet, SupportsCondition};
use super::style::{
    ComputedStyle, ContentPart, ContentValue, PropertyId, PropertyValue, expand_declaration,
};
use super::values::Viewport;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Priority {
    important: u8,
    origin: u8,
    specificity: Specificity,
    order: u64,
}

struct FlatRule<'a> {
    selectors: &'a [ComplexSelector],
    declarations: &'a [Declaration],
    origin: u8,
    order: u64,
}

pub struct StyleResolver<'a> {
    doc: &'a Document,
    rules: Vec<FlatRule<'a>>,
    viewport: Viewport,
}

impl<'a> StyleResolver<'a> {
    pub fn new(
        doc: &'a Document,
        user_agent: &'a Stylesheet,
        author: &'a [Stylesheet],
        viewport: Viewport,
    ) -> Self {
        let mut rules = Vec::new();
        let mut order = 0u64;
        flatten_sheet(&user_agent.rules, 0, viewport, &mut rules, &mut order);
        for sheet in author {
            flatten_sheet(&sheet.rules, 1, viewport, &mut rules, &mut order);
        }
        Self {
            doc,
            rules,
            viewport,
        }
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn resolve(
        &self,
        node: NodeId,
        parent: &ComputedStyle,
        inline: &[Declaration],
        root_font_size: f32,
    ) -> ComputedStyle {
        let mut custom_winners: HashMap<String, (&[ComponentValue], Priority)> = HashMap::new();
        let mut candidates: Vec<(&str, &[ComponentValue], Priority)> = Vec::new();

        for rule in &self.rules {
            let mut best: Option<Specificity> = None;
            for selector in rule.selectors {
                if matches_complex(self.doc, node, selector) {
                    let specificity = selector.specificity();
                    if best.is_none_or(|current| specificity > current) {
                        best = Some(specificity);
                    }
                }
            }
            let Some(specificity) = best else { continue };

            for (index, declaration) in rule.declarations.iter().enumerate() {
                let priority = Priority {
                    important: u8::from(declaration.important),
                    origin: rule.origin,
                    specificity,
                    order: rule.order + index as u64,
                };
                collect_declaration(declaration, priority, &mut custom_winners, &mut candidates);
            }
        }

        for (index, declaration) in inline.iter().enumerate() {
            let priority = Priority {
                important: u8::from(declaration.important),
                origin: 2,
                specificity: Specificity(0, 0, 0),
                order: u64::MAX / 2 + index as u64,
            };
            collect_declaration(declaration, priority, &mut custom_winners, &mut candidates);
        }

        // Build inherited custom properties, then resolve them.
        let mut raw_custom: HashMap<String, Vec<ComponentValue>> = parent.custom_properties.clone();
        for (name, (values, _)) in &custom_winners {
            raw_custom.insert(name.clone(), values.to_vec());
        }
        let mut resolved_custom: HashMap<String, Vec<ComponentValue>> = HashMap::new();
        for name in raw_custom.keys() {
            let mut visiting = vec![name.clone()];
            let resolved = substitute(&raw_custom[name], &raw_custom, &mut visiting);
            resolved_custom.insert(name.clone(), resolved);
        }

        let mut winners: HashMap<PropertyId, (PropertyValue, Priority)> = HashMap::new();
        for (name, values, priority) in candidates {
            let resolved = substitute(values, &resolved_custom, &mut Vec::new());
            for (id, value) in expand_declaration(name, &resolved) {
                match winners.get_mut(&id) {
                    Some(existing) => {
                        if priority >= existing.1 {
                            *existing = (value, priority);
                        }
                    }
                    None => {
                        winners.insert(id, (value, priority));
                    }
                }
            }
        }

        let mut style = ComputedStyle::inherited_from(parent);
        style.custom_properties = resolved_custom;
        resolve_content(&mut style, self.doc, node);

        // Color first so `currentColor` resolves correctly.
        if let Some((value, _)) = winners.get(&PropertyId::Color) {
            style.apply(
                PropertyId::Color,
                value,
                parent,
                root_font_size,
                self.viewport,
            );
        }
        if let Some((value, _)) = winners.get(&PropertyId::FontSize) {
            style.apply(
                PropertyId::FontSize,
                value,
                parent,
                root_font_size,
                self.viewport,
            );
        }
        if let Some((value, _)) = winners.get(&PropertyId::LineHeight) {
            style.apply(
                PropertyId::LineHeight,
                value,
                parent,
                root_font_size,
                self.viewport,
            );
        }

        for (id, (value, _)) in &winners {
            if matches!(
                id,
                PropertyId::Color | PropertyId::FontSize | PropertyId::LineHeight
            ) {
                continue;
            }
            style.apply(*id, value, parent, root_font_size, self.viewport);
        }

        style
    }
    /// Compute the style of a `::before`/`::after` pseudo-element.
    pub fn resolve_pseudo(
        &self,
        node: NodeId,
        element_style: &ComputedStyle,
        pseudo: &str,
        root_font_size: f32,
    ) -> ComputedStyle {
        let mut custom_winners: HashMap<String, (&[ComponentValue], Priority)> = HashMap::new();
        let mut candidates: Vec<(&str, &[ComponentValue], Priority)> = Vec::new();

        for rule in &self.rules {
            let mut best: Option<Specificity> = None;
            for selector in rule.selectors {
                if matches_complex_pseudo(self.doc, node, selector, pseudo) {
                    let specificity = selector.specificity();
                    if best.is_none_or(|current| specificity > current) {
                        best = Some(specificity);
                    }
                }
            }
            let Some(specificity) = best else { continue };
            for (index, declaration) in rule.declarations.iter().enumerate() {
                let priority = Priority {
                    important: u8::from(declaration.important),
                    origin: rule.origin,
                    specificity,
                    order: rule.order + index as u64,
                };
                collect_declaration(declaration, priority, &mut custom_winners, &mut candidates);
            }
        }

        let mut raw_custom: HashMap<String, Vec<ComponentValue>> =
            element_style.custom_properties.clone();
        for (name, (values, _)) in &custom_winners {
            raw_custom.insert(name.clone(), values.to_vec());
        }
        let mut resolved_custom: HashMap<String, Vec<ComponentValue>> = HashMap::new();
        for name in raw_custom.keys() {
            let mut visiting = vec![name.clone()];
            let resolved = substitute(&raw_custom[name], &raw_custom, &mut visiting);
            resolved_custom.insert(name.clone(), resolved);
        }

        let mut winners: HashMap<PropertyId, (PropertyValue, Priority)> = HashMap::new();
        for (name, values, priority) in candidates {
            let resolved = substitute(values, &resolved_custom, &mut Vec::new());
            for (id, value) in expand_declaration(name, &resolved) {
                match winners.get_mut(&id) {
                    Some(existing) => {
                        if priority >= existing.1 {
                            *existing = (value, priority);
                        }
                    }
                    None => {
                        winners.insert(id, (value, priority));
                    }
                }
            }
        }

        let mut style = ComputedStyle::inherited_from(element_style);
        style.display = crate::engine::css::Display::Inline;
        style.content = None;
        style.content_parts = None;
        style.custom_properties = resolved_custom;

        if let Some((value, _)) = winners.get(&PropertyId::Color) {
            style.apply(
                PropertyId::Color,
                value,
                element_style,
                root_font_size,
                self.viewport,
            );
        }
        if let Some((value, _)) = winners.get(&PropertyId::FontSize) {
            style.apply(
                PropertyId::FontSize,
                value,
                element_style,
                root_font_size,
                self.viewport,
            );
        }
        if let Some((value, _)) = winners.get(&PropertyId::LineHeight) {
            style.apply(
                PropertyId::LineHeight,
                value,
                element_style,
                root_font_size,
                self.viewport,
            );
        }
        for (id, (value, _)) in &winners {
            if matches!(
                id,
                PropertyId::Color | PropertyId::FontSize | PropertyId::LineHeight
            ) {
                continue;
            }
            style.apply(*id, value, element_style, root_font_size, self.viewport);
        }

        resolve_content(&mut style, self.doc, node);
        style
    }
}

fn resolve_content(style: &mut ComputedStyle, doc: &Document, node: NodeId) {
    style.content = match &style.content_parts {
        Some(ContentValue::Parts(parts)) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    ContentPart::Text(value) => text.push_str(value),
                    ContentPart::Attr(name) => {
                        if let Some(value) = doc.get_attribute(node, name) {
                            text.push_str(value);
                        }
                    }
                }
            }
            Some(text)
        }
        _ => None,
    };
}

fn collect_declaration<'a>(
    declaration: &'a Declaration,
    priority: Priority,
    custom_winners: &mut HashMap<String, (&'a [ComponentValue], Priority)>,
    candidates: &mut Vec<(&'a str, &'a [ComponentValue], Priority)>,
) {
    if declaration.name.starts_with("--") {
        match custom_winners.get_mut(&declaration.name) {
            Some(existing) => {
                if priority >= existing.1 {
                    *existing = (&declaration.value, priority);
                }
            }
            None => {
                custom_winners.insert(declaration.name.clone(), (&declaration.value, priority));
            }
        }
    } else {
        candidates.push((&declaration.name, &declaration.value, priority));
    }
}

fn flatten_sheet<'a>(
    rules: &'a [Rule],
    origin: u8,
    viewport: Viewport,
    out: &mut Vec<FlatRule<'a>>,
    order: &mut u64,
) {
    for rule in rules {
        match rule {
            Rule::Style(style_rule) => {
                out.push(FlatRule {
                    selectors: &style_rule.selectors,
                    declarations: &style_rule.declarations,
                    origin,
                    order: *order,
                });
                *order += style_rule.declarations.len().max(1) as u64;
            }
            Rule::Media(media) => {
                if media.matches(viewport.width, viewport.height) {
                    flatten_sheet(&media.rules, origin, viewport, out, order);
                }
            }
            Rule::Supports(supports) => {
                if supports_condition(&supports.condition) {
                    flatten_sheet(&supports.rules, origin, viewport, out, order);
                }
            }
            Rule::Import(_) => {}
        }
    }
}

/// Substitute `var()` references using custom property values.
fn substitute(
    values: &[ComponentValue],
    lookup: &HashMap<String, Vec<ComponentValue>>,
    visiting: &mut Vec<String>,
) -> Vec<ComponentValue> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        match value {
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("var") => {
                let (property, fallback) = split_var_args(args);
                if let Some(property) = property
                    && !visiting.contains(&property)
                    && let Some(replacement) = lookup.get(&property)
                {
                    visiting.push(property);
                    out.extend(substitute(replacement, lookup, visiting));
                    visiting.pop();
                    continue;
                }
                if let Some(fallback) = fallback {
                    out.extend(substitute(fallback, lookup, visiting));
                }
            }
            ComponentValue::Function { name, args } => {
                out.push(ComponentValue::Function {
                    name: name.clone(),
                    args: substitute(args, lookup, visiting),
                });
            }
            other => out.push(other.clone()),
        }
    }
    out
}

fn split_var_args(args: &[ComponentValue]) -> (Option<String>, Option<&[ComponentValue]>) {
    let mut comma = None;
    for (index, value) in args.iter().enumerate() {
        if matches!(value, ComponentValue::Comma) {
            comma = Some(index);
            break;
        }
    }
    let (head, tail) = match comma {
        Some(index) => (&args[..index], Some(&args[index + 1..])),
        None => (args, None),
    };
    let property = head.iter().find_map(|value| match value {
        ComponentValue::Ident(name) => Some(name.clone()),
        _ => None,
    });
    (property, tail)
}

fn supports_condition(condition: &SupportsCondition) -> bool {
    match condition {
        SupportsCondition::Declaration { name, value } => {
            if name == "display"
                && value.iter().any(|component| {
                    matches!(component, ComponentValue::Ident(ident)
                    if matches!(
                        ident.to_ascii_lowercase().as_str(),
                        "grid" | "inline-grid" | "contents"
                    ))
                })
            {
                return false;
            }
            !expand_declaration(name, value).is_empty()
        }
        SupportsCondition::And(parts) => parts.iter().all(supports_condition),
        SupportsCondition::Or(parts) => parts.iter().any(supports_condition),
        SupportsCondition::Not(inner) => !supports_condition(inner),
        SupportsCondition::Never => false,
    }
}
