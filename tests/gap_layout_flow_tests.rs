//! Coverage for CSS float/clear in normal flow and `table-layout: fixed`
//! column distribution.

use image::RgbaImage;
use nexium::engine::css::values::{Clear, Float, TableLayout};
use nexium::engine::layout::{LayoutBox, LayoutResult};
use nexium::{Browser, Config, NodeId, RenderOptions, RenderedPage};

async fn render(html: &str, width: f32, height: f32) -> (nexium::Page, RenderedPage) {
    let browser = Browser::new(Config::default());
    let page = browser
        .load_html("https://gap-flow.test/", html)
        .expect("page");
    let rendered = browser
        .render(
            &page,
            RenderOptions {
                viewport: nexium::engine::css::Viewport { width, height },
                ..RenderOptions::default()
            },
        )
        .await
        .expect("render");
    (page, rendered)
}

fn find_box<'a>(root: &'a LayoutBox, node: NodeId) -> Option<&'a LayoutBox> {
    if root.node == Some(node) {
        return Some(root);
    }
    root.children
        .iter()
        .chain(root.atomics.iter())
        .find_map(|child| find_box(child, node))
}

fn node(page: &nexium::Page, selector: &str) -> NodeId {
    page.select(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("missing {selector}"))
}

fn first_fragment_x(root: &LayoutBox, node: NodeId, line: usize) -> f32 {
    let box_ = find_box(root, node).unwrap_or_else(|| panic!("no layout box for {node:?}"));
    box_.lines[line].fragments[0].x
}

// ---------------------------------------------------------------------------
// CSS parsing / computed values
// ---------------------------------------------------------------------------

#[test]
fn float_clear_table_layout_parse_to_computed_values() {
    use nexium::Page;
    use nexium::engine::css::{ComputedStyle, StyleResolver, user_agent_stylesheet};

    let initial = ComputedStyle::initial();
    assert_eq!(initial.float, Float::None);
    assert_eq!(initial.clear, Clear::None);
    assert_eq!(initial.table_layout, TableLayout::Auto);

    let html = "<style>#a{float:left;clear:both} #b{float:right} #c{clear:left} table{table-layout:fixed}</style>\
                <div id=a></div><div id=b></div><div id=c></div><table id=t></table>";
    let page = Page::from_html(url::Url::parse("https://gap-flow.test/").unwrap(), html);
    let resolver = StyleResolver::new(
        &page.document,
        user_agent_stylesheet(),
        &page.stylesheets,
        nexium::engine::css::Viewport {
            width: 800.0,
            height: 600.0,
        },
    );
    let mut style = ComputedStyle::initial();
    let root = page.select("html").unwrap().unwrap();
    let root_style = resolver.resolve(root, &style, &[], 16.0);

    let a = node(&page, "#a");
    style = resolver.resolve(a, &root_style, &[], 16.0);
    assert_eq!(style.float, Float::Left);
    assert_eq!(style.clear, Clear::Both);

    let b = node(&page, "#b");
    let b_style = resolver.resolve(b, &root_style, &[], 16.0);
    assert_eq!(b_style.float, Float::Right);

    let c = node(&page, "#c");
    let c_style = resolver.resolve(c, &root_style, &[], 16.0);
    assert_eq!(c_style.clear, Clear::Left);
    // Non-inherited: a child of a left-floated parent is still un-floated.
    assert_eq!(b_style.float, Float::Right);
    assert_eq!(c_style.float, Float::None);

    let table = node(&page, "#t");
    let table_style = resolver.resolve(table, &root_style, &[], 16.0);
    assert_eq!(table_style.table_layout, TableLayout::Fixed);
}

// ---------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn left_float_sits_at_left_edge_and_inline_text_wraps_beside_it() {
    let html = "<style>body{margin:0}#f{float:left;width:100px;height:100px;background:#ff0000}\
                #t{width:300px}</style>\
                <div id=f></div><div id=t>alpha beta gamma delta epsilon zeta eta theta \
                iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega \
                alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron \
                pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta \
                eta theta iota kappa lambda mu nu xi omicron pi rho sigma</div>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let f = node(&page, "#f");
    let f_rect = rendered.layout.node_rect(f).unwrap();
    assert_eq!(
        (f_rect.x, f_rect.y, f_rect.width, f_rect.height),
        (0.0, 0.0, 100.0, 100.0)
    );

    let t = node(&page, "#t");
    let box_ = find_box(&rendered.layout.root, t).unwrap();
    assert!(!box_.lines.is_empty(), "expected wrapped lines");
    // Lines overlapping the float start to its right.
    assert!(
        first_fragment_x(&rendered.layout.root, t, 0) >= 99.0,
        "first text fragment should start beside the float: {:?}",
        box_.lines[0]
    );
    // Once past the float's bottom, lines use the full width again.
    let mut full_width_line = false;
    for line in &box_.lines {
        if line.y >= f_rect.bottom() {
            if let Some(fragment) = line.fragments.first() {
                assert!(
                    fragment.x < 99.0,
                    "line below float should reset: {fragment:?}"
                );
                full_width_line = true;
            }
        }
    }
    assert!(
        full_width_line,
        "expected at least one line below the float"
    );
}

