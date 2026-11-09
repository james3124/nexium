//! HTML tree construction.
//!
//! A pragmatic subset of the WHATWG tree construction algorithm: implied
//! `html`/`head`/`body`, void elements, RCDATA/RAWTEXT handling, common
//! auto-closing rules (`p`, list items, table cells, options, headings) and
//! forgiving handling of mismatched end tags.

use std::borrow::Cow;

use crate::engine::dom::{
    Attribute, Document, Namespace, NodeData, NodeId, QuirksMode, is_void_element,
};

use super::tokenizer::{Token, Tokenizer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    AfterBody,
}

pub fn parse_html(input: &str) -> Document {
    let normalized: Cow<'_, str> = if input.contains('\r') {
        Cow::Owned(input.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(input)
    };
    Parser::new(normalized.as_ref()).run()
}

struct Parser<'a> {
    tokenizer: Tokenizer<'a>,
    doc: Document,
    stack: Vec<NodeId>,
    mode: Mode,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
    doctype_seen: bool,
    active_formatting: Vec<ActiveFormatting>,
}

#[derive(Clone, Debug)]
enum ActiveFormatting {
    Marker,
    Element {
        node: NodeId,
        name: String,
        attributes: Vec<Attribute>,
    },
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            tokenizer: Tokenizer::new(input),
            doc: Document::new(),
            stack: Vec::new(),
            mode: Mode::Initial,
            html: None,
            head: None,
            body: None,
            doctype_seen: false,
            active_formatting: Vec::new(),
        }
    }

    fn run(mut self) -> Document {
        while let Some(token) = self.tokenizer.next_token() {
            self.process(token);
        }
        if !self.doctype_seen {
            self.doc.set_quirks_mode(QuirksMode::Quirks);
        }
        self.doc
    }

    fn current(&self) -> NodeId {
        *self.stack.last().expect("parser stack must not be empty")
    }

    fn process(&mut self, token: Token) {
        if let Token::Doctype {
            name,
            public_id,
            system_id,
            ..
        } = &token
        {
            if self.mode == Mode::Initial && !self.doctype_seen {
                self.doctype_seen = true;
                let mode = classify_quirks(name, public_id, system_id);
                self.doc.set_quirks_mode(mode);
                let id = self.doc.create_doctype(name, public_id, system_id);
                let root = self.doc.document_node();
                self.doc
                    .append_child(root, id)
                    .expect("append doctype to document");
                self.mode = Mode::BeforeHtml;
            }
            return;
        }

        match self.mode {
            Mode::Initial => self.in_initial(token),
            Mode::BeforeHtml => self.in_before_html(token),
            Mode::BeforeHead => self.in_before_head(token),
            Mode::InHead => self.in_head(token),
            Mode::AfterHead => self.in_after_head(token),
            Mode::InBody => self.in_body(token),
            Mode::AfterBody => self.in_after_body(token),
        }
    }

    fn in_initial(&mut self, token: Token) {
        match token {
            Token::Comment(text) => {
                let root = self.doc.document_node();
                self.append_comment(root, &text);
            }
            Token::Characters(text) => {
                if !text.chars().all(is_space) {
                    self.mode = Mode::BeforeHtml;
                    self.process(Token::Characters(text));
                }
            }
            Token::StartTag { .. } | Token::EndTag { .. } => {
                self.mode = Mode::BeforeHtml;
                self.process(token);
            }
            Token::Doctype { .. } => {}
        }
    }

    fn in_before_html(&mut self, token: Token) {
        if let Token::Comment(text) = &token {
            let root = self.doc.document_node();
            self.append_comment(root, text);
            return;
        }
        if let Token::Characters(text) = &token
            && text.chars().all(is_space)
        {
            return;
        }
        if let Token::StartTag { name, .. } = &token
            && name == "html"
        {
            self.ensure_html();
            self.merge_attributes_to_html(&token);
            self.mode = Mode::BeforeHead;
            return;
        }
        self.ensure_html();
        self.mode = Mode::BeforeHead;
        self.process(token);
    }

    fn in_before_head(&mut self, token: Token) {
        if let Token::Comment(text) = &token {
            let html = self.ensure_html();
            self.append_comment(html, text);
            return;
        }
        if let Token::Characters(text) = &token
            && text.chars().all(is_space)
        {
            return;
        }
        if let Token::StartTag { name, .. } = &token {
            if name == "html" {
                self.ensure_html();
                self.merge_attributes_to_html(&token);
                return;
            }
            if name == "head" {
                self.ensure_head();
                return;
            }
        }
        self.ensure_head();
        self.mode = Mode::InHead;
        self.process(token);
    }

    fn in_head(&mut self, token: Token) {
        if let Token::Comment(text) = &token {
            let parent = self.current();
            self.append_comment(parent, text);
            return;
        }
        if let Token::Characters(text) = &token {
            let inside_head_element = self.head.is_some_and(|head| self.current() != head);
            if inside_head_element || text.chars().all(is_space) {
                self.insert_text(text);
            } else {
                let text = text.clone();
                self.leave_head();
                self.process(Token::Characters(text));
            }
            return;
        }
        if let Token::StartTag { name, .. } = &token {
            if name == "html" {
                self.ensure_html();
                self.merge_attributes_to_html(&token);
                return;
            }
            if name == "head" {
                return;
            }
            if self.head.is_some_and(|head| self.current() != head) {
                if is_void_element(name) {
                    self.insert_element(&token);
                } else {
                    self.insert_element_and_push(&token);
                }
                return;
            }
            if is_head_void(name) {
                self.insert_element(&token);
                return;
            }
            if is_head_rawtext(name) || name == "template" || name == "noscript" {
                self.insert_element_and_push(&token);
                return;
            }
            self.leave_head();
            self.process(token);
            return;
        }
        if let Token::EndTag { name } = &token {
            match name.as_str() {
                "head" => self.leave_head(),
                "body" | "html" | "br" => {
                    self.leave_head();
                    self.process(token);
                }
                "template" => self.pop_through("template"),
                _ => self.pop_through(name),
            }
        }
    }

    fn in_after_head(&mut self, token: Token) {
        if let Token::Comment(text) = &token {
            let html = self.ensure_html();
            self.append_comment(html, text);
            return;
        }
        if let Token::Characters(text) = &token {
            let inside_head_element = self.head.is_some_and(|head| self.current() != head);
            if inside_head_element {
                self.insert_text(text);
            } else if text.chars().all(is_space) {
                self.ensure_body();
                self.insert_text(text);
            } else {
                let text = text.clone();
                self.ensure_body();
                self.process(Token::Characters(text));
            }
            return;
        }
        if let Token::StartTag { name, .. } = &token {
            if name == "html" {
                self.ensure_html();
                self.merge_attributes_to_html(&token);
                return;
            }
            if name == "body" {
                self.ensure_body();
                self.merge_attributes_to_body(&token);
                return;
            }
            if name == "head" {
                return;
            }
            if is_after_head_tag(name) {
                if let Some(head) = self.head {
                    self.stack.push(head);
                    if is_head_void(name) {
                        self.insert_element(&token);
                        self.stack.pop();
                    } else if matches!(name.as_str(), "script" | "style") {
                        // Keep the element (and head) open so the tokenizer's
                        // raw-text content is inserted into it.
                        self.insert_element_and_push(&token);
                    } else {
                        self.insert_element_and_push(&token);
                        self.stack.pop();
                        self.stack.pop();
                    }
                }
                return;
            }
            self.ensure_body();
            self.process(token);
            return;
        }
        if let Token::EndTag { name } = &token {
            let inside_head_element = self.head.is_some_and(|head| self.current() != head);
            if inside_head_element {
                self.pop_through(name);
                if let Some(head) = self.head
                    && self.stack.last() == Some(&head)
                {
                    self.stack.pop();
                }
                return;
            }
            if matches!(name.as_str(), "body" | "html" | "br") {
                self.ensure_body();
                self.process(token);
            }
        }
    }

    fn in_body(&mut self, token: Token) {
        match token {
            Token::Comment(text) => {
                let parent = self.current();
                self.append_comment(parent, &text);
            }
            Token::Characters(text) => self.insert_text(&text),
            Token::StartTag { .. } => self.start_tag_in_body(&token),
            Token::EndTag { name } => self.end_tag_in_body(&name),
            Token::Doctype { .. } => {}
        }
    }

    fn in_after_body(&mut self, token: Token) {
        if let Token::Comment(text) = &token {
            let parent = self.html.unwrap_or_else(|| self.doc.document_node());
            self.append_comment(parent, text);
            return;
        }
        if let Token::Characters(text) = &token
            && text.chars().all(is_space)
        {
            self.insert_text(text);
            return;
        }
        if let Token::EndTag { name } = &token
            && name == "html"
        {
            return;
        }
        self.mode = Mode::InBody;
        self.process(token);
    }

    fn ensure_html(&mut self) -> NodeId {
        if let Some(html) = self.html {
            return html;
        }
        let root = self.doc.document_node();
        let html = self.doc.create_element("html");
        self.doc
            .append_child(root, html)
            .expect("append html to document");
        self.html = Some(html);
        self.stack.push(html);
        html
    }

    fn ensure_head(&mut self) -> NodeId {
        if let Some(head) = self.head {
            return head;
        }
        let html = self.ensure_html();
        let head = self.doc.create_element("head");
        self.doc.append_child(html, head).expect("append head");
        self.head = Some(head);
        self.stack.push(head);
        self.mode = Mode::InHead;
        head
    }

    fn ensure_body(&mut self) {
        self.ensure_html();
        if let Some(body) = self.body {
            if !self.stack.contains(&body) {
                self.stack.push(body);
            }
            self.mode = Mode::InBody;
            return;
        }
        if let Some(head) = self.head
            && self.stack.last() == Some(&head)
        {
            self.stack.pop();
        }
        let html = self.html.expect("html exists");
        let body = self.doc.create_element("body");
        self.doc.append_child(html, body).expect("append body");
        self.body = Some(body);
        self.stack.push(body);
        self.mode = Mode::InBody;
    }

    fn leave_head(&mut self) {
        if let Some(head) = self.head
            && self.stack.last() == Some(&head)
        {
            self.stack.pop();
        }
        self.mode = Mode::AfterHead;
    }

    fn append_comment(&mut self, parent: NodeId, text: &str) {
        let id = self.doc.create_comment(text);
        self.doc
            .append_child(parent, id)
            .expect("append comment node");
    }

    fn current_foreign_namespace(&self) -> Option<Namespace> {
        let element = self.doc.element(self.current())?;
        match element.namespace {
            Namespace::Html => None,
            Namespace::Svg => {
                if matches!(element.name.as_str(), "foreignObject" | "desc" | "title") {
                    None
                } else {
                    Some(Namespace::Svg)
                }
            }
            Namespace::MathMl => {
                let html_encoding = element.get_attribute("encoding").is_some_and(|value| {
                    value.eq_ignore_ascii_case("text/html")
                        || value.eq_ignore_ascii_case("application/xhtml+xml")
                });
                if element.name == "annotation-xml" && html_encoding {
                    None
                } else {
                    Some(Namespace::MathMl)
                }
            }
        }
    }

    fn exit_foreign_content(&mut self) {
        while let Some(&id) = self.stack.last() {
            let Some(element) = self.doc.element(id) else {
                break;
            };
            if element.namespace == Namespace::Html {
                break;
            }
            self.stack.pop();
        }
    }

    fn insert_foreign_element(&mut self, token: &Token, namespace: Namespace) {
        let Token::StartTag {
            name,
            attrs,
            self_closing,
            ..
        } = token
        else {
            return;
        };
        let name = adjust_foreign_tag_name(namespace, name);
        let attributes: Vec<Attribute> = attrs
            .iter()
            .map(|attribute| Attribute {
                name: adjust_foreign_attribute_name(namespace, &attribute.name),
                value: attribute.value.clone(),
            })
            .collect();
        let parent = self.insertion_parent();
        let id = self.doc.create_element_ns(&name, namespace, attributes);
        let _ = self.doc.append_child(parent, id);
        if !self_closing {
            self.stack.push(id);
        }
    }

    fn insertion_parent(&self) -> NodeId {
        let current = self.current();
        self.doc
            .element(current)
            .and_then(|element| element.template_contents)
            .unwrap_or(current)
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.current_foreign_namespace().is_none() {
            self.reconstruct_formatting();
        }
        let parent = self.insertion_parent();
        let strip_newline = {
            let is_pre = self
                .doc
                .element(parent)
                .is_some_and(|el| matches!(el.name.as_str(), "pre" | "textarea" | "listing"));
            is_pre && self.doc.children(parent).is_empty() && text.starts_with('\n')
        };
        let text = if strip_newline { &text[1..] } else { text };
        if text.is_empty() {
            return;
        }
        if let Some(&last) = self.doc.children(parent).last()
            && let NodeData::Text(existing) = &mut self.doc.node_mut(last).data
        {
            existing.push_str(text);
            return;
        }
        let id = self.doc.create_text(text);
        self.doc.append_child(parent, id).expect("append text node");
    }

    fn insert_element(&mut self, token: &Token) -> NodeId {
        let Token::StartTag { name, attrs, .. } = token else {
            unreachable!("insert_element requires a start tag");
        };
        let parent = self.insertion_parent();
        let id = self.doc.create_element_with_attributes(name, attrs.clone());
        if name == "template" {
            let fragment = self.doc.create_document_fragment();
            if let Some(element) = self.doc.element_mut(id) {
                element.template_contents = Some(fragment);
            }
        }
        self.doc.append_child(parent, id).expect("append element");
        id
    }

    fn insert_element_and_push(&mut self, token: &Token) -> NodeId {
        let id = self.insert_element(token);
        if let Token::StartTag { name, .. } = token
            && !is_void_element(name)
        {
            self.stack.push(id);
        }
        id
    }

    fn merge_attributes_to_html(&mut self, token: &Token) {
        if let Some(html) = self.html {
            self.merge_attributes(html, token);
        }
    }

    fn merge_attributes_to_body(&mut self, token: &Token) {
        if let Some(body) = self.body {
            self.merge_attributes(body, token);
        }
    }

    fn merge_attributes(&mut self, target: NodeId, token: &Token) {
        let Token::StartTag { attrs, .. } = token else {
            return;
        };
        for attr in attrs {
            let exists = self
                .doc
                .element(target)
                .is_some_and(|el| el.has_attribute(&attr.name));
            if !exists {
                let _ = self.doc.set_attribute(target, &attr.name, &attr.value);
            }
        }
    }

    fn start_tag_in_body(&mut self, token: &Token) {
        let Token::StartTag {
            name,
            attrs,
            self_closing,
            ..
        } = token
        else {
            return;
        };
        let name = name.as_str();
        if let Some(namespace) = self.current_foreign_namespace() {
            if is_foreign_breakout(name) {
                self.exit_foreign_content();
            } else {
                self.insert_foreign_element(token, namespace);
                return;
            }
        }
        if name == "svg" || name == "math" {
            let namespace = if name == "svg" {
                Namespace::Svg
            } else {
                Namespace::MathMl
            };
            self.insert_foreign_element(token, namespace);
            return;
        }
        let _ = self_closing;
        if should_reconstruct_before(name) {
            self.reconstruct_formatting();
        }
        if matches!(
            name,
            "td" | "th" | "caption" | "template" | "applet" | "object" | "marquee"
        ) {
            self.active_formatting.push(ActiveFormatting::Marker);
        }
        match name {
            "html" => {
                self.ensure_html();
                self.merge_attributes_to_html(token);
            }
            "body" => {
                if let Some(body) = self.body {
                    self.merge_attributes(body, token);
                }
            }
            "head" => {}
            "image" => {
                let remapped = remap_start_tag(token, "img");
                self.insert_element(&remapped);
            }
            "p" => {
                self.close_p();
                self.insert_element_and_push(token);
            }
            "li" => {
                self.close_list_item("li", &["ul", "ol"]);
                self.close_p();
                self.insert_element_and_push(token);
            }
            "dd" | "dt" => {
                self.close_list_item("dd", &["dl"]);
                self.close_list_item("dt", &["dl"]);
                self.close_p();
                self.insert_element_and_push(token);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p();
                self.close_heading();
                self.insert_element_and_push(token);
            }
            "option" => {
                if self.doc.tag_name(self.current()) == Some("option") {
                    self.stack.pop();
                }
                self.insert_element_and_push(token);
            }
            "optgroup" => {
                if self.doc.tag_name(self.current()) == Some("option") {
                    self.stack.pop();
                }
                if self.doc.tag_name(self.current()) == Some("optgroup") {
                    self.stack.pop();
                }
                self.insert_element_and_push(token);
            }
            "tr" => {
                self.pop_through_any_bounded(&["td", "th"], &["table"]);
                self.pop_through_any_bounded(&["tr"], &["table"]);
                self.insert_element_and_push(token);
            }
            "td" | "th" => {
                self.pop_through_any_bounded(&["td", "th"], &["tr", "table"]);
                self.insert_element_and_push(token);
            }
            "tbody" | "thead" | "tfoot" => {
                self.pop_through_any_bounded(&["td", "th"], &["table"]);
                self.pop_through_any_bounded(&["tr"], &["table"]);
                self.pop_through_any_bounded(&["tbody", "thead", "tfoot"], &["table"]);
                self.insert_element_and_push(token);
            }
            "caption" | "colgroup" => {
                self.pop_through_any_bounded(&["td", "th"], &["table"]);
                self.pop_through_any_bounded(&["tr"], &["table"]);
                self.pop_through_any_bounded(&["tbody", "thead", "tfoot"], &["table"]);
                self.insert_element_and_push(token);
            }
            "form" => {
                let form_open = self
                    .stack
                    .iter()
                    .any(|&id| self.doc.tag_name(id) == Some("form"));
                if !form_open {
                    self.close_p();
                    self.insert_element_and_push(token);
                }
            }
            "button" => {
                self.pop_through_any(&["button"]);
                self.close_p();
                self.insert_element_and_push(token);
            }
            "a" => {
                if self
                    .stack
                    .iter()
                    .any(|&id| self.doc.tag_name(id) == Some("a"))
                {
                    self.adoption_agency_end("a");
                }
                self.insert_element_and_push(token);
                let node = self.current();
                self.add_formatting(node, "a", attrs.clone());
            }
            "nobr" => {
                if self
                    .stack
                    .iter()
                    .any(|&id| self.doc.tag_name(id) == Some("nobr"))
                {
                    self.adoption_agency_end("nobr");
                }
                self.insert_element_and_push(token);
                let node = self.current();
                self.add_formatting(node, "nobr", attrs.clone());
            }
            "table" => {
                self.close_p();
                self.insert_element_and_push(token);
            }
            "br" | "img" | "input" | "hr" | "meta" | "link" | "base" | "basefont" | "bgsound"
            | "col" | "embed" | "param" | "source" | "track" | "wbr" | "area" | "frame"
            | "keygen" => {
                if name == "hr" {
                    self.close_p();
                }
                self.insert_element(token);
            }
            _ => {
                if closes_p(name) {
                    self.close_p();
                }
                self.insert_element_and_push(token);
                if is_formatting_tag(name) {
                    let node = self.current();
                    self.add_formatting(node, name, attrs.clone());
                }
            }
        }
    }

    fn end_tag_in_body(&mut self, name: &str) {
        if self.stack.iter().any(|&id| {
            self.doc
                .element(id)
                .is_some_and(|element| element.namespace != Namespace::Html)
        }) && let Some(index) = self.stack.iter().rposition(|&id| {
            self.doc.element(id).is_some_and(|element| {
                element.namespace != Namespace::Html && element.name.eq_ignore_ascii_case(name)
            })
        }) {
            self.stack.truncate(index);
            if name.eq_ignore_ascii_case("svg") || name.eq_ignore_ascii_case("math") {
                self.active_formatting.clear();
            }
            return;
        }
        if is_formatting_tag(name) {
            if !self.adoption_agency_end(name) {
                self.pop_through(name);
            }
            return;
        }
        match name {
            "body" | "html" => self.mode = Mode::AfterBody,
            "p" => self.pop_through("p"),
            "li" => self.pop_through_any(&["li"]),
            "dd" | "dt" => self.pop_through_any(&["dd", "dt"]),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.pop_through_any(&["h1", "h2", "h3", "h4", "h5", "h6"]);
            }
            "td" | "th" | "caption" | "template" => {
                self.clear_formatting_to_marker();
                self.pop_through(name);
            }
            "br" | "head" => {}
            _ => self.pop_through(name),
        }
    }

    fn add_formatting(&mut self, node: NodeId, name: &str, attributes: Vec<Attribute>) {
        if let Some(position) = self.active_formatting.iter().rposition(|entry| {
            matches!(
                entry,
                ActiveFormatting::Element {
                    name: entry_name,
                    attributes: entry_attrs,
                    ..
                } if entry_name == name && *entry_attrs == attributes
            )
        }) {
            self.active_formatting.remove(position);
        }
        self.active_formatting.push(ActiveFormatting::Element {
            node,
            name: name.to_owned(),
            attributes,
        });
    }

    fn remove_active_formatting(&mut self, name: &str) {
        if let Some(position) = self.active_formatting.iter().rposition(|entry| {
            matches!(entry, ActiveFormatting::Element { name: entry_name, .. } if entry_name == name)
        }) {
            self.active_formatting.remove(position);
        }
    }

    fn clear_formatting_to_marker(&mut self) {
        while let Some(entry) = self.active_formatting.pop() {
            if matches!(entry, ActiveFormatting::Marker) {
                break;
            }
        }
    }

    /// Re-open active formatting elements that are no longer on the stack.
    fn reconstruct_formatting(&mut self) {
        let start = match self
            .active_formatting
            .iter()
            .rposition(|entry| matches!(entry, ActiveFormatting::Marker))
        {
            Some(index) => index + 1,
            None => 0,
        };
        for index in start..self.active_formatting.len() {
            let (node, name, attributes) = match &self.active_formatting[index] {
                ActiveFormatting::Element {
                    node,
                    name,
                    attributes,
                } => (*node, name.clone(), attributes.clone()),
                ActiveFormatting::Marker => continue,
            };
            if self.stack.contains(&node) {
                continue;
            }
            let parent = self.insertion_parent();
            let new_node = self.doc.create_element_with_attributes(&name, attributes);
            let _ = self.doc.append_child(parent, new_node);
            self.stack.push(new_node);
            if let ActiveFormatting::Element { node, .. } = &mut self.active_formatting[index] {
                *node = new_node;
            }
        }
    }

    /// Pragmatic adoption agency: handles common misnesting, detaching a
    /// formatting element from an open block.
    fn adoption_agency_end(&mut self, name: &str) -> bool {
        if self.doc.tag_name(self.current()) == Some(name) {
            self.stack.pop();
            self.remove_active_formatting(name);
            return true;
        }
        let Some(position) = self.active_formatting.iter().rposition(|entry| {
            matches!(entry, ActiveFormatting::Element { name: entry_name, .. } if entry_name == name)
        }) else {
            return false;
        };
        let (formatting_node, formatting_name, formatting_attributes) =
            match &self.active_formatting[position] {
                ActiveFormatting::Element {
                    node,
                    name,
                    attributes,
                } => (*node, name.clone(), attributes.clone()),
                ActiveFormatting::Marker => return false,
            };
        let Some(stack_index) = self.stack.iter().rposition(|&id| id == formatting_node) else {
            self.active_formatting.remove(position);
            return true;
        };

        let furthest_block = (stack_index + 1..self.stack.len()).find(|&index| {
            self.doc
                .tag_name(self.stack[index])
                .is_some_and(is_special_element)
        });
        if let Some(block_index) = furthest_block {
            let block = self.stack[block_index];
            if let Some(parent) = self.doc.parent(formatting_node) {
                let siblings: Vec<NodeId> = self.doc.children(parent).to_vec();
                let formatting_position = siblings.iter().position(|&id| id == formatting_node);
                let next_sibling =
                    formatting_position.and_then(|index| siblings.get(index + 1).copied());
                let _ = self.doc.remove_child(formatting_node, block);
                let _ = self.doc.insert_before(parent, block, next_sibling);
            }
            // Re-wrap the block's existing content in a clone of the
            // formatting element, approximating the adoption agency loop.
            let clone = self
                .doc
                .create_element_with_attributes(&formatting_name, formatting_attributes);
            let existing_children: Vec<NodeId> = self.doc.children(block).to_vec();
            let _ = self.doc.append_child(block, clone);
            for child in existing_children {
                let _ = self.doc.remove_child(block, child);
                let _ = self.doc.append_child(clone, child);
            }
            self.stack.remove(stack_index);
            self.active_formatting.remove(position);
            return true;
        }

        self.stack.truncate(stack_index);
        self.active_formatting.remove(position);
        true
    }

    fn close_p(&mut self) {
        if self
            .stack
            .iter()
            .any(|&id| self.doc.tag_name(id) == Some("p"))
        {
            self.pop_through("p");
        }
    }

    fn close_heading(&mut self) {
        self.pop_through_any(&["h1", "h2", "h3", "h4", "h5", "h6"]);
    }

    fn close_list_item(&mut self, item: &str, boundaries: &[&str]) {
        let mut index = None;
        for (i, &id) in self.stack.iter().enumerate().rev() {
            let Some(tag) = self.doc.tag_name(id) else {
                continue;
            };
            if tag == item {
                index = Some(i);
                break;
            }
            if boundaries.contains(&tag) {
                break;
            }
        }
        if let Some(index) = index {
            self.stack.truncate(index);
        }
    }

    fn pop_through(&mut self, name: &str) {
        let Some(index) = self
            .stack
            .iter()
            .rposition(|&id| self.doc.tag_name(id) == Some(name))
        else {
            return;
        };
        // Never close the root html element implicitly.
        if index == 0 {
            return;
        }
        self.stack.truncate(index);
    }

    fn pop_through_any(&mut self, names: &[&str]) {
        let Some(index) = self.stack.iter().rposition(|&id| {
            self.doc
                .tag_name(id)
                .is_some_and(|tag| names.contains(&tag))
        }) else {
            return;
        };
        if index == 0 {
            return;
        }
        self.stack.truncate(index);
    }

    fn pop_through_any_bounded(&mut self, names: &[&str], boundaries: &[&str]) {
        for index in (1..self.stack.len()).rev() {
            let Some(tag) = self.doc.tag_name(self.stack[index]) else {
                continue;
            };
            if names.contains(&tag) {
                self.stack.truncate(index);
                return;
            }
            if boundaries.contains(&tag) {
                return;
            }
        }
    }
}

