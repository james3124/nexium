use nexium::engine::css::tokenizer::{Token, tokenize};
use nexium::engine::css::{
    Color, ComputedStyle, Display, LineHeight, StyleResolver, Viewport, parse_declaration_list,
    parse_stylesheet, user_agent_stylesheet,
};
use nexium::engine::dom::{Document, NodeId};
use nexium::parse_html;

fn inline_style(doc: &Document, node: NodeId) -> Vec<nexium::engine::css::Declaration> {
    doc.get_attribute(node, "style")
        .map(parse_declaration_list)
        .unwrap_or_default()
}

fn compute(css: &str, html: &str, selector: &str, viewport: Viewport) -> ComputedStyle {
    let doc = parse_html(html);
    let author = [parse_stylesheet(css)];
    let resolver = StyleResolver::new(&doc, user_agent_stylesheet(), &author, viewport);
    let node = doc.select(selector).unwrap().unwrap();
    let root = doc.select("html").unwrap().unwrap();

    let initial = ComputedStyle::initial();
    let root_style = resolver.resolve(root, &initial, &inline_style(&doc, root), 16.0);
    let root_font_size = root_style.font_size;

    let mut chain = doc.ancestors(node);
    chain.reverse();
    let mut style = root_style;
    for &ancestor in chain.iter().skip(1) {
        style = resolver.resolve(
            ancestor,
            &style,
            &inline_style(&doc, ancestor),
            root_font_size,
        );
    }
    resolver.resolve(node, &style, &inline_style(&doc, node), root_font_size)
}

fn default_viewport() -> Viewport {
    Viewport {
        width: 800.0,
        height: 600.0,
    }
}

#[test]
fn tokenizes_core_css_syntax() {
    let tokens = tokenize(r#"a.b > #id { color: rgb(1, 2, 3); width: 50%; margin: -10px; }"#);
    assert!(tokens.contains(&Token::Ident("a".into())));
    assert!(tokens.contains(&Token::Percentage(50.0)));
    assert!(
        tokens.iter().any(
            |t| matches!(t, Token::Dimension { value, unit } if *value == -10.0 && unit == "px")
        )
    );
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t, Token::Function(name) if name == "rgb"))
    );
    assert!(tokens.contains(&Token::Hash {
        value: "id".into(),
        id: true
    }));
}

#[test]
fn parses_rules_and_important() {
    let sheet = parse_stylesheet(
        "p, .x { color: red; margin: 0 !important; } @media (min-width: 600px) { a { color: blue; } }",
    );
    assert_eq!(sheet.rules.len(), 2);
}

#[test]
fn parses_declarations_from_inline_style() {
    let declarations = parse_declaration_list("color: red; margin: 4px 8px; background: #fff");
    assert_eq!(declarations.len(), 3);
    assert_eq!(declarations[0].name, "color");
}

#[test]
fn specificity_and_source_order() {
    let style = compute(
        "p { color: red; } .a { color: green; } #target { color: blue; }",
        r#"<p id="target" class="a">x</p>"#,
        "#target",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 0, 255, 255));

    let style = compute(
        "p { color: red; } p { color: green; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 128, 0, 255));
}

#[test]
fn important_and_inline_styles_override() {
    let style = compute(
        "p { color: red !important; } #x { color: blue; }",
        r#"<p id="x" style="color: green">x</p>"#,
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));

    let style = compute(
        "p { color: red; }",
        r#"<p style="color: green !important">x</p>"#,
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 128, 0, 255));
}