#[tokio::test]
async fn right_float_sits_at_right_edge_and_text_starts_at_left() {
    let html = "<style>body{margin:0}#f{float:right;width:100px;height:80px;background:#0000ff}\
                #t{width:400px}</style>\
                <div id=f></div><div id=t>one two three four five six seven eight nine ten \
                eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty \
                twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven</div>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let f = node(&page, "#f");
    let f_rect = rendered.layout.node_rect(f).unwrap();
    assert_eq!(f_rect.x, 300.0, "right float should hug the right edge");
    assert_eq!(f_rect.width, 100.0);

    let t = node(&page, "#t");
    assert_eq!(
        first_fragment_x(&rendered.layout.root, t, 0),
        0.0,
        "text should start at the left of a right float"
    );
    let box_ = find_box(&rendered.layout.root, t).unwrap();
    for line in box_.lines.iter().filter(|l| l.y < f_rect.bottom()) {
        for fragment in &line.fragments {
            assert!(
                fragment.x + fragment.width <= f_rect.x + 0.01,
                "text overlapped right float: {fragment:?}"
            );
        }
    }
}

#[tokio::test]
async fn clear_left_pushes_following_block_below_float() {
    let html = "<style>body{margin:0}#f{float:left;width:100px;height:60px;background:red}\
                #c{clear:left;height:20px;background:green}</style>\
                <div id=f></div><div id=c></div>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let f_rect = rendered.layout.node_rect(node(&page, "#f")).unwrap();
    let c_rect = rendered.layout.node_rect(node(&page, "#c")).unwrap();
    assert!(
        c_rect.y >= f_rect.bottom() - 0.01,
        "clear should move #c below #f: {f_rect:?} {c_rect:?}"
    );
}

#[tokio::test]
async fn without_clear_following_block_starts_at_float_top() {
    let html = "<style>body{margin:0}#f{float:left;width:100px;height:60px;background:red}\
                #c{height:20px;background:green}</style>\
                <div id=f></div><div id=c></div>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let c_rect = rendered.layout.node_rect(node(&page, "#c")).unwrap();
    assert_eq!(c_rect.y, 0.0, "floats leave flow so #c starts at the top");
}

#[tokio::test]
async fn inline_level_float_is_positioned_and_text_wraps() {
    let html = "<style>body{margin:0}#t{width:300px}#f{float:left;width:50px;height:50px;background:#ff0000}</style>\
                <div id=t><span id=f></span>alpha beta gamma delta epsilon zeta eta theta iota \
                kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha \
                beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron</div>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let f = node(&page, "#f");
    let f_rect = rendered.layout.node_rect(f).unwrap();
    assert_eq!(f_rect.x, 0.0);
    assert_eq!(f_rect.width, 50.0);
    assert_eq!(f_rect.height, 50.0);

    let t = node(&page, "#t");
    let box_ = find_box(&rendered.layout.root, t).unwrap();
    // The first line may contain only the float; find the first text fragment.
    let first_text = box_
        .lines
        .iter()
        .flat_map(|line| line.fragments.iter())
        .find(|fragment| matches!(fragment.kind, nexium::engine::layout::FragmentKind::Text(_)))
        .expect("expected text fragment");
    assert!(
        first_text.x >= 49.0,
        "inline text should wrap beside the float: {first_text:?}"
    );
}

#[tokio::test]
async fn float_background_is_painted_at_its_position() {
    let html = "<style>body{margin:0}#f{float:left;width:40px;height:40px;background:#ff0000}</style>\
                <div id=f></div><div>some text here</div>";
    let (page, rendered) = render(html, 400.0, 200.0).await;
    let f_rect = rendered.layout.node_rect(node(&page, "#f")).unwrap();
    let pixel = *rendered
        .image
        .get_pixel((f_rect.x + 10.0) as u32, (f_rect.y + 10.0) as u32);
    assert_eq!(
        pixel,
        image::Rgba([255, 0, 0, 255]),
        "float should be painted"
    );
}

#[tokio::test]
async fn flex_column_wrap_creates_a_second_column() {
    let html = "<style>body{margin:0}#f{display:flex;flex-direction:column;flex-wrap:wrap;\
                width:250px;height:60px;gap:0}.i{width:100px;height:25px}</style>\
                <div id=f><div class=i id=a></div><div class=i id=b></div>\
                <div class=i id=c></div><div class=i id=d></div></div>";
    let (page, rendered) = render(html, 500.0, 200.0).await;
    let rect = |selector| rendered.layout.node_rect(node(&page, selector)).unwrap();
    let (a, b, c, d) = (rect("#a"), rect("#b"), rect("#c"), rect("#d"));
    assert_eq!((a.x, a.y), (0.0, 0.0));
    assert_eq!((b.x, b.y), (0.0, 25.0));
    assert_eq!((c.x, c.y), (100.0, 0.0), "third item wraps to column two");
    assert_eq!((d.x, d.y), (100.0, 25.0));
}