fn remap_start_tag(token: &Token, new_name: &str) -> Token {
    match token {
        Token::StartTag {
            attrs,
            self_closing,
            ..
        } => Token::StartTag {
            name: new_name.to_owned(),
            attrs: attrs.clone(),
            self_closing: *self_closing,
        },
        other => other.clone(),
    }
}

fn classify_quirks(name: &str, public_id: &str, system_id: &str) -> QuirksMode {
    if name != "html" {
        return QuirksMode::Quirks;
    }
    let public_upper = public_id.to_ascii_uppercase();
    let has_system = !system_id.is_empty();

    let limited_prefixes = [
        "-//W3C//DTD XHTML 1.0 FRAMESET//",
        "-//W3C//DTD XHTML 1.0 TRANSITIONAL//",
    ];
    if limited_prefixes
        .iter()
        .any(|prefix| public_upper.starts_with(prefix))
    {
        return QuirksMode::LimitedQuirks;
    }

    let html4_quirky = [
        "-//W3C//DTD HTML 4.01 FRAMESET//",
        "-//W3C//DTD HTML 4.01 TRANSITIONAL//",
    ];
    if html4_quirky
        .iter()
        .any(|prefix| public_upper.starts_with(prefix))
    {
        return if has_system {
            QuirksMode::LimitedQuirks
        } else {
            QuirksMode::Quirks
        };
    }

    let quirks_prefixes = [
        "-//W3O//DTD W3 HTML STRICT//",
        "-//W3C//DTD HTML 4.0 FRAMESET//",
        "-//W3C//DTD HTML 4.0 TRANSITIONAL//",
        "-//W3C//DTD HTML 3.2",
        "-//W3C//DTD HTML 3",
        "-//MICROSOFT//DTD",
        "-//W3C//DTD W3 HTML//",
        "-//IETF//DTD HTML 2.0",
    ];
    if quirks_prefixes
        .iter()
        .any(|prefix| public_upper.starts_with(prefix))
    {
        return QuirksMode::Quirks;
    }

    QuirksMode::NoQuirks
}