#[test]
fn inheritance_of_font_and_color() {
    let style = compute(
        "body { color: #123456; font-size: 20px; }",
        "<body><div><p>x</p></div></body>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0x12, 0x34, 0x56, 255));
    assert_eq!(style.font_size, 20.0);
}

#[test]
fn relative_font_sizes() {
    let style = compute(
        "html { font-size: 20px; } p { font-size: 1.5em; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.font_size, 30.0);

    let style = compute(
        "html { font-size: 20px; } p { font-size: 150%; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.font_size, 30.0);
}

#[test]
fn unitless_line_height_inherits_as_factor() {
    let style = compute(
        "body { line-height: 1.5; } p { font-size: 20px; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.line_height, 30.0);
    assert_eq!(style.line_height_factor, Some(1.5));
}

#[test]
fn margin_and_padding_shorthands() {
    let style = compute(
        "p { margin: 10px 20px; padding: 1px 2px 3px 4px; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.margin.top, nexium::engine::css::Length::Px(10.0));
    assert_eq!(style.margin.right, nexium::engine::css::Length::Px(20.0));
    assert_eq!(style.margin.bottom, nexium::engine::css::Length::Px(10.0));
    assert_eq!(style.margin.left, nexium::engine::css::Length::Px(20.0));
    assert_eq!(style.padding.top, nexium::engine::css::Length::Px(1.0));
    assert_eq!(style.padding.right, nexium::engine::css::Length::Px(2.0));
    assert_eq!(style.padding.bottom, nexium::engine::css::Length::Px(3.0));
    assert_eq!(style.padding.left, nexium::engine::css::Length::Px(4.0));
}

#[test]
fn border_shorthand_and_current_color() {
    let style = compute(
        "p { color: rgb(10, 20, 30); border: 2px solid; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.border_width.top, 2.0);
    assert_eq!(
        style.border_style.top,
        nexium::engine::css::BorderStyle::Solid
    );
    assert_eq!(style.border_color.top, Color::rgba(10, 20, 30, 255));
}

#[test]
fn color_formats() {
    for (css, expected) in [
        ("p { color: #ff0000; }", Color::rgba(255, 0, 0, 255)),
        ("p { color: #f00; }", Color::rgba(255, 0, 0, 255)),
        ("p { color: red; }", Color::rgba(255, 0, 0, 255)),
        ("p { color: rgb(255, 0, 0); }", Color::rgba(255, 0, 0, 255)),
        ("p { color: rgb(100% 0% 0%); }", Color::rgba(255, 0, 0, 255)),
        (
            "p { color: rgba(255, 0, 0, 0.5); }",
            Color::rgba(255, 0, 0, 128),
        ),
        (
            "p { color: hsl(0, 100%, 50%); }",
            Color::rgba(255, 0, 0, 255),
        ),
    ] {
        let style = compute(css, "<p>x</p>", "p", default_viewport());
        assert_eq!(style.color, expected, "{css}");
    }
}

#[test]
fn css_variables_and_fallbacks() {
    let style = compute(
        ":root { --brand: #00ff00; --spacing: 12px; } p { color: var(--brand); margin: var(--spacing); }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 255, 0, 255));
    assert_eq!(style.margin.top, nexium::engine::css::Length::Px(12.0));

    let style = compute(
        "p { color: var(--missing, blue); }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 0, 255, 255));
}

#[test]
fn css_variables_are_inherited() {
    let style = compute(
        "body { --c: rgb(1, 2, 3); } p { color: var(--c); }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(1, 2, 3, 255));
}

#[test]
fn media_queries_are_evaluated_against_viewport() {
    let css = "@media (min-width: 600px) { p { color: red; } }";
    let wide = compute(
        css,
        "<p>x</p>",
        "p",
        Viewport {
            width: 800.0,
            height: 600.0,
        },
    );
    assert_eq!(wide.color, Color::rgba(255, 0, 0, 255));

    let narrow = compute(
        css,
        "<p>x</p>",
        "p",
        Viewport {
            width: 400.0,
            height: 600.0,
        },
    );
    assert_eq!(narrow.color, Color::BLACK);
}

#[test]
fn ua_defaults_for_common_elements() {
    let style = compute("", "<h1>x</h1>", "h1", default_viewport());
    assert_eq!(style.display, Display::Block);
    assert_eq!(style.font_size, 32.0);
    assert_eq!(style.font_weight, 700);

    let style = compute("", "<script>1</script>", "script", default_viewport());
    assert_eq!(style.display, Display::None);

    let style = compute("", "<p>x</p>", "p", default_viewport());
    assert_eq!(style.margin.top, nexium::engine::css::Length::Em(1.0));
    assert_eq!(style.line_height, 19.2);
}

#[test]
fn line_height_lengths() {
    let style = compute(
        "p { font-size: 20px; line-height: 30px; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.line_height, 30.0);
    let _ = LineHeight::Normal;
}

#[test]
fn display_and_position_properties() {
    let style = compute(
        "p { display: none; position: absolute; left: 10px; top: 20%; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.display, Display::None);
    assert_eq!(style.position, nexium::engine::css::Position::Absolute);
    assert_eq!(style.left, nexium::engine::css::Length::Px(10.0));
    assert_eq!(style.top, nexium::engine::css::Length::Percent(20.0));
}

#[test]
fn pseudo_class_selectors() {
    let doc = parse_html("<ul><li>a</li><li>b</li><li>c</li></ul>");
    assert_eq!(doc.select_all("li:first-child").unwrap().len(), 1);
    assert_eq!(doc.select_all("li:last-child").unwrap().len(), 1);
    assert_eq!(doc.select_all("li:nth-child(2)").unwrap().len(), 1);
    assert_eq!(doc.select_all("li:nth-child(2n+1)").unwrap().len(), 2);
    assert_eq!(doc.select_all("li:not(:last-child)").unwrap().len(), 2);
    assert_eq!(doc.select_all("li ~ li").unwrap().len(), 2);

    let doc = parse_html("<div><span class=a></span><span></span></div>");
    assert_eq!(doc.select_all("span:only-child").unwrap().len(), 0);
    assert_eq!(doc.select_all("span:empty").unwrap().len(), 2);
}

#[test]
fn attribute_operators() {
    let doc = parse_html(
        r#"<a class="btn primary" href="https://example.com/x.png"></a><a class="btn"></a>"#,
    );
    assert_eq!(doc.select_all(r#"[class~="primary"]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[href^="https"]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[href$=".png"]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[href*="example"]"#).unwrap().len(), 1);
    assert_eq!(doc.select_all(r#"[class|="btn"]"#).unwrap().len(), 1);
}

fn compute_pseudo(
    css: &str,
    html: &str,
    selector: &str,
    pseudo: &str,
    viewport: Viewport,
) -> ComputedStyle {
    let doc = parse_html(html);
    let author = [parse_stylesheet(css)];
    let resolver = StyleResolver::new(&doc, user_agent_stylesheet(), &author, viewport);
    let node = doc.select(selector).unwrap().unwrap();
    let root = doc.select("html").unwrap().unwrap();
    let initial = ComputedStyle::initial();
    let root_style = resolver.resolve(root, &initial, &[], 16.0);
    let root_font_size = root_style.font_size;
    let mut chain = doc.ancestors(node);
    chain.reverse();
    let mut style = root_style;
    for &ancestor in chain.iter().skip(1) {
        style = resolver.resolve(
            ancestor,
            &style,
            &inline_style(&doc, ancestor),
            root_font_size,
        );
    }
    let element_style = resolver.resolve(node, &style, &inline_style(&doc, node), root_font_size);
    resolver.resolve_pseudo(node, &element_style, pseudo, root_font_size)
}

#[test]
fn generated_content_pseudo_styles() {
    let style = compute_pseudo(
        "p { font-size: 20px; } p::before { content: \"→ \"; color: red; }",
        "<p>x</p>",
        "p",
        "before",
        default_viewport(),
    );
    assert_eq!(style.content.as_deref(), Some("→ "));
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));
    assert_eq!(style.font_size, 20.0);
    assert_eq!(style.display, Display::Inline);

    let style = compute_pseudo(
        "p::after { content: attr(data-tail); }",
        "<p data-tail=\"!\">x</p>",
        "p",
        "after",
        default_viewport(),
    );
    assert_eq!(style.content.as_deref(), Some("!"));

    let style = compute_pseudo(
        "p::before { content: none; }",
        "<p>x</p>",
        "p",
        "before",
        default_viewport(),
    );
    assert_eq!(style.content, None);
}

#[test]
fn pseudo_selectors_do_not_match_real_elements() {
    let doc = parse_html("<p>x</p>");
    assert!(doc.select_all("p::before").unwrap().is_empty());
    assert!(doc.select_all("p::after").unwrap().is_empty());
}

#[test]
fn calc_expressions_resolve_in_computed_styles() {
    let style = compute(
        "p { font-size: calc(1em + 4px); margin: calc(2 * 5px); padding: calc(10px / 2); }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.font_size, 20.0);
    assert_eq!(
        style.margin.top,
        nexium::engine::css::Length::Calc(nexium::engine::css::values::LinearLength {
            px: 10.0,
            ..Default::default()
        })
    );
    assert_eq!(
        style.padding.top,
        nexium::engine::css::Length::Calc(nexium::engine::css::values::LinearLength {
            px: 5.0,
            ..Default::default()
        })
    );

    let style = compute(
        "p { font-size: calc((10px + 2px) * 2 - 4px); }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.font_size, 20.0);

    let style = compute(
        "p { font-size: 30px; font-size: calc(10px * 2px); }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.font_size, 30.0);
}

#[test]
fn supports_rules_are_evaluated() {
    let style = compute(
        "@supports (display: flex) { p { color: red; } }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(255, 0, 0, 255));

    let style = compute(
        "@supports (text-shadow: 0 0 1px #000) { p { color: red; } }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::BLACK);

    let style = compute(
        "@supports (display: grid) { p { color: red; } }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::BLACK);

    let style = compute(
        "@supports not (display: flex) { p { color: red; } }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::BLACK);

    let style = compute(
        "@supports (display: flex) and (color: red) { p { color: blue; } }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.color, Color::rgba(0, 0, 255, 255));
}

#[test]
fn media_orientation_and_aspect_ratio() {
    let css = "@media (orientation: portrait) { p { color: red; } }";
    let portrait = compute(
        css,
        "<p>x</p>",
        "p",
        Viewport {
            width: 400.0,
            height: 800.0,
        },
    );
    assert_eq!(portrait.color, Color::rgba(255, 0, 0, 255));
    let landscape = compute(
        css,
        "<p>x</p>",
        "p",
        Viewport {
            width: 800.0,
            height: 400.0,
        },
    );
    assert_eq!(landscape.color, Color::BLACK);

    let css = "@media (aspect-ratio: 16/9) { p { color: red; } }";
    let matched = compute(
        css,
        "<p>x</p>",
        "p",
        Viewport {
            width: 800.0,
            height: 450.0,
        },
    );
    assert_eq!(matched.color, Color::rgba(255, 0, 0, 255));
}

#[test]
fn background_and_radius_properties() {
    let style = compute(
        "p { background-image: url(a.png); background-repeat: no-repeat; background-position: right bottom; background-size: cover; border-radius: 12px; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(
        style.background_repeat,
        nexium::engine::css::BackgroundRepeat::NoRepeat
    );
    assert_eq!(
        style.background_size.mode,
        nexium::engine::css::BackgroundSizeMode::Cover
    );
    assert_eq!(
        style.background_position,
        (
            nexium::engine::css::Length::Percent(100.0),
            nexium::engine::css::Length::Percent(100.0)
        )
    );
    assert_eq!(style.border_radius, nexium::engine::css::Length::Px(12.0));

    let style = compute(
        "p { background: #112233 url(b.png) no-repeat center / 50% auto; }",
        "<p>x</p>",
        "p",
        default_viewport(),
    );
    assert_eq!(style.background_color, Color::rgba(0x11, 0x22, 0x33, 255));
    assert_eq!(style.background_image.as_deref(), Some("b.png"));
    assert_eq!(
        style.background_repeat,
        nexium::engine::css::BackgroundRepeat::NoRepeat
    );
    assert_eq!(
        style.background_position.0,
        nexium::engine::css::Length::Percent(50.0)
    );
    assert_eq!(
        style.background_size.width,
        nexium::engine::css::Length::Percent(50.0)
    );
}