#[tokio::test]
async fn flex_column_without_wrap_stays_in_one_column() {
    let html = "<style>body{margin:0}#f{display:flex;flex-direction:column;\
                width:250px;height:60px;gap:0}.i{width:100px;height:25px}</style>\
                <div id=f><div class=i id=a></div><div class=i id=b></div>\
                <div class=i id=c></div><div class=i id=d></div></div>";
    let (page, rendered) = render(html, 500.0, 200.0).await;
    let rect = |selector| rendered.layout.node_rect(node(&page, selector)).unwrap();
    assert_eq!(
        (rect("#a").x, rect("#b").x, rect("#c").x, rect("#d").x),
        (0.0, 0.0, 0.0, 0.0)
    );
    assert_eq!((rect("#c").y, rect("#d").y), (50.0, 75.0));
}

// ---------------------------------------------------------------------------
// table-layout: fixed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fixed_table_uses_col_widths_then_distributes_remainder() {
    let html = "<style>body{margin:0}\
                table{width:400px;table-layout:fixed;border-spacing:0}\
                td{padding:0}</style>\
                <table><colgroup>\
                <col style=\"width:100px\"><col width=200><col>\
                </colgroup><tr><td id=c1>1</td><td id=c2>2</td><td id=c3>3</td></tr></table>";
    let (page, rendered) = render(html, 500.0, 200.0).await;
    let c1 = rendered.layout.node_rect(node(&page, "#c1")).unwrap();
    let c2 = rendered.layout.node_rect(node(&page, "#c2")).unwrap();
    let c3 = rendered.layout.node_rect(node(&page, "#c3")).unwrap();
    assert_eq!((c1.x, c1.width), (0.0, 100.0));
    assert_eq!((c2.x, c2.width), (100.0, 200.0));
    assert_eq!((c3.x, c3.width), (300.0, 100.0));
}

#[tokio::test]
async fn fixed_table_honors_stylesheet_rules_on_col_elements() {
    let html = "<style>body{margin:0}\
                table{table-layout:fixed;width:300px;border-spacing:0}\
                col.a{width:80px}td{padding:0}</style>\
                <table><colgroup><col class=a><col></colgroup>\
                <tr><td id=c1>1</td><td id=c2>2</td></tr></table>";
    let (page, rendered) = render(html, 500.0, 200.0).await;
    let c1 = rendered.layout.node_rect(node(&page, "#c1")).unwrap();
    let c2 = rendered.layout.node_rect(node(&page, "#c2")).unwrap();
    assert_eq!((c1.x, c1.width), (0.0, 80.0));
    assert_eq!((c2.x, c2.width), (80.0, 220.0));
}

#[tokio::test]
async fn fixed_table_uses_first_row_widths() {
    let html = "<style>body{margin:0}\
                table{table-layout:fixed;width:400px;border-spacing:0}\
                td{padding:0}</style>\
                <table><tr>\
                <td id=c1 style=\"width:120px\">1</td>\
                <td id=c2>2</td><td id=c3>3</td>\
                </tr></table>";
    let (page, rendered) = render(html, 500.0, 200.0).await;
    let c1 = rendered.layout.node_rect(node(&page, "#c1")).unwrap();
    let c2 = rendered.layout.node_rect(node(&page, "#c2")).unwrap();
    let c3 = rendered.layout.node_rect(node(&page, "#c3")).unwrap();
    assert_eq!((c1.x, c1.width), (0.0, 120.0));
    assert_eq!(c2.width, (400.0 - 120.0) / 2.0);
    assert_eq!(c3.x, 120.0 + c2.width);
}

#[tokio::test]
async fn fixed_table_without_hints_distributes_columns_evenly() {
    let html = "<style>body{margin:0}\
                table{table-layout:fixed;width:300px;border-spacing:0}\
                td{padding:0}</style>\
                <table><tr><td id=c1>1</td><td id=c2>2</td><td id=c3>3</td></tr></table>";
    let (page, rendered) = render(html, 500.0, 200.0).await;
    for (selector, expected_x) in [("#c1", 0.0), ("#c2", 100.0), ("#c3", 200.0)] {
        let rect = rendered.layout.node_rect(node(&page, selector)).unwrap();
        assert_eq!((rect.x, rect.width), (expected_x, 100.0));
    }
}

#[tokio::test]
async fn auto_table_still_uses_intrinsic_content_widths() {
    // Regression guard: a table without `table-layout: fixed` keeps the old
    // content-driven sizing (a very long word widens its column).
    let html = "<style>body{margin:0}table{border-spacing:0}td{padding:0}</style>\
                <table><tr><td id=c1>short</td>\
                <td id=c2>supercalifragilisticexpialidocioussupercalifragilistic</td></tr></table>";
    let (page, rendered) = render(html, 800.0, 200.0).await;
    let c1 = rendered.layout.node_rect(node(&page, "#c1")).unwrap();
    let c2 = rendered.layout.node_rect(node(&page, "#c2")).unwrap();
    assert!(
        c2.width > c1.width,
        "auto layout should size to content: {c1:?} {c2:?}"
    );
}

// Keep the unused import honest while documenting the geometry helper.
#[allow(dead_code)]
fn _assert_layout_type(_: &LayoutResult, _: &RgbaImage) {}