fn is_foreign_breakout(name: &str) -> bool {
    matches!(
        name,
        "b" | "big"
            | "blockquote"
            | "body"
            | "br"
            | "center"
            | "code"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "em"
            | "embed"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "listing"
            | "menu"
            | "meta"
            | "nobr"
            | "ol"
            | "p"
            | "pre"
            | "ruby"
            | "s"
            | "small"
            | "span"
            | "strong"
            | "strike"
            | "sub"
            | "sup"
            | "table"
            | "tt"
            | "u"
            | "ul"
            | "var"
    )
}

fn adjust_foreign_tag_name(namespace: Namespace, name: &str) -> String {
    if namespace == Namespace::MathMl {
        return name.to_owned();
    }
    let adjusted = match name {
        "altglyph" => "altGlyph",
        "altglyphdef" => "altGlyphDef",
        "altglyphitem" => "altGlyphItem",
        "animatecolor" => "animateColor",
        "animatemotion" => "animateMotion",
        "animatetransform" => "animateTransform",
        "clippath" => "clipPath",
        "feblend" => "feBlend",
        "fecolormatrix" => "feColorMatrix",
        "fecomponenttransfer" => "feComponentTransfer",
        "fecomposite" => "feComposite",
        "feconvolvematrix" => "feConvolveMatrix",
        "fediffuselighting" => "feDiffuseLighting",
        "fedisplacementmap" => "feDisplacementMap",
        "fedistantlight" => "feDistantLight",
        "fedropshadow" => "feDropShadow",
        "feflood" => "feFlood",
        "fefunca" => "feFuncA",
        "fefuncb" => "feFuncB",
        "fefuncg" => "feFuncG",
        "fefuncr" => "feFuncR",
        "fegaussianblur" => "feGaussianBlur",
        "feimage" => "feImage",
        "femerge" => "feMerge",
        "femergenode" => "feMergeNode",
        "femorphology" => "feMorphology",
        "feoffset" => "feOffset",
        "fepointlight" => "fePointLight",
        "fespecularlighting" => "feSpecularLighting",
        "fespotlight" => "feSpotLight",
        "fetile" => "feTile",
        "feturbulence" => "feTurbulence",
        "foreignobject" => "foreignObject",
        "glyphref" => "glyphRef",
        "lineargradient" => "linearGradient",
        "radialgradient" => "radialGradient",
        "textpath" => "textPath",
        other => other,
    };
    adjusted.to_owned()
}

