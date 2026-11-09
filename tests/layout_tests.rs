use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use nexium::engine::css::{StyleResolver, Viewport, user_agent_stylesheet};
use nexium::engine::layout::{LayoutBox, LayoutResult, Rect, layout_document};
use nexium::engine::text::FontSet;
use nexium::{NodeId, Page};

fn layout(html: &str, width: f32, height: f32) -> LayoutResult {
    let page = Page::from_html(url::Url::parse("https://layout.test/").unwrap(), html);
    let viewport = Viewport { width, height };
    let resolver = StyleResolver::new(
        &page.document,
        user_agent_stylesheet(),
        &page.stylesheets,
        viewport,
    );
    let fonts = FontSet::embedded();
    let images: HashMap<String, Arc<RgbaImage>> = HashMap::new();
    layout_document(&page.document, &resolver, &fonts, &images, None)
}

fn document_rect(result: &LayoutResult, html: &str, selector: &str) -> Rect {
    let page = Page::from_html(url::Url::parse("https://layout.test/").unwrap(), html);
    let node = page.select(selector).unwrap().unwrap();
    result.node_rects.get(&node).copied().expect("node rect")
}

fn box_of(result: &LayoutResult, html: &str, selector: &str) -> (NodeId, Rect) {
    let page = Page::from_html(url::Url::parse("https://layout.test/").unwrap(), html);
    let node = page.select(selector).unwrap().unwrap();
    let rect = result.node_rects.get(&node).copied().expect("node rect");
    (node, rect)
}

fn find_box(result: &LayoutResult, node: NodeId) -> Option<&LayoutBox> {
    fn walk(box_: &LayoutBox, target: NodeId) -> Option<&LayoutBox> {
        if box_.node == Some(target) {
            return Some(box_);
        }
        for child in &box_.children {
            if let Some(found) = walk(child, target) {
                return Some(found);
            }
        }
        for atomic in &box_.atomics {
            if let Some(found) = walk(atomic, target) {
                return Some(found);
            }
        }
        None
    }
    walk(&result.root, node)
}

#[test]
fn percentage_width_against_body() {
    let html = "<style>body{margin:0} #a{width:50%;height:10px}</style><div id=a></div>";
    let result = layout(html, 800.0, 600.0);
    let rect = document_rect(&result, html, "#a");
    assert_eq!(rect.width, 400.0);
    assert_eq!(rect.x, 0.0);
}

#[test]
fn box_model_padding_border_margin() {
    let html = "<style>body{margin:0} #a{width:100px;height:50px;padding:10px;border:2px solid;margin:5px}</style><div id=a></div>";
    let result = layout(html, 800.0, 600.0);
    let (node, rect) = box_of(&result, html, "#a");
    assert_eq!(rect.x, 5.0);
    assert_eq!(rect.y, 5.0);
    assert_eq!(rect.width, 124.0);
    assert_eq!(rect.height, 74.0);
    let box_ = find_box(&result, node).unwrap();
    assert_eq!(box_.content_x(), 17.0);
    assert_eq!(box_.content_width(), 100.0);
    assert_eq!(box_.content_height, 50.0);
}

#[test]
fn adjacent_vertical_margins_collapse() {
    let html = "<style>body{margin:0} #a{height:20px;margin-bottom:10px} #b{height:30px;margin-top:15px}</style><div id=a></div><div id=b></div>";
    let result = layout(html, 800.0, 600.0);
    let b = document_rect(&result, html, "#b");
    assert_eq!(b.y, 35.0);
}

#[test]
fn text_wraps_into_multiple_lines() {
    let html = "<style>body{margin:0} #d{width:100px;font-size:16px}</style><div id=d>word word word word word</div>";
    let result = layout(html, 800.0, 600.0);
    let (node, rect) = box_of(&result, html, "#d");
    let box_ = find_box(&result, node).unwrap();
    assert!(box_.lines.len() >= 2, "lines: {}", box_.lines.len());
    assert!(rect.height > 19.0);
}

#[test]
fn inline_blocks_flow_horizontally() {
    let html = "<style>body{margin:0} span{display:inline-block;width:50px;height:20px}</style><div><span id=a></span><span id=b></span></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    assert_eq!(a.x, 0.0);
    assert_eq!(b.x, 50.0);
    assert_eq!(a.y, b.y);
}

