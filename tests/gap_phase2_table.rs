//! Phase 2 table-layout gaps: rowspan, border-collapse and captions.

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

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.5
}

#[test]
fn rowspan_cell_covers_its_rows_without_overlap() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} td{padding:0}</style>\
<table><tr><td id=a rowspan=2>A</td><td id=b>one</td></tr>\
<tr><td id=c>two<br>three</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let c = document_rect(&result, html, "#c");

    // The spanning cell starts in row 0 and covers both rows.
    assert!(close(a.y, b.y), "{a:?} {b:?}");
    assert!(close(a.height, b.height + c.height), "{a:?} {b:?} {c:?}");
    assert!(close(a.bottom(), c.bottom()), "{a:?} {c:?}");
    // Row 1 starts below row 0; the second-row cell does not overlap it.
    assert!(c.y >= b.bottom() - 0.01, "{b:?} {c:?}");
    // The spanning cell keeps column 0, so `c` shifts into column 1.
    assert!(close(c.x, b.x), "{b:?} {c:?}");
    assert!(c.x >= a.right() - 0.01, "{a:?} {c:?}");
    // Two columns span the full table width.
    assert!(close(a.width + b.width, 300.0), "{a:?} {b:?}");
}

#[test]
fn rowspan_shifts_later_cells_around_the_span() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} td{padding:0}</style>\
<table><tr><td id=a rowspan=2>A</td><td id=b>B</td></tr>\
<tr><td id=c>C</td><td id=d>D</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let c = document_rect(&result, html, "#c");
    let d = document_rect(&result, html, "#d");

    assert!(close(a.x, 0.0), "{a:?}");
    // Three columns: `a` owns column 0, row 1's cells occupy columns 1 and 2.
    assert!(close(a.right(), b.x), "{a:?} {b:?}");
    assert!(close(c.x, b.x), "{b:?} {c:?}");
    assert!(close(d.x, c.right()), "{c:?} {d:?}");
    assert!(close(d.right(), 300.0), "{d:?}");
    assert!(close(c.width, b.width), "{b:?} {c:?}");
    assert!(c.y >= b.bottom() - 0.01, "{b:?} {c:?}");
    assert!(close(a.height, b.height + c.height), "{a:?} {b:?} {c:?}");
}

#[test]
fn border_collapse_shares_internal_borders() {
    let html = "<style>body{margin:0} table{width:300px;border-collapse:collapse;border:4px solid #000} \
td{padding:0;border:2px solid #000;height:30px}</style>\
<table><tr><td id=a>A</td><td id=b>B</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let (a_node, _) = box_of(&result, html, "#a");
    let (b_node, _) = box_of(&result, html, "#b");
    let a_box = find_box(&result, a_node).unwrap();
    let b_box = find_box(&result, b_node).unwrap();

    // Collapsed model removes border-spacing: the cells abut exactly.
    assert_eq!(a.x, 4.0);
    assert!(close(b.x, a.right()), "{a:?} {b:?}");
    assert!(close(b.right(), 304.0), "{b:?}");

    // Internal borders are halved per side; outer cell borders stay full.
    assert_eq!(a_box.border.left, 2.0);
    assert_eq!(a_box.border.right, 1.0);
    assert_eq!(b_box.border.left, 1.0);
    assert_eq!(b_box.border.right, 2.0);
    assert_eq!(a_box.border.top, 2.0);
    assert_eq!(a_box.border.bottom, 2.0);
    assert_eq!(b_box.border.top, 2.0);
    assert_eq!(b_box.border.bottom, 2.0);

    // The outer table border still participates in the box model.
    let table = document_rect(&result, html, "table");
    assert!(close(table.width, 308.0), "{table:?}");
}

#[test]
fn border_collapse_keeps_separate_behavior_unchanged() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} \
td{padding:0;border:2px solid #000;height:20px}</style>\
<table><tr><td id=a>A</td><td id=b>B</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let (a_node, _) = box_of(&result, html, "#a");
    let a_box = find_box(&result, a_node).unwrap();
    let (b_node, _) = box_of(&result, html, "#b");
    let b_box = find_box(&result, b_node).unwrap();
    // No collapsing in the separated model.
    assert_eq!(a_box.border.right, 2.0);
    assert_eq!(b_box.border.left, 2.0);
}

#[test]
fn caption_lays_out_above_table_and_is_excluded_from_grid() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} td{padding:0}</style>\
<table><caption id=cap>Title</caption><tr><td id=a></td><td id=b></td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let cap = document_rect(&result, html, "#cap");
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let table = document_rect(&result, html, "table");

    assert!(close(cap.width, 300.0), "{cap:?}");
    assert_eq!(cap.x, 0.0);
    assert!(close(cap.y, table.y), "{cap:?} {table:?}");
    assert!(cap.height > 0.0, "{cap:?}");
    assert!(a.y >= cap.bottom() - 0.01, "{cap:?} {a:?}");
    // Caption is not a cell: the row still has two 150px columns.
    assert!(close(a.width, 150.0), "{a:?}");
    assert!(close(b.width, 150.0), "{b:?}");
}

#[test]
fn caption_side_bottom_places_caption_after_rows() {
    let html = "<style>body{margin:0} table{width:300px;border-spacing:0} td{padding:0} \
caption{caption-side:bottom}</style>\
<table><caption id=cap>Title</caption><tr><td id=a>x</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let cap = document_rect(&result, html, "#cap");
    let a = document_rect(&result, html, "#a");

    assert!(cap.y >= a.bottom() - 0.01, "{a:?} {cap:?}");
    assert!(a.y < cap.y, "{a:?} {cap:?}");
    assert!(close(cap.width, 300.0), "{cap:?}");
}

#[test]
fn collapse_and_rowspan_combine_without_overlap() {
    let html = "<style>body{margin:0} table{width:300px;border-collapse:collapse} \
td{padding:0;border:1px solid #000}</style>\
<table><tr><td id=a rowspan=2>A</td><td id=b>B</td></tr>\
<tr><td id=c>C</td></tr></table>";
    let result = layout(html, 800.0, 400.0);
    let a = document_rect(&result, html, "#a");
    let b = document_rect(&result, html, "#b");
    let c = document_rect(&result, html, "#c");

    assert!(close(a.height, b.height + c.height), "{a:?} {b:?} {c:?}");
    assert!(c.y >= b.bottom() - 0.01, "{b:?} {c:?}");
    assert!(close(c.x, b.x), "{b:?} {c:?}");
}