fn adjust_foreign_attribute_name(namespace: Namespace, name: &str) -> String {
    if namespace == Namespace::MathMl {
        return if name == "definitionurl" {
            "definitionURL".to_owned()
        } else {
            name.to_owned()
        };
    }
    let adjusted = match name {
        "attributename" => "attributeName",
        "attributetype" => "attributeType",
        "basefrequency" => "baseFrequency",
        "baseprofile" => "baseProfile",
        "calcmode" => "calcMode",
        "clippathunits" => "clipPathUnits",
        "diffuseconstant" => "diffuseConstant",
        "edgemode" => "edgeMode",
        "filterunits" => "filterUnits",
        "glyphref" => "glyphRef",
        "gradienttransform" => "gradientTransform",
        "gradientunits" => "gradientUnits",
        "kernelmatrix" => "kernelMatrix",
        "kernelunitlength" => "kernelUnitLength",
        "keypoints" => "keyPoints",
        "keysplines" => "keySplines",
        "keytimes" => "keyTimes",
        "lengthadjust" => "lengthAdjust",
        "limitingconeangle" => "limitingConeAngle",
        "markerheight" => "markerHeight",
        "markerunits" => "markerUnits",
        "markerwidth" => "markerWidth",
        "maskcontentunits" => "maskContentUnits",
        "maskunits" => "maskUnits",
        "numoctaves" => "numOctaves",
        "pathlength" => "pathLength",
        "patterncontentunits" => "patternContentUnits",
        "patterntransform" => "patternTransform",
        "patternunits" => "patternUnits",
        "pointsatx" => "pointsAtX",
        "pointsaty" => "pointsAtY",
        "pointsatz" => "pointsAtZ",
        "preservealpha" => "preserveAlpha",
        "preserveaspectratio" => "preserveAspectRatio",
        "primitiveunits" => "primitiveUnits",
        "refx" => "refX",
        "refy" => "refY",
        "repeatcount" => "repeatCount",
        "repeatdur" => "repeatDur",
        "requiredextensions" => "requiredExtensions",
        "requiredfeatures" => "requiredFeatures",
        "specularconstant" => "specularConstant",
        "specularexponent" => "specularExponent",
        "spreadmethod" => "spreadMethod",
        "startoffset" => "startOffset",
        "stddeviation" => "stdDeviation",
        "stitchtiles" => "stitchTiles",
        "surfacescale" => "surfaceScale",
        "systemlanguage" => "systemLanguage",
        "tablevalues" => "tableValues",
        "targetx" => "targetX",
        "targety" => "targetY",
        "textlength" => "textLength",
        "viewbox" => "viewBox",
        "viewtarget" => "viewTarget",
        "xchannelselector" => "xChannelSelector",
        "ychannelselector" => "yChannelSelector",
        "zoomandpan" => "zoomAndPan",
        other => other,
    };
    adjusted.to_owned()
}