#[test]
fn flex_row_distributes_free_space() {
    let html = "<style>body{margin:0} #f{display:flex;width:600px;height:100px} .i{flex:1}</style><div id=f><div class=i id=a></div><div class=i id=b></div><div class=i id=c></div></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let c = document_rect(&result, html, "#c");
    assert!((a.width - 200.0).abs() < 0.5, "{a:?}");
    assert!((b.x - 200.0).abs() < 0.5, "{b:?}");
    assert!((c.x - 400.0).abs() < 0.5, "{c:?}");
}

#[test]
fn flex_justify_center_with_gap() {
    let html = "<style>body{margin:0} #f{display:flex;width:500px;justify-content:center;column-gap:10px} .i{width:50px;height:20px}</style><div id=f><div class=i id=a></div><div class=i id=b></div></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    assert!((a.x - 195.0).abs() < 0.5, "{a:?}");
    assert!((b.x - 255.0).abs() < 0.5, "{b:?}");
}

#[test]
fn flex_column_stacks_items() {
    let html = "<style>body{margin:0} #f{display:flex;flex-direction:column;width:200px} .i{height:30px}</style><div id=f><div class=i id=a></div><div class=i id=b></div></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    assert_eq!(a.y, 0.0);
    assert_eq!(b.y, 30.0);
    assert!((a.width - 200.0).abs() < 0.5);
}

#[test]
fn absolute_positioning_is_relative_to_positioned_ancestor() {
    let html = "<style>body{margin:0} #p{position:relative;width:300px;height:200px;margin-left:50px} #c{position:absolute;left:10px;top:20px;width:50px;height:30px}</style><div id=p><div id=c></div></div>";
    let result = layout(html, 800.0, 600.0);
    let parent = document_rect(&result, html, "#p");
    let child = document_rect(&result, html, "#c");
    assert_eq!(child.x, parent.x + 10.0);
    assert_eq!(child.y, parent.y + 20.0);
    assert_eq!(child.width, 50.0);
    assert_eq!(child.height, 30.0);
}

#[test]
fn relative_offsets_shift_the_box() {
    let html = "<style>body{margin:0} #a{position:relative;left:15px;top:10px;width:100px;height:20px}</style><div id=a></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    assert_eq!(a.x, 15.0);
    assert_eq!(a.y, 10.0);
}

#[test]
fn display_none_is_not_laid_out() {
    let html = "<style>body{margin:0} #a{display:none}</style><div id=a></div><p id=b>x</p>";
    let result = layout(html, 800.0, 600.0);
    let (node, _) = box_of(&result, html, "#b");
    assert!(find_box(&result, node).is_some());
    let page = Page::from_html(url::Url::parse("https://layout.test/").unwrap(), html);
    let hidden = page.select("#a").unwrap().unwrap();
    assert!(!result.node_rects.contains_key(&hidden));
}

#[test]
fn max_width_clamps_and_auto_margins_center() {
    let html = "<style>body{margin:0} #a{width:500px;max-width:200px;margin:0 auto;height:10px}</style><div id=a></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    assert_eq!(a.width, 200.0);
    assert_eq!(a.x, 300.0);
}

#[test]
fn min_height_applies() {
    let html = "<style>body{margin:0} #a{min-height:120px}</style><div id=a></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    assert_eq!(a.height, 120.0);
}

#[test]
fn document_height_includes_content() {
    let html = "<style>body{margin:0} #a{height:1500px}</style><div id=a></div>";
    let result = layout(html, 800.0, 600.0);
    assert!(result.document_height >= 1500.0);
}

#[test]
fn form_controls_are_replaced_boxes() {
    let html = "<form><input id=i value=hello><button id=b>Go</button></form>";
    let result = layout(html, 800.0, 600.0);
    let (node, rect) = box_of(&result, html, "#i");
    assert!(rect.width > 100.0);
    assert!(rect.height > 10.0);
    let box_ = find_box(&result, node).unwrap();
    assert_eq!(box_.text.as_deref(), Some("hello"));
}

#[test]
fn generated_before_and_after_content_is_laid_out() {
    let html = "<style>body{margin:0} p::before{content:\"→ \"} p::after{content:attr(data-tail)}</style><p data-tail=\"!\">hi</p>";
    let result = layout(html, 400.0, 200.0);
    let (node, _) = box_of(&result, html, "p");
    let box_ = find_box(&result, node).unwrap();
    let mut text = String::new();
    for line in &box_.lines {
        for fragment in &line.fragments {
            match &fragment.kind {
                nexium::engine::layout::FragmentKind::Text(value) => text.push_str(value),
                nexium::engine::layout::FragmentKind::Space => text.push(' '),
                _ => {}
            }
        }
    }
    assert_eq!(text.trim(), "→ hi!");
}

#[test]
fn generated_content_works_in_block_containers() {
    let html = "<style>body{margin:0} #d::before{content:\"B\"} #d::after{content:\"A\"}</style><div id=d><p>x</p></div>";
    let result = layout(html, 400.0, 200.0);
    let (node, _) = box_of(&result, html, "#d");
    let box_ = find_box(&result, node).unwrap();
    let texts: Vec<String> = box_
        .children
        .iter()
        .flat_map(|child| {
            child.lines.iter().flat_map(|line| {
                line.fragments
                    .iter()
                    .filter_map(|fragment| match &fragment.kind {
                        nexium::engine::layout::FragmentKind::Text(value) => Some(value.clone()),
                        _ => None,
                    })
            })
        })
        .collect();
    assert_eq!(texts.first().map(String::as_str), Some("B"));
    assert_eq!(texts.last().map(String::as_str), Some("A"));
}

#[test]
fn calc_widths_resolve_against_the_containing_block() {
    let html =
        "<style>body{margin:0} #a{width:calc(100% - 40px);height:10px}</style><div id=a></div>";
    let result = layout(html, 500.0, 300.0);
    let a = document_rect(&result, html, "#a");
    assert_eq!(a.width, 460.0);
}

#[test]
fn flex_wrap_creates_multiple_lines() {
    let html = "<style>body{margin:0} #f{display:flex;flex-wrap:wrap;width:250px} .i{width:100px;height:20px}</style><div id=f><div class=i id=a></div><div class=i id=b></div><div class=i id=c></div></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let c = document_rect(&result, html, "#c");
    assert_eq!(a.x, 0.0);
    assert_eq!(b.x, 100.0);
    assert_eq!(c.x, 0.0);
    assert!(c.y >= a.y + 20.0, "c should wrap: {c:?}");
    assert_eq!(a.y, 0.0);
}

#[test]
fn flex_align_content_centers_lines() {
    let html = "<style>body{margin:0} #f{display:flex;flex-wrap:wrap;align-content:center;width:250px;height:200px} .i{width:100px;height:20px}</style><div id=f><div class=i id=a></div><div class=i id=b></div><div class=i id=c></div></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let c = document_rect(&result, html, "#c");
    assert!((a.y - 80.0).abs() < 0.5, "{a:?}");
    assert!((c.y - 100.0).abs() < 0.5, "{c:?}");
}

#[test]
fn flex_nowrap_keeps_one_line() {
    let html = "<style>body{margin:0} #f{display:flex;width:150px} .i{width:100px;height:20px}</style><div id=f><div class=i id=a></div><div class=i id=b></div></div>";
    let result = layout(html, 800.0, 600.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    assert_eq!(a.y, b.y);
    assert!(b.x >= 50.0);
}

#[test]
fn table_cells_are_laid_out_side_by_side() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} td{padding:0}</style><table><tr><td id=a>A</td><td id=b>B</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    assert_eq!(a.y, b.y);
    assert!(b.x > a.x + 50.0, "{a:?} {b:?}");
    assert!((a.width + b.width - 300.0).abs() < 0.5, "{a:?} {b:?}");
}

#[test]
fn table_rows_stack_and_cells_stretch() {
    let html = "<style>body{margin:0} table{border-spacing:0} td{padding:0}</style><table><tr><td id=a></td><td id=b>one<br>two</td></tr><tr><td id=c>c</td><td id=d>d</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let c = document_rect(&result, html, "#c");
    let d = document_rect(&result, html, "#d");
    assert!((a.height - b.height).abs() < 0.5, "{a:?} {b:?}");
    assert_eq!(c.y, d.y);
    assert!(c.y >= a.y + a.height, "{a:?} {c:?}");
    assert!(b.height > 25.0);
}