fn is_formatting_tag(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    )
}

fn should_reconstruct_before(name: &str) -> bool {
    !closes_p(name)
        && !matches!(
            name,
            "a" | "html"
                | "head"
                | "body"
                | "table"
                | "caption"
                | "colgroup"
                | "col"
                | "tbody"
                | "thead"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
                | "li"
                | "dd"
                | "dt"
                | "option"
                | "optgroup"
                | "form"
                | "script"
                | "style"
                | "template"
                | "title"
                | "meta"
                | "link"
                | "base"
                | "br"
                | "hr"
                | "img"
                | "input"
                | "select"
                | "textarea"
                | "button"
                | "iframe"
                | "noembed"
                | "noframes"
                | "plaintext"
        )
}

fn is_special_element(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "applet"
            | "article"
            | "aside"
            | "blockquote"
            | "button"
            | "caption"
            | "center"
            | "details"
            | "dir"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "iframe"
            | "li"
            | "main"
            | "marquee"
            | "menu"
            | "nav"
            | "object"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn is_space(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

fn is_head_void(name: &str) -> bool {
    matches!(name, "base" | "basefont" | "bgsound" | "link" | "meta")
}

fn is_head_rawtext(name: &str) -> bool {
    matches!(name, "title" | "style" | "script")
}

fn is_after_head_tag(name: &str) -> bool {
    matches!(
        name,
        "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noframes"
            | "script"
            | "style"
            | "template"
    )
}

fn closes_p(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "center"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "header"
            | "hgroup"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "pre"
            | "listing"
            | "section"
            | "summary"
            | "ul"
            | "select"
            | "textarea"
    )
}