fn box_text(box_: &LayoutBox) -> String {
    let mut text = String::new();
    for line in &box_.lines {
        for fragment in &line.fragments {
            match &fragment.kind {
                nexium::engine::layout::FragmentKind::Text(value) => text.push_str(value),
                nexium::engine::layout::FragmentKind::Space => text.push(' '),
                _ => {}
            }
        }
    }
    text
}

#[test]
fn list_items_render_markers() {
    let html = "<style>body{margin:0} ul,ol{margin:0;padding:0}</style><ul><li id=a>one</li><li id=b>two</li></ul><ol><li id=c>three</li><li id=d>four</li></ol>";
    let result = layout(html, 400.0, 400.0);
    let (a, _) = box_of(&result, html, "#a");
    let (b, _) = box_of(&result, html, "#b");
    let (c, _) = box_of(&result, html, "#c");
    let (d, _) = box_of(&result, html, "#d");
    assert!(box_text(find_box(&result, a).unwrap()).starts_with('•'));
    assert!(box_text(find_box(&result, b).unwrap()).starts_with('•'));
    assert!(box_text(find_box(&result, c).unwrap()).starts_with("1."));
    assert!(box_text(find_box(&result, d).unwrap()).starts_with("2."));
}

#[test]
fn list_style_none_suppresses_markers() {
    let html = "<style>body{margin:0} ul{margin:0;padding:0;list-style-type:none}</style><ul><li id=a>one</li></ul>";
    let result = layout(html, 400.0, 200.0);
    let (a, _) = box_of(&result, html, "#a");
    assert!(!box_text(find_box(&result, a).unwrap()).starts_with('•'));
}

#[test]
fn percentage_heights_resolve_against_definite_ancestors() {
    let html = "<style>html,body{margin:0;height:100%} #a{height:50%}</style><div id=a></div>";
    let result = layout(html, 400.0, 600.0);
    let a = document_rect(&result, html, "#a");
    assert_eq!(a.height, 300.0);

    let html = "<style>body{margin:0} #a{height:50%}</style><div id=a></div>";
    let result = layout(html, 400.0, 600.0);
    let a = document_rect(&result, html, "#a");
    assert_eq!(a.height, 0.0);
}

#[test]
fn parent_child_top_margins_collapse() {
    let html = "<style>body{margin:8px} p{margin:20px 0 0 0}</style><p id=p>x</p>";
    let result = layout(html, 400.0, 300.0);
    let p = document_rect(&result, html, "#p");
    assert_eq!(p.y, 20.0);

    let html = "<style>body{margin:0} #wrap{margin-top:10px;border-top:1px solid #000} #child{margin-top:25px}</style><div id=wrap><div id=child></div></div>";
    let result = layout(html, 400.0, 300.0);
    let wrap = document_rect(&result, html, "#wrap");
    let child = document_rect(&result, html, "#child");
    assert_eq!(child.y, wrap.y + 1.0 + 25.0);
}

#[test]
fn parent_child_bottom_margins_collapse() {
    let html = "<style>body{margin:0} p{margin:0 0 40px 0;height:20px}</style><p id=p></p>";
    let result = layout(html, 400.0, 300.0);
    let p = document_rect(&result, html, "#p");
    let body_node = {
        let page = Page::from_html(url::Url::parse("https://layout.test/").unwrap(), html);
        page.select("body").unwrap().unwrap()
    };
    let body = result.node_rects.get(&body_node).copied().unwrap();
    assert_eq!(body.height, p.height);
}

#[test]
fn sibling_margins_still_collapse() {
    let html = "<style>body{margin:0} p{margin:0;height:10px} #a{margin-bottom:12px} #b{margin-top:30px}</style><p id=a></p><p id=b></p>";
    let result = layout(html, 400.0, 300.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    assert_eq!(b.y, a.y + 10.0 + 30.0);
}

#[test]
fn table_colspan_spans_columns() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} td{padding:0}</style><table><tr><td>a</td><td>b</td><td>c</td></tr><tr><td id=span colspan=3>wide</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let span = document_rect(&result, html, "#span");
    assert!((span.width - 300.0).abs() < 0.5, "{span:?}");
    assert_eq!(span.x, 0.0);
}
