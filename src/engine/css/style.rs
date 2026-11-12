//! Computed styles and property/shorthand expansion.

use std::collections::HashMap;

use super::parser::ComponentValue;
use super::values::{
    AlignContent, AlignItems, AlignSelf, BackgroundRepeat, BackgroundSize, BackgroundSizeMode,
    BorderStyle, BoxSizing, Color, Display, Edges, FlexDirection, FlexWrap, FontStyle,
    GridAutoFlow, GridPlacement, GridTrack, JustifyContent, Length, LineHeight, ListStyleType,
    Overflow, PaintColor, Position, TextAlign, TextDecoration, Viewport, Visibility, WhiteSpace,
    expand_four, ident, number, parse_color, parse_font_size_keyword, parse_length, parse_lengths,
    parse_url, percentage, significant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PropertyId {
    Display,
    Position,
    Color,
    BackgroundColor,
    BackgroundImage,
    BackgroundRepeat,
    BackgroundSize,
    BackgroundPosition,
    BorderRadius,
    FontSize,
    FontWeight,
    FontStyle,
    LineHeight,
    TextAlign,
    WhiteSpace,
    TextDecoration,
    Visibility,
    Overflow,
    Opacity,
    BoxSizing,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    BorderTopWidth,
    BorderRightWidth,
    BorderBottomWidth,
    BorderLeftWidth,
    BorderTopStyle,
    BorderRightStyle,
    BorderBottomStyle,
    BorderLeftStyle,
    BorderTopColor,
    BorderRightColor,
    BorderBottomColor,
    BorderLeftColor,
    Width,
    Height,
    MinWidth,
    MaxWidth,
    MinHeight,
    MaxHeight,
    FlexDirection,
    FlexWrap,
    JustifyContent,
    AlignItems,
    AlignSelf,
    AlignContent,
    BorderSpacing,
    BorderCollapse,
    CaptionSide,
    ListStyleType,
    FlexGrow,
    FlexShrink,
    FlexBasis,
    RowGap,
    ColumnGap,
    GridTemplateColumns,
    GridTemplateRows,
    GridAutoColumns,
    GridAutoRows,
    GridAutoFlow,
    GridColumnStart,
    GridColumnEnd,
    GridRowStart,
    GridRowEnd,
    Order,
    Top,
    Right,
    Bottom,
    Left,
    ZIndex,
    FontFamily,
    Content,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Display(Display),
    Position(Position),
    Color(PaintColor),
    BackgroundColor(PaintColor),
    BackgroundImage(Option<String>),
    BackgroundRepeat(BackgroundRepeat),
    BackgroundSize(BackgroundSize),
    BackgroundPosition(Length, Length),
    BorderRadius(Length),
    FontSize(Length),
    FontWeight(u16),
    FontStyle(FontStyle),
    LineHeight(LineHeight),
    TextAlign(TextAlign),
    WhiteSpace(WhiteSpace),
    TextDecoration(TextDecoration),
    Visibility(Visibility),
    Overflow(Overflow),
    Opacity(f32),
    BoxSizing(BoxSizing),
    MarginTop(Length),
    MarginRight(Length),
    MarginBottom(Length),
    MarginLeft(Length),
    PaddingTop(Length),
    PaddingRight(Length),
    PaddingBottom(Length),
    PaddingLeft(Length),
    BorderWidth(BorderEdge, Length),
    BorderStyle(BorderEdge, BorderStyle),
    BorderColor(BorderEdge, PaintColor),
    Width(Length),
    Height(Length),
    MinWidth(Length),
    MaxWidth(Length),
    MinHeight(Length),
    MaxHeight(Length),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    JustifyContent(JustifyContent),
    AlignItems(AlignItems),
    AlignSelf(AlignSelf),
    AlignContent(AlignContent),
    BorderSpacing(Length, Length),
    BorderCollapse(BorderCollapse),
    CaptionSide(CaptionSide),
    ListStyleType(ListStyleType),
    FlexGrow(f32),
    FlexShrink(f32),
    FlexBasis(Length),
    RowGap(Length),
    ColumnGap(Length),
    GridTemplateColumns(Vec<GridTrack>),
    GridTemplateRows(Vec<GridTrack>),
    GridAutoColumns(GridTrack),
    GridAutoRows(GridTrack),
    GridAutoFlow(GridAutoFlow),
    GridColumnStart(GridPlacement),
    GridColumnEnd(GridPlacement),
    GridRowStart(GridPlacement),
    GridRowEnd(GridPlacement),
    Order(i32),
    Top(Length),
    Right(Length),
    Bottom(Length),
    Left(Length),
    ZIndex(i32),
    FontFamily(String),
    Content(ContentValue),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentPart {
    Text(String),
    Attr(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentValue {
    None,
    Parts(Vec<ContentPart>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderEdge {
    Top,
    Right,
    Bottom,
    Left,
}

/// `border-collapse` values. Inherited, initial `separate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BorderCollapse {
    #[default]
    Separate,
    Collapse,
}

/// `caption-side` values. Inherited, initial `top`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CaptionSide {
    #[default]
    Top,
    Bottom,
}

impl BorderEdge {
    fn width_id(self) -> PropertyId {
        match self {
            BorderEdge::Top => PropertyId::BorderTopWidth,
            BorderEdge::Right => PropertyId::BorderRightWidth,
            BorderEdge::Bottom => PropertyId::BorderBottomWidth,
            BorderEdge::Left => PropertyId::BorderLeftWidth,
        }
    }

    fn style_id(self) -> PropertyId {
        match self {
            BorderEdge::Top => PropertyId::BorderTopStyle,
            BorderEdge::Right => PropertyId::BorderRightStyle,
            BorderEdge::Bottom => PropertyId::BorderBottomStyle,
            BorderEdge::Left => PropertyId::BorderLeftStyle,
        }
    }

    fn color_id(self) -> PropertyId {
        match self {
            BorderEdge::Top => PropertyId::BorderTopColor,
            BorderEdge::Right => PropertyId::BorderRightColor,
            BorderEdge::Bottom => PropertyId::BorderBottomColor,
            BorderEdge::Left => PropertyId::BorderLeftColor,
        }
    }
}

const EDGES: [BorderEdge; 4] = [
    BorderEdge::Top,
    BorderEdge::Right,
    BorderEdge::Bottom,
    BorderEdge::Left,
];

#[derive(Clone, Debug)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub color: Color,
    pub background_color: Color,
    pub background_image: Option<String>,
    pub background_repeat: BackgroundRepeat,
    pub background_size: BackgroundSize,
    pub background_position: (Length, Length),
    pub border_radius: Length,
    pub font_size: f32,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub line_height: f32,
    pub line_height_factor: Option<f32>,
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    pub text_decoration: TextDecoration,
    pub visibility: Visibility,
    pub overflow: Overflow,
    pub opacity: f32,
    pub box_sizing: BoxSizing,
    pub margin: Edges<Length>,
    pub padding: Edges<Length>,
    pub border_width: Edges<f32>,
    pub border_style: Edges<BorderStyle>,
    pub border_color: Edges<Color>,
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub max_width: Length,
    pub min_height: Length,
    pub max_height: Length,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    pub align_self: AlignSelf,
    pub align_content: AlignContent,
    pub border_spacing_x: f32,
    pub border_spacing_y: f32,
    pub border_collapse: BorderCollapse,
    pub caption_side: CaptionSide,
    pub list_style_type: ListStyleType,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub row_gap: Length,
    pub column_gap: Length,
    pub grid_template_columns: Vec<GridTrack>,
    pub grid_template_rows: Vec<GridTrack>,
    pub grid_auto_columns: GridTrack,
    pub grid_auto_rows: GridTrack,
    pub grid_auto_flow: GridAutoFlow,
    pub grid_column_start: GridPlacement,
    pub grid_column_end: GridPlacement,
    pub grid_row_start: GridPlacement,
    pub grid_row_end: GridPlacement,
    pub order: i32,
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,
    pub z_index: i32,
    pub font_family: String,
    pub content: Option<String>,
    pub content_parts: Option<ContentValue>,
    pub custom_properties: HashMap<String, Vec<ComponentValue>>,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self::initial()
    }
}

impl ComputedStyle {
    pub fn initial() -> Self {
        Self {
            display: Display::Inline,
            position: Position::Static,
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            background_image: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_size: BackgroundSize::default(),
            background_position: (Length::Percent(0.0), Length::Percent(0.0)),
            border_radius: Length::Px(0.0),
            font_size: 16.0,
            font_weight: 400,
            font_style: FontStyle::Normal,
            line_height: 19.2,
            line_height_factor: Some(1.2),
            text_align: TextAlign::Left,
            white_space: WhiteSpace::Normal,
            text_decoration: TextDecoration::None,
            visibility: Visibility::Visible,
            overflow: Overflow::Visible,
            opacity: 1.0,
            box_sizing: BoxSizing::ContentBox,
            margin: Edges::<Length>::ZERO,
            padding: Edges::<Length>::ZERO,
            border_width: Edges::<f32>::ZERO,
            border_style: Edges::all(BorderStyle::None),
            border_color: Edges::all(Color::BLACK),
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Px(0.0),
            max_width: Length::Auto,
            min_height: Length::Px(0.0),
            max_height: Length::Auto,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: AlignSelf::Auto,
            align_content: AlignContent::Stretch,
            border_spacing_x: 0.0,
            border_spacing_y: 0.0,
            border_collapse: BorderCollapse::Separate,
            caption_side: CaptionSide::Top,
            list_style_type: ListStyleType::Disc,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            row_gap: Length::Px(0.0),
            column_gap: Length::Px(0.0),
            grid_template_columns: Vec::new(),
            grid_template_rows: Vec::new(),
            grid_auto_columns: GridTrack::Auto,
            grid_auto_rows: GridTrack::Auto,
            grid_auto_flow: GridAutoFlow::Row,
            grid_column_start: GridPlacement::Auto,
            grid_column_end: GridPlacement::Auto,
            grid_row_start: GridPlacement::Auto,
            grid_row_end: GridPlacement::Auto,
            order: 0,
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            left: Length::Auto,
            z_index: 0,
            font_family: "sans-serif".to_owned(),
            content: None,
            content_parts: None,
            custom_properties: HashMap::new(),
        }
    }

    pub fn inherited_from(parent: &ComputedStyle) -> Self {
        let mut style = Self::initial();
        style.color = parent.color;
        style.font_size = parent.font_size;
        style.font_weight = parent.font_weight;
        style.font_style = parent.font_style;
        style.line_height = parent.line_height;
        style.line_height_factor = parent.line_height_factor;
        style.text_align = parent.text_align;
        style.white_space = parent.white_space;
        style.text_decoration = parent.text_decoration;
        style.visibility = parent.visibility;
        style.list_style_type = parent.list_style_type;
        style.border_collapse = parent.border_collapse;
        style.caption_side = parent.caption_side;
        style.font_family = parent.font_family.clone();
        style.custom_properties = parent.custom_properties.clone();
        style
    }

    pub fn apply(
        &mut self,
        _id: PropertyId,
        value: &PropertyValue,
        parent: &ComputedStyle,
        root_font_size: f32,
        viewport: Viewport,
    ) {
        match value {
            PropertyValue::Display(display) => self.display = *display,
            PropertyValue::Position(position) => self.position = *position,
            PropertyValue::Color(color) => self.color = color.resolve(self.color),
            PropertyValue::BackgroundColor(color) => {
                self.background_color = color.resolve(self.color);
            }
            PropertyValue::BackgroundImage(url) => self.background_image = url.clone(),
            PropertyValue::BackgroundRepeat(repeat) => self.background_repeat = *repeat,
            PropertyValue::BackgroundSize(size) => self.background_size = *size,
            PropertyValue::BackgroundPosition(x, y) => self.background_position = (*x, *y),
            PropertyValue::BorderRadius(length) => self.border_radius = *length,
            PropertyValue::FontSize(length) => {
                if let Some(size) =
                    length.resolve(parent.font_size, root_font_size, viewport, parent.font_size)
                {
                    self.font_size = size.max(0.0);
                    if let Some(factor) = self.line_height_factor {
                        self.line_height = self.font_size * factor;
                    }
                }
            }
            PropertyValue::FontWeight(weight) => self.font_weight = *weight,
            PropertyValue::FontStyle(style) => self.font_style = *style,
            PropertyValue::LineHeight(line_height) => {
                self.line_height = match line_height {
                    LineHeight::Normal => {
                        self.line_height_factor = Some(1.2);
                        self.font_size * 1.2
                    }
                    LineHeight::Number(n) => {
                        self.line_height_factor = Some(n.max(0.0));
                        self.font_size * n.max(0.0)
                    }
                    LineHeight::Length(length) => {
                        self.line_height_factor = None;
                        length
                            .resolve(self.font_size, root_font_size, viewport, self.font_size)
                            .unwrap_or(self.font_size * 1.2)
                    }
                };
            }
            PropertyValue::TextAlign(align) => self.text_align = *align,
            PropertyValue::WhiteSpace(space) => self.white_space = *space,
            PropertyValue::TextDecoration(decoration) => self.text_decoration = *decoration,
            PropertyValue::Visibility(visibility) => self.visibility = *visibility,
            PropertyValue::Overflow(overflow) => self.overflow = *overflow,
            PropertyValue::Opacity(opacity) => self.opacity = opacity.clamp(0.0, 1.0),
            PropertyValue::BoxSizing(sizing) => self.box_sizing = *sizing,
            PropertyValue::MarginTop(length) => self.margin.top = *length,
            PropertyValue::MarginRight(length) => self.margin.right = *length,
            PropertyValue::MarginBottom(length) => self.margin.bottom = *length,
            PropertyValue::MarginLeft(length) => self.margin.left = *length,
            PropertyValue::PaddingTop(length) => self.padding.top = *length,
            PropertyValue::PaddingRight(length) => self.padding.right = *length,
            PropertyValue::PaddingBottom(length) => self.padding.bottom = *length,
            PropertyValue::PaddingLeft(length) => self.padding.left = *length,
            PropertyValue::BorderWidth(edge, length) => {
                let px = length
                    .resolve(self.font_size, root_font_size, viewport, 0.0)
                    .unwrap_or(0.0)
                    .max(0.0);
                match edge {
                    BorderEdge::Top => self.border_width.top = px,
                    BorderEdge::Right => self.border_width.right = px,
                    BorderEdge::Bottom => self.border_width.bottom = px,
                    BorderEdge::Left => self.border_width.left = px,
                }
            }
            PropertyValue::BorderStyle(edge, style) => match edge {
                BorderEdge::Top => self.border_style.top = *style,
                BorderEdge::Right => self.border_style.right = *style,
                BorderEdge::Bottom => self.border_style.bottom = *style,
                BorderEdge::Left => self.border_style.left = *style,
            },
            PropertyValue::BorderColor(edge, color) => {
                let color = color.resolve(self.color);
                match edge {
                    BorderEdge::Top => self.border_color.top = color,
                    BorderEdge::Right => self.border_color.right = color,
                    BorderEdge::Bottom => self.border_color.bottom = color,
                    BorderEdge::Left => self.border_color.left = color,
                }
            }
            PropertyValue::Width(length) => self.width = *length,
            PropertyValue::Height(length) => self.height = *length,
            PropertyValue::MinWidth(length) => self.min_width = *length,
            PropertyValue::MaxWidth(length) => self.max_width = *length,
            PropertyValue::MinHeight(length) => self.min_height = *length,
            PropertyValue::MaxHeight(length) => self.max_height = *length,
            PropertyValue::FlexDirection(direction) => self.flex_direction = *direction,
            PropertyValue::FlexWrap(wrap) => self.flex_wrap = *wrap,
            PropertyValue::JustifyContent(justify) => self.justify_content = *justify,
            PropertyValue::AlignItems(align) => self.align_items = *align,
            PropertyValue::AlignSelf(align) => self.align_self = *align,
            PropertyValue::AlignContent(align) => self.align_content = *align,
            PropertyValue::BorderSpacing(horizontal, vertical) => {
                self.border_spacing_x = horizontal
                    .resolve(self.font_size, root_font_size, viewport, 0.0)
                    .unwrap_or(0.0)
                    .max(0.0);
                self.border_spacing_y = vertical
                    .resolve(self.font_size, root_font_size, viewport, 0.0)
                    .unwrap_or(0.0)
                    .max(0.0);
            }
            PropertyValue::ListStyleType(kind) => self.list_style_type = *kind,
            PropertyValue::BorderCollapse(collapse) => self.border_collapse = *collapse,
            PropertyValue::CaptionSide(side) => self.caption_side = *side,
            PropertyValue::FlexGrow(grow) => self.flex_grow = grow.max(0.0),
            PropertyValue::FlexShrink(shrink) => self.flex_shrink = shrink.max(0.0),
            PropertyValue::FlexBasis(basis) => self.flex_basis = *basis,
            PropertyValue::RowGap(gap) => self.row_gap = *gap,
            PropertyValue::ColumnGap(gap) => self.column_gap = *gap,
            PropertyValue::GridTemplateColumns(tracks) => {
                self.grid_template_columns = tracks.clone()
            }
            PropertyValue::GridTemplateRows(tracks) => self.grid_template_rows = tracks.clone(),
            PropertyValue::GridAutoColumns(track) => self.grid_auto_columns = track.clone(),
            PropertyValue::GridAutoRows(track) => self.grid_auto_rows = track.clone(),
            PropertyValue::GridAutoFlow(flow) => self.grid_auto_flow = *flow,
            PropertyValue::GridColumnStart(placement) => self.grid_column_start = *placement,
            PropertyValue::GridColumnEnd(placement) => self.grid_column_end = *placement,
            PropertyValue::GridRowStart(placement) => self.grid_row_start = *placement,
            PropertyValue::GridRowEnd(placement) => self.grid_row_end = *placement,
            PropertyValue::Order(order) => self.order = *order,
            PropertyValue::Top(length) => self.top = *length,
            PropertyValue::Right(length) => self.right = *length,
            PropertyValue::Bottom(length) => self.bottom = *length,
            PropertyValue::Left(length) => self.left = *length,
            PropertyValue::ZIndex(z_index) => self.z_index = *z_index,
            PropertyValue::FontFamily(family) => self.font_family = family.clone(),
            PropertyValue::Content(content) => self.content_parts = Some(content.clone()),
        }
    }
}

/// Expand a (var-substituted) declaration into longhand properties.
pub fn expand_declaration(
    name: &str,
    values: &[ComponentValue],
) -> Vec<(PropertyId, PropertyValue)> {
    let mut out = Vec::new();
    let sig = significant(values);
    match name {
        "display" => {
            if let Some(display) = sig.iter().find_map(|v| parse_display(v)) {
                out.push((PropertyId::Display, PropertyValue::Display(display)));
            }
        }
        "position" => {
            if let Some(position) = sig.iter().find_map(|v| parse_position(v)) {
                out.push((PropertyId::Position, PropertyValue::Position(position)));
            }
        }
        "color" => {
            if let Some(color) = sig.iter().find_map(|v| parse_color(v)) {
                out.push((PropertyId::Color, PropertyValue::Color(color)));
            }
        }
        "background-color" => {
            if let Some(color) = sig.iter().find_map(|v| parse_color(v)) {
                out.push((
                    PropertyId::BackgroundColor,
                    PropertyValue::BackgroundColor(color),
                ));
            }
        }
        "background-image" => {
            let image = if sig.iter().any(|v| ident(v).as_deref() == Some("none")) {
                Some(None)
            } else {
                sig.iter().find_map(|v| parse_url(v)).map(Some)
            };
            if let Some(image) = image {
                out.push((
                    PropertyId::BackgroundImage,
                    PropertyValue::BackgroundImage(image),
                ));
            }
        }
        "background" => {
            out.push((
                PropertyId::BackgroundImage,
                PropertyValue::BackgroundImage(None),
            ));
            out.push((
                PropertyId::BackgroundColor,
                PropertyValue::BackgroundColor(PaintColor::Rgba(Color::TRANSPARENT)),
            ));
            let mut position_values: Vec<ComponentValue> = Vec::new();
            let mut size_values: Vec<ComponentValue> = Vec::new();
            let mut after_slash = false;
            for value in &sig {
                if matches!(value, ComponentValue::Delim('/')) {
                    after_slash = true;
                    continue;
                }
                if let Some(url) = parse_url(value) {
                    out.push((
                        PropertyId::BackgroundImage,
                        PropertyValue::BackgroundImage(Some(url)),
                    ));
                } else if let Some(repeat) = parse_background_repeat_value(value) {
                    out.push((
                        PropertyId::BackgroundRepeat,
                        PropertyValue::BackgroundRepeat(repeat),
                    ));
                } else if let Some(mode) = parse_background_size_mode(value) {
                    out.push((
                        PropertyId::BackgroundSize,
                        PropertyValue::BackgroundSize(BackgroundSize {
                            mode,
                            width: Length::Auto,
                            height: Length::Auto,
                        }),
                    ));
                } else if after_slash {
                    size_values.push((**value).clone());
                } else if is_background_position_value(value) {
                    position_values.push((**value).clone());
                } else if let Some(color) = parse_color(value) {
                    out.push((
                        PropertyId::BackgroundColor,
                        PropertyValue::BackgroundColor(color),
                    ));
                }
            }
            if let Some(position) = parse_background_position(&position_values) {
                out.push((
                    PropertyId::BackgroundPosition,
                    PropertyValue::BackgroundPosition(position.0, position.1),
                ));
            }
            if let Some(size) = parse_background_size(&size_values) {
                out.push((
                    PropertyId::BackgroundSize,
                    PropertyValue::BackgroundSize(size),
                ));
            }
        }
        "background-repeat" => {
            if let Some(repeat) = sig
                .iter()
                .find_map(|value| parse_background_repeat_value(value))
            {
                out.push((
                    PropertyId::BackgroundRepeat,
                    PropertyValue::BackgroundRepeat(repeat),
                ));
            }
        }
        "background-size" => {
            if let Some(size) = parse_background_size(
                &sig.iter()
                    .map(|value| (**value).clone())
                    .collect::<Vec<_>>(),
            ) {
                out.push((
                    PropertyId::BackgroundSize,
                    PropertyValue::BackgroundSize(size),
                ));
            }
        }
        "background-position" => {
            if let Some(position) = parse_background_position(
                &sig.iter()
                    .map(|value| (**value).clone())
                    .collect::<Vec<_>>(),
            ) {
                out.push((
                    PropertyId::BackgroundPosition,
                    PropertyValue::BackgroundPosition(position.0, position.1),
                ));
            }
        }
        "border-radius" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((
                    PropertyId::BorderRadius,
                    PropertyValue::BorderRadius(length),
                ));
            }
        }
        "font-size" => {
            if let Some(size) = sig.iter().find_map(|v| {
                if let Some(keyword) = ident(v)
                    && let Some(size) = parse_font_size_keyword(&keyword)
                {
                    return Some(size);
                }
                parse_length(v)
            }) {
                out.push((PropertyId::FontSize, PropertyValue::FontSize(size)));
            }
        }
        "font-weight" => {
            if let Some(weight) = sig.iter().copied().find_map(parse_font_weight) {
                out.push((PropertyId::FontWeight, PropertyValue::FontWeight(weight)));
            }
        }
        "font-style" => {
            if let Some(style) = sig.iter().copied().find_map(parse_font_style) {
                out.push((PropertyId::FontStyle, PropertyValue::FontStyle(style)));
            }
        }
        "line-height" => {
            if let Some(line_height) = sig.iter().copied().find_map(parse_line_height) {
                out.push((
                    PropertyId::LineHeight,
                    PropertyValue::LineHeight(line_height),
                ));
            }
        }
        "text-align" => {
            if let Some(align) = sig.iter().copied().find_map(parse_text_align) {
                out.push((PropertyId::TextAlign, PropertyValue::TextAlign(align)));
            }
        }
        "white-space" => {
            if let Some(space) = sig.iter().copied().find_map(parse_white_space) {
                out.push((PropertyId::WhiteSpace, PropertyValue::WhiteSpace(space)));
            }
        }
        "text-decoration" | "text-decoration-line" => {
            let mut decoration = TextDecoration::None;
            for value in &sig {
                match ident(value).as_deref() {
                    Some("underline") => decoration = TextDecoration::Underline,
                    Some("line-through") => decoration = TextDecoration::LineThrough,
                    Some("none") => decoration = TextDecoration::None,
                    _ => {}
                }
            }
            out.push((
                PropertyId::TextDecoration,
                PropertyValue::TextDecoration(decoration),
            ));
        }
        "visibility" => {
            if let Some(visibility) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "visible" => Some(Visibility::Visible),
                "hidden" | "collapse" => Some(Visibility::Hidden),
                _ => None,
            }) {
                out.push((
                    PropertyId::Visibility,
                    PropertyValue::Visibility(visibility),
                ));
            }
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            if let Some(overflow) = sig.iter().copied().find_map(parse_overflow) {
                out.push((PropertyId::Overflow, PropertyValue::Overflow(overflow)));
            }
        }
        "opacity" => {
            if let Some(value) = sig
                .iter()
                .find_map(|v| number(v).or_else(|| percentage(v).map(|p| p / 100.0)))
            {
                out.push((PropertyId::Opacity, PropertyValue::Opacity(value)));
            }
        }
        "box-sizing" => {
            if let Some(sizing) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "border-box" => Some(BoxSizing::BorderBox),
                "content-box" => Some(BoxSizing::ContentBox),
                _ => None,
            }) {
                out.push((PropertyId::BoxSizing, PropertyValue::BoxSizing(sizing)));
            }
        }
        "width" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((PropertyId::Width, PropertyValue::Width(length)));
            }
        }
        "height" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((PropertyId::Height, PropertyValue::Height(length)));
            }
        }
        "min-width" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((PropertyId::MinWidth, PropertyValue::MinWidth(length)));
            }
        }
        "max-width" => {
            let length = sig.iter().find_map(|v| {
                if ident(v).as_deref() == Some("none") {
                    Some(Length::Auto)
                } else {
                    parse_length(v)
                }
            });
            if let Some(length) = length {
                out.push((PropertyId::MaxWidth, PropertyValue::MaxWidth(length)));
            }
        }
        "min-height" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((PropertyId::MinHeight, PropertyValue::MinHeight(length)));
            }
        }
        "max-height" => {
            let length = sig.iter().find_map(|v| {
                if ident(v).as_deref() == Some("none") {
                    Some(Length::Auto)
                } else {
                    parse_length(v)
                }
            });
            if let Some(length) = length {
                out.push((PropertyId::MaxHeight, PropertyValue::MaxHeight(length)));
            }
        }
        "margin" | "padding" => {
            if let Some(lengths) = parse_lengths(values)
                && let Some(four) = expand_four(&lengths)
            {
                let padding = name == "padding";
                let ids = if padding {
                    [
                        PropertyId::PaddingTop,
                        PropertyId::PaddingRight,
                        PropertyId::PaddingBottom,
                        PropertyId::PaddingLeft,
                    ]
                } else {
                    [
                        PropertyId::MarginTop,
                        PropertyId::MarginRight,
                        PropertyId::MarginBottom,
                        PropertyId::MarginLeft,
                    ]
                };
                for (index, length) in four.into_iter().enumerate() {
                    if padding && length.is_auto() {
                        continue;
                    }
                    let value = match index {
                        0 if padding => PropertyValue::PaddingTop(length),
                        1 if padding => PropertyValue::PaddingRight(length),
                        2 if padding => PropertyValue::PaddingBottom(length),
                        3 if padding => PropertyValue::PaddingLeft(length),
                        0 => PropertyValue::MarginTop(length),
                        1 => PropertyValue::MarginRight(length),
                        2 => PropertyValue::MarginBottom(length),
                        _ => PropertyValue::MarginLeft(length),
                    };
                    out.push((ids[index], value));
                }
            }
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                let (id, value) = match name {
                    "margin-top" => (PropertyId::MarginTop, PropertyValue::MarginTop(length)),
                    "margin-right" => (PropertyId::MarginRight, PropertyValue::MarginRight(length)),
                    "margin-bottom" => (
                        PropertyId::MarginBottom,
                        PropertyValue::MarginBottom(length),
                    ),
                    _ => (PropertyId::MarginLeft, PropertyValue::MarginLeft(length)),
                };
                out.push((id, value));
            }
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length)
                && !length.is_auto()
            {
                let (id, value) = match name {
                    "padding-top" => (PropertyId::PaddingTop, PropertyValue::PaddingTop(length)),
                    "padding-right" => (
                        PropertyId::PaddingRight,
                        PropertyValue::PaddingRight(length),
                    ),
                    "padding-bottom" => (
                        PropertyId::PaddingBottom,
                        PropertyValue::PaddingBottom(length),
                    ),
                    _ => (PropertyId::PaddingLeft, PropertyValue::PaddingLeft(length)),
                };
                out.push((id, value));
            }
        }
        "border-width" => {
            if let Some(four) = parse_border_widths(&sig) {
                for (index, length) in four.into_iter().enumerate() {
                    out.push((
                        EDGES[index].width_id(),
                        PropertyValue::BorderWidth(EDGES[index], length),
                    ));
                }
            }
        }
        "border-style" => {
            if let Some(four) = parse_border_styles(&sig) {
                for (index, style) in four.into_iter().enumerate() {
                    out.push((
                        EDGES[index].style_id(),
                        PropertyValue::BorderStyle(EDGES[index], style),
                    ));
                }
            }
        }
        "border-color" => {
            if let Some(four) = parse_border_colors(&sig) {
                for (index, color) in four.into_iter().enumerate() {
                    out.push((
                        EDGES[index].color_id(),
                        PropertyValue::BorderColor(EDGES[index], color),
                    ));
                }
            }
        }
        "border" => {
            if let Some((width, style, color)) = parse_border_shorthand(&sig) {
                for edge in EDGES {
                    out.push((edge.width_id(), PropertyValue::BorderWidth(edge, width)));
                    out.push((edge.style_id(), PropertyValue::BorderStyle(edge, style)));
                    out.push((edge.color_id(), PropertyValue::BorderColor(edge, color)));
                }
            }
        }
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            if let Some((width, style, color)) = parse_border_shorthand(&sig) {
                let edge = match name {
                    "border-top" => BorderEdge::Top,
                    "border-right" => BorderEdge::Right,
                    "border-bottom" => BorderEdge::Bottom,
                    _ => BorderEdge::Left,
                };
                out.push((edge.width_id(), PropertyValue::BorderWidth(edge, width)));
                out.push((edge.style_id(), PropertyValue::BorderStyle(edge, style)));
                out.push((edge.color_id(), PropertyValue::BorderColor(edge, color)));
            }
        }
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            if let Some(length) = sig.iter().copied().find_map(parse_border_width) {
                let edge = edge_from_name(name);
                out.push((edge.width_id(), PropertyValue::BorderWidth(edge, length)));
            }
        }
        "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            if let Some(style) = sig.iter().copied().find_map(parse_border_style) {
                let edge = edge_from_name(name);
                out.push((edge.style_id(), PropertyValue::BorderStyle(edge, style)));
            }
        }
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            if let Some(color) = sig.iter().find_map(|v| parse_color(v)) {
                let edge = edge_from_name(name);
                out.push((edge.color_id(), PropertyValue::BorderColor(edge, color)));
            }
        }
        "flex-direction" => {
            if let Some(direction) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "row" => Some(FlexDirection::Row),
                "row-reverse" => Some(FlexDirection::RowReverse),
                "column" => Some(FlexDirection::Column),
                "column-reverse" => Some(FlexDirection::ColumnReverse),
                _ => None,
            }) {
                out.push((
                    PropertyId::FlexDirection,
                    PropertyValue::FlexDirection(direction),
                ));
            }
        }
        "flex-wrap" => {
            if let Some(wrap) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "nowrap" => Some(FlexWrap::NoWrap),
                "wrap" | "wrap-reverse" => Some(FlexWrap::Wrap),
                _ => None,
            }) {
                out.push((PropertyId::FlexWrap, PropertyValue::FlexWrap(wrap)));
            }
        }
        "flex-flow" => {
            for value in &sig {
                match ident(value).as_deref() {
                    Some("row") => out.push((
                        PropertyId::FlexDirection,
                        PropertyValue::FlexDirection(FlexDirection::Row),
                    )),
                    Some("row-reverse") => out.push((
                        PropertyId::FlexDirection,
                        PropertyValue::FlexDirection(FlexDirection::RowReverse),
                    )),
                    Some("column") => out.push((
                        PropertyId::FlexDirection,
                        PropertyValue::FlexDirection(FlexDirection::Column),
                    )),
                    Some("column-reverse") => out.push((
                        PropertyId::FlexDirection,
                        PropertyValue::FlexDirection(FlexDirection::ColumnReverse),
                    )),
                    Some("nowrap") => out.push((
                        PropertyId::FlexWrap,
                        PropertyValue::FlexWrap(FlexWrap::NoWrap),
                    )),
                    Some("wrap") => out.push((
                        PropertyId::FlexWrap,
                        PropertyValue::FlexWrap(FlexWrap::Wrap),
                    )),
                    _ => {}
                }
            }
        }
        "justify-content" => {
            if let Some(justify) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "flex-start" | "start" | "left" => Some(JustifyContent::FlexStart),
                "flex-end" | "end" | "right" => Some(JustifyContent::FlexEnd),
                "center" => Some(JustifyContent::Center),
                "space-between" => Some(JustifyContent::SpaceBetween),
                "space-around" => Some(JustifyContent::SpaceAround),
                "space-evenly" => Some(JustifyContent::SpaceEvenly),
                _ => None,
            }) {
                out.push((
                    PropertyId::JustifyContent,
                    PropertyValue::JustifyContent(justify),
                ));
            }
        }
        "align-items" => {
            if let Some(align) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "stretch" | "normal" => Some(AlignItems::Stretch),
                "flex-start" | "start" => Some(AlignItems::FlexStart),
                "flex-end" | "end" => Some(AlignItems::FlexEnd),
                "center" => Some(AlignItems::Center),
                "baseline" => Some(AlignItems::Baseline),
                _ => None,
            }) {
                out.push((PropertyId::AlignItems, PropertyValue::AlignItems(align)));
            }
        }
        "align-self" => {
            if let Some(align) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "auto" => Some(AlignSelf::Auto),
                "stretch" | "normal" => Some(AlignSelf::Stretch),
                "flex-start" | "start" => Some(AlignSelf::FlexStart),
                "flex-end" | "end" => Some(AlignSelf::FlexEnd),
                "center" => Some(AlignSelf::Center),
                "baseline" => Some(AlignSelf::Baseline),
                _ => None,
            }) {
                out.push((PropertyId::AlignSelf, PropertyValue::AlignSelf(align)));
            }
        }
        "list-style-type" => {
            if let Some(kind) = sig.iter().find_map(|value| match ident(value)?.as_str() {
                "disc" => Some(ListStyleType::Disc),
                "circle" => Some(ListStyleType::Circle),
                "square" => Some(ListStyleType::Square),
                "decimal" => Some(ListStyleType::Decimal),
                "none" => Some(ListStyleType::None),
                _ => None,
            }) {
                out.push((
                    PropertyId::ListStyleType,
                    PropertyValue::ListStyleType(kind),
                ));
            }
        }
        "list-style" => {
            for value in &sig {
                if let Some(kind) = match ident(value).as_deref() {
                    Some("disc") => Some(ListStyleType::Disc),
                    Some("circle") => Some(ListStyleType::Circle),
                    Some("square") => Some(ListStyleType::Square),
                    Some("decimal") => Some(ListStyleType::Decimal),
                    Some("none") => Some(ListStyleType::None),
                    _ => None,
                } {
                    out.push((
                        PropertyId::ListStyleType,
                        PropertyValue::ListStyleType(kind),
                    ));
                }
            }
        }
        "border-spacing" => {
            let lengths: Vec<Length> = sig
                .iter()
                .filter(|value| !matches!(value, ComponentValue::Comma))
                .filter_map(|value| parse_length(value))
                .collect();
            match lengths.as_slice() {
                [both] => out.push((
                    PropertyId::BorderSpacing,
                    PropertyValue::BorderSpacing(*both, *both),
                )),
                [horizontal, vertical] => out.push((
                    PropertyId::BorderSpacing,
                    PropertyValue::BorderSpacing(*horizontal, *vertical),
                )),
                _ => {}
            }
        }
        "border-collapse" => {
            if let Some(collapse) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "collapse" => Some(BorderCollapse::Collapse),
                "separate" => Some(BorderCollapse::Separate),
                _ => None,
            }) {
                out.push((
                    PropertyId::BorderCollapse,
                    PropertyValue::BorderCollapse(collapse),
                ));
            }
        }
        "caption-side" => {
            if let Some(side) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "top" => Some(CaptionSide::Top),
                "bottom" => Some(CaptionSide::Bottom),
                _ => None,
            }) {
                out.push((PropertyId::CaptionSide, PropertyValue::CaptionSide(side)));
            }
        }
        "align-content" => {
            if let Some(align) = sig.iter().find_map(|v| match ident(v)?.as_str() {
                "flex-start" | "start" => Some(AlignContent::FlexStart),
                "flex-end" | "end" => Some(AlignContent::FlexEnd),
                "center" => Some(AlignContent::Center),
                "space-between" => Some(AlignContent::SpaceBetween),
                "space-around" => Some(AlignContent::SpaceAround),
                "stretch" | "normal" => Some(AlignContent::Stretch),
                _ => None,
            }) {
                out.push((PropertyId::AlignContent, PropertyValue::AlignContent(align)));
            }
        }
        "flex-grow" => {
            if let Some(grow) = sig.iter().copied().find_map(number) {
                out.push((PropertyId::FlexGrow, PropertyValue::FlexGrow(grow)));
            }
        }
        "flex-shrink" => {
            if let Some(shrink) = sig.iter().copied().find_map(number) {
                out.push((PropertyId::FlexShrink, PropertyValue::FlexShrink(shrink)));
            }
        }
        "flex-basis" => {
            if let Some(basis) = sig.iter().copied().find_map(parse_flex_basis) {
                out.push((PropertyId::FlexBasis, PropertyValue::FlexBasis(basis)));
            }
        }
        "flex" => {
            if let Some((grow, shrink, basis)) = parse_flex_shorthand(&sig) {
                out.push((PropertyId::FlexGrow, PropertyValue::FlexGrow(grow)));
                out.push((PropertyId::FlexShrink, PropertyValue::FlexShrink(shrink)));
                out.push((PropertyId::FlexBasis, PropertyValue::FlexBasis(basis)));
            }
        }
        "gap" | "grid-gap" => {
            if let Some(lengths) = parse_lengths(values) {
                let row = lengths[0];
                let column = *lengths.get(1).unwrap_or(&row);
                out.push((PropertyId::RowGap, PropertyValue::RowGap(row)));
                out.push((PropertyId::ColumnGap, PropertyValue::ColumnGap(column)));
            }
        }
        "row-gap" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((PropertyId::RowGap, PropertyValue::RowGap(length)));
            }
        }
        "column-gap" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                out.push((PropertyId::ColumnGap, PropertyValue::ColumnGap(length)));
            }
        }
        "grid-template-columns" => {
            if let Some(tracks) = parse_grid_tracks(&sig) {
                out.push((
                    PropertyId::GridTemplateColumns,
                    PropertyValue::GridTemplateColumns(tracks),
                ));
            }
        }
        "grid-template-rows" => {
            if let Some(tracks) = parse_grid_tracks(&sig) {
                out.push((
                    PropertyId::GridTemplateRows,
                    PropertyValue::GridTemplateRows(tracks),
                ));
            }
        }
        "grid-auto-columns" => {
            if let Some(track) = sig.iter().find_map(|value| parse_grid_track(value)) {
                out.push((
                    PropertyId::GridAutoColumns,
                    PropertyValue::GridAutoColumns(track),
                ));
            }
        }
        "grid-auto-rows" => {
            if let Some(track) = sig.iter().find_map(|value| parse_grid_track(value)) {
                out.push((PropertyId::GridAutoRows, PropertyValue::GridAutoRows(track)));
            }
        }
        "grid-auto-flow" => {
            let column = sig
                .iter()
                .any(|value| ident(value).as_deref() == Some("column"));
            let dense = sig
                .iter()
                .any(|value| ident(value).as_deref() == Some("dense"));
            let flow = match (column, dense) {
                (false, false) => GridAutoFlow::Row,
                (true, false) => GridAutoFlow::Column,
                (false, true) => GridAutoFlow::RowDense,
                (true, true) => GridAutoFlow::ColumnDense,
            };
            if sig
                .iter()
                .any(|value| matches!(ident(value).as_deref(), Some("row" | "column")))
            {
                out.push((PropertyId::GridAutoFlow, PropertyValue::GridAutoFlow(flow)));
            }
        }
        "grid-column-start" | "grid-column-end" | "grid-row-start" | "grid-row-end" => {
            if let Some(placement) = sig.iter().find_map(|value| parse_grid_placement(value)) {
                let (id, value) = match name {
                    "grid-column-start" => (
                        PropertyId::GridColumnStart,
                        PropertyValue::GridColumnStart(placement),
                    ),
                    "grid-column-end" => (
                        PropertyId::GridColumnEnd,
                        PropertyValue::GridColumnEnd(placement),
                    ),
                    "grid-row-start" => (
                        PropertyId::GridRowStart,
                        PropertyValue::GridRowStart(placement),
                    ),
                    _ => (PropertyId::GridRowEnd, PropertyValue::GridRowEnd(placement)),
                };
                out.push((id, value));
            }
        }
        "grid-column" | "grid-row" => {
            let mut parts: Vec<Vec<ComponentValue>> = vec![Vec::new()];
            for value in &sig {
                if matches!(value, ComponentValue::Delim('/')) {
                    parts.push(Vec::new());
                } else if let Some(part) = parts.last_mut() {
                    part.push((**value).clone());
                }
            }
            let start = parts
                .first()
                .and_then(|part| part.iter().find_map(parse_grid_placement));
            let end = parts
                .get(1)
                .and_then(|part| part.iter().find_map(parse_grid_placement));
            if name == "grid-column" {
                if let Some(start) = start {
                    out.push((
                        PropertyId::GridColumnStart,
                        PropertyValue::GridColumnStart(start),
                    ));
                }
                if let Some(end) = end {
                    out.push((PropertyId::GridColumnEnd, PropertyValue::GridColumnEnd(end)));
                }
            } else {
                if let Some(start) = start {
                    out.push((PropertyId::GridRowStart, PropertyValue::GridRowStart(start)));
                }
                if let Some(end) = end {
                    out.push((PropertyId::GridRowEnd, PropertyValue::GridRowEnd(end)));
                }
            }
        }
        "order" => {
            if let Some(value) = sig.iter().find_map(|v| match v {
                ComponentValue::Number(n) => Some(*n as i32),
                _ => None,
            }) {
                out.push((PropertyId::Order, PropertyValue::Order(value)));
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let Some(length) = sig.iter().copied().find_map(parse_length) {
                let (id, value) = match name {
                    "top" => (PropertyId::Top, PropertyValue::Top(length)),
                    "right" => (PropertyId::Right, PropertyValue::Right(length)),
                    "bottom" => (PropertyId::Bottom, PropertyValue::Bottom(length)),
                    _ => (PropertyId::Left, PropertyValue::Left(length)),
                };
                out.push((id, value));
            }
        }
        "inset" => {
            if let Some(lengths) = parse_lengths(values)
                && let Some(four) = expand_four(&lengths)
            {
                out.push((PropertyId::Top, PropertyValue::Top(four[0])));
                out.push((PropertyId::Right, PropertyValue::Right(four[1])));
                out.push((PropertyId::Bottom, PropertyValue::Bottom(four[2])));
                out.push((PropertyId::Left, PropertyValue::Left(four[3])));
            }
        }
        "z-index" => {
            if let Some(value) = sig.iter().find_map(|v| match v {
                ComponentValue::Number(n) => Some(*n as i32),
                _ => None,
            }) {
                out.push((PropertyId::ZIndex, PropertyValue::ZIndex(value)));
            }
        }
        "content" => {
            let mut parts = Vec::new();
            let mut none = false;
            for value in &sig {
                match value {
                    ComponentValue::StringLiteral(text) => {
                        parts.push(ContentPart::Text(text.clone()))
                    }
                    ComponentValue::Ident(name)
                        if name.eq_ignore_ascii_case("none")
                            || name.eq_ignore_ascii_case("normal") =>
                    {
                        none = true;
                    }
                    ComponentValue::Function { name, args }
                        if name.eq_ignore_ascii_case("attr") =>
                    {
                        if let Some(attribute) = args.iter().find_map(|arg| match arg {
                            ComponentValue::Ident(name) => Some(name.clone()),
                            _ => None,
                        }) {
                            parts.push(ContentPart::Attr(attribute));
                        }
                    }
                    _ => {}
                }
            }
            let content = if none || parts.is_empty() {
                ContentValue::None
            } else {
                ContentValue::Parts(parts)
            };
            out.push((PropertyId::Content, PropertyValue::Content(content)));
        }
        "font-family" => {
            let family = join_family(&sig);
            if !family.is_empty() {
                out.push((PropertyId::FontFamily, PropertyValue::FontFamily(family)));
            }
        }
        "font" => {
            expand_font_shorthand(&sig, &mut out);
        }
        _ => {}
    }
    out
}

fn parse_background_repeat_value(value: &ComponentValue) -> Option<BackgroundRepeat> {
    Some(match ident(value)?.as_str() {
        "repeat" | "space" | "round" => BackgroundRepeat::Repeat,
        "repeat-x" => BackgroundRepeat::RepeatX,
        "repeat-y" => BackgroundRepeat::RepeatY,
        "no-repeat" => BackgroundRepeat::NoRepeat,
        _ => return None,
    })
}

fn parse_background_size_mode(value: &ComponentValue) -> Option<BackgroundSizeMode> {
    Some(match ident(value)?.as_str() {
        "cover" => BackgroundSizeMode::Cover,
        "contain" => BackgroundSizeMode::Contain,
        "auto" => BackgroundSizeMode::Auto,
        _ => return None,
    })
}

fn is_background_position_value(value: &ComponentValue) -> bool {
    if let Some(name) = ident(value) {
        return matches!(
            name.as_str(),
            "left" | "right" | "top" | "bottom" | "center"
        );
    }
    matches!(
        value,
        ComponentValue::Percentage(_) | ComponentValue::Dimension { .. }
    ) || matches!(value, ComponentValue::Number(number) if *number == 0.0)
}

fn position_keyword(name: &str) -> Option<Length> {
    Some(match name {
        "left" | "top" => Length::Percent(0.0),
        "center" => Length::Percent(50.0),
        "right" | "bottom" => Length::Percent(100.0),
        _ => return None,
    })
}

fn parse_background_position(values: &[ComponentValue]) -> Option<(Length, Length)> {
    let values: Vec<&ComponentValue> = values
        .iter()
        .filter(|value| !matches!(value, ComponentValue::Whitespace | ComponentValue::Comma))
        .collect();
    if values.is_empty() {
        return None;
    }
    let mut resolved: Vec<Option<Length>> = Vec::new();
    for value in &values {
        if let Some(length) = parse_length(value) {
            resolved.push(Some(length));
        } else if let Some(keyword) = ident(value).and_then(|name| position_keyword(&name)) {
            resolved.push(Some(keyword));
        } else {
            resolved.push(None);
        }
    }
    match resolved.as_slice() {
        [Some(horizontal)] => Some((*horizontal, Length::Percent(50.0))),
        [Some(first), Some(second)] => {
            let first_vertical = values
                .first()
                .and_then(|value| ident(value))
                .is_some_and(|name| matches!(name.as_str(), "top" | "bottom"));
            if first_vertical {
                Some((*second, *first))
            } else {
                Some((*first, *second))
            }
        }
        _ => None,
    }
}

fn parse_background_size(values: &[ComponentValue]) -> Option<BackgroundSize> {
    let values: Vec<&ComponentValue> = values
        .iter()
        .filter(|value| !matches!(value, ComponentValue::Whitespace | ComponentValue::Comma))
        .collect();
    if values.is_empty() {
        return None;
    }
    if values.len() == 1
        && let Some(mode) = parse_background_size_mode(values[0])
    {
        return Some(BackgroundSize {
            mode,
            width: Length::Auto,
            height: Length::Auto,
        });
    }
    let mut lengths = Vec::new();
    for value in &values {
        if ident(value).as_deref() == Some("auto") {
            lengths.push(Length::Auto);
        } else {
            lengths.push(parse_length(value)?);
        }
    }
    match lengths.as_slice() {
        [both] => Some(BackgroundSize {
            mode: BackgroundSizeMode::Explicit,
            width: *both,
            height: Length::Auto,
        }),
        [width, height] => Some(BackgroundSize {
            mode: BackgroundSizeMode::Explicit,
            width: *width,
            height: *height,
        }),
        _ => None,
    }
}

fn parse_grid_track(value: &ComponentValue) -> Option<GridTrack> {
    match value {
        ComponentValue::Dimension { value, unit } if unit.eq_ignore_ascii_case("fr") => {
            Some(GridTrack::Fr((*value as f32).max(0.0)))
        }
        ComponentValue::Ident(name) if name.eq_ignore_ascii_case("auto") => Some(GridTrack::Auto),
        ComponentValue::Ident(name) if name.eq_ignore_ascii_case("min-content") => {
            Some(GridTrack::MinContent)
        }
        ComponentValue::Ident(name) if name.eq_ignore_ascii_case("max-content") => {
            Some(GridTrack::MaxContent)
        }
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("minmax") => {
            args.iter().find_map(|arg| match parse_grid_track(arg) {
                Some(GridTrack::Fr(value)) => Some(GridTrack::Fr(value)),
                Some(GridTrack::Length(length)) => Some(GridTrack::Length(length)),
                _ => None,
            })
        }
        _ => parse_length(value).map(GridTrack::Length),
    }
}

fn parse_grid_tracks(values: &[&ComponentValue]) -> Option<Vec<GridTrack>> {
    let mut tracks = Vec::new();
    for value in values {
        match value {
            ComponentValue::Comma => {}
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("repeat") => {
                let mut count = 1usize;
                let mut inner = Vec::new();
                let mut count_seen = false;
                for arg in args {
                    if matches!(arg, ComponentValue::Comma) {
                        continue;
                    }
                    if !count_seen && let Some(number) = number(arg) {
                        count = number.clamp(1.0, 100.0) as usize;
                        count_seen = true;
                        continue;
                    }
                    if let Some(track) = parse_grid_track(arg) {
                        inner.push(track);
                    }
                }
                if inner.is_empty() {
                    return None;
                }
                for _ in 0..count {
                    tracks.extend(inner.iter().cloned());
                }
            }
            _ => tracks.push(parse_grid_track(value)?),
        }
    }
    if tracks.is_empty() {
        None
    } else {
        Some(tracks)
    }
}

fn parse_grid_placement(value: &ComponentValue) -> Option<GridPlacement> {
    if ident(value).is_some_and(|name| name.eq_ignore_ascii_case("auto")) {
        return Some(GridPlacement::Auto);
    }
    if let Some(number) = number(value) {
        return Some(GridPlacement::Line(number as i32));
    }
    if let ComponentValue::Function { name, args } = value
        && name.eq_ignore_ascii_case("span")
    {
        let count = args.iter().find_map(number)?.max(1.0) as u32;
        return Some(GridPlacement::Span(count));
    }
    None
}

fn parse_display(value: &ComponentValue) -> Option<Display> {
    Some(match ident(value)?.as_str() {
        "none" => Display::None,
        "block" | "flow-root" => Display::Block,
        "grid" | "inline-grid" => Display::Grid,
        "inline" | "contents" => Display::Inline,
        "inline-block" => Display::InlineBlock,
        "flex" | "inline-flex" => Display::Flex,
        "list-item" => Display::ListItem,
        "table" | "inline-table" => Display::Table,
        "table-row" => Display::TableRow,
        "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
        "table-cell" => Display::TableCell,
        _ => return None,
    })
}

fn parse_position(value: &ComponentValue) -> Option<Position> {
    Some(match ident(value)?.as_str() {
        "static" => Position::Static,
        "relative" => Position::Relative,
        "absolute" => Position::Absolute,
        "fixed" => Position::Fixed,
        "sticky" => Position::Sticky,
        _ => return None,
    })
}

fn parse_font_weight(value: &ComponentValue) -> Option<u16> {
    if let Some(n) = number(value) {
        return Some(n.clamp(1.0, 1000.0) as u16);
    }
    Some(match ident(value)?.as_str() {
        "normal" => 400,
        "bold" => 700,
        "bolder" => 700,
        "lighter" => 300,
        _ => return None,
    })
}

fn parse_font_style(value: &ComponentValue) -> Option<FontStyle> {
    Some(match ident(value)?.as_str() {
        "normal" => FontStyle::Normal,
        "italic" | "oblique" => FontStyle::Italic,
        _ => return None,
    })
}

fn parse_line_height(value: &ComponentValue) -> Option<LineHeight> {
    if ident(value).as_deref() == Some("normal") {
        return Some(LineHeight::Normal);
    }
    if let Some(n) = number(value) {
        return Some(LineHeight::Number(n));
    }
    parse_length(value).map(LineHeight::Length)
}

fn parse_text_align(value: &ComponentValue) -> Option<TextAlign> {
    Some(match ident(value)?.as_str() {
        "left" | "start" => TextAlign::Left,
        "right" | "end" => TextAlign::Right,
        "center" => TextAlign::Center,
        "justify" => TextAlign::Justify,
        _ => return None,
    })
}

fn parse_white_space(value: &ComponentValue) -> Option<WhiteSpace> {
    Some(match ident(value)?.as_str() {
        "normal" => WhiteSpace::Normal,
        "nowrap" => WhiteSpace::Nowrap,
        "pre" => WhiteSpace::Pre,
        "pre-wrap" | "pre-line" | "break-spaces" => WhiteSpace::PreWrap,
        _ => return None,
    })
}

fn parse_overflow(value: &ComponentValue) -> Option<Overflow> {
    Some(match ident(value)?.as_str() {
        "visible" => Overflow::Visible,
        "hidden" | "clip" => Overflow::Hidden,
        "scroll" => Overflow::Scroll,
        "auto" | "overlay" => Overflow::Auto,
        _ => return None,
    })
}

fn parse_border_width(value: &ComponentValue) -> Option<Length> {
    if let Some(name) = ident(value) {
        return Some(match name.as_str() {
            "thin" => Length::Px(1.0),
            "medium" => Length::Px(3.0),
            "thick" => Length::Px(5.0),
            _ => return None,
        });
    }
    parse_length(value)
}

fn parse_border_style(value: &ComponentValue) -> Option<BorderStyle> {
    Some(match ident(value)?.as_str() {
        "none" | "hidden" => BorderStyle::None,
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => return None,
    })
}

fn parse_border_widths(values: &[&ComponentValue]) -> Option<[Length; 4]> {
    let widths: Vec<Length> = values
        .iter()
        .filter_map(|&value| parse_border_width(value))
        .collect();
    if widths.is_empty() {
        return None;
    }
    expand_four(&widths)
}

fn parse_border_styles(values: &[&ComponentValue]) -> Option<[BorderStyle; 4]> {
    let styles: Vec<BorderStyle> = values
        .iter()
        .filter_map(|&value| parse_border_style(value))
        .collect();
    if styles.is_empty() {
        return None;
    }
    expand_four(&styles)
}

fn parse_border_colors(values: &[&ComponentValue]) -> Option<[PaintColor; 4]> {
    let colors: Vec<PaintColor> = values
        .iter()
        .filter_map(|&value| parse_color(value))
        .collect();
    if colors.is_empty() {
        return None;
    }
    expand_four(&colors)
}

fn parse_border_shorthand(values: &[&ComponentValue]) -> Option<(Length, BorderStyle, PaintColor)> {
    let mut width = Length::Px(3.0);
    let mut style = BorderStyle::None;
    let mut color = PaintColor::CurrentColor;
    let mut found = false;
    for &value in values {
        if let Some(parsed) = parse_border_style(value) {
            style = parsed;
            found = true;
        } else if let Some(parsed) = parse_border_width(value) {
            width = parsed;
            found = true;
        } else if let Some(parsed) = parse_color(value) {
            color = parsed;
            found = true;
        }
    }
    if found {
        Some((width, style, color))
    } else {
        None
    }
}

fn edge_from_name(name: &str) -> BorderEdge {
    if name.contains("right") {
        BorderEdge::Right
    } else if name.contains("bottom") {
        BorderEdge::Bottom
    } else if name.contains("left") {
        BorderEdge::Left
    } else {
        BorderEdge::Top
    }
}

fn parse_flex_basis(value: &ComponentValue) -> Option<Length> {
    match ident(value).as_deref() {
        Some("auto") => Some(Length::Auto),
        Some("content" | "max-content" | "min-content" | "fit-content") => Some(Length::Auto),
        _ => parse_length(value),
    }
}

fn parse_flex_shorthand(values: &[&ComponentValue]) -> Option<(f32, f32, Length)> {
    if values.len() == 1 {
        match ident(values[0]).as_deref() {
            Some("none") => return Some((0.0, 0.0, Length::Auto)),
            Some("auto") => return Some((1.0, 1.0, Length::Auto)),
            Some("initial") => return Some((0.0, 1.0, Length::Auto)),
            _ => {}
        }
    }
    let mut grow: Option<f32> = None;
    let mut shrink: Option<f32> = None;
    let mut basis: Option<Length> = None;
    for &value in values {
        if let Some(n) = number(value) {
            if grow.is_none() {
                grow = Some(n);
            } else if shrink.is_none() {
                shrink = Some(n);
            }
        } else if let Some(length) = parse_flex_basis(value) {
            basis = Some(length);
        }
    }
    if grow.is_none() && shrink.is_none() && basis.is_none() {
        return None;
    }
    Some((
        grow.unwrap_or(1.0),
        shrink.unwrap_or(1.0),
        basis.unwrap_or(Length::Auto),
    ))
}

fn join_family(values: &[&ComponentValue]) -> String {
    let mut parts = Vec::new();
    for value in values {
        match value {
            ComponentValue::Ident(name) => parts.push(name.clone()),
            ComponentValue::StringLiteral(text) => parts.push(text.clone()),
            _ => {}
        }
    }
    parts.join(", ")
}

fn expand_font_shorthand(values: &[&ComponentValue], out: &mut Vec<(PropertyId, PropertyValue)>) {
    let mut i = 0;
    let mut weight = None;
    let mut style = None;
    let mut size = None;
    let mut line_height = None;
    let mut family_parts: Vec<String> = Vec::new();

    while i < values.len() && size.is_none() {
        let value = values[i];
        if let Some(parsed) = parse_font_style(value) {
            style = Some(parsed);
        } else if let Some(parsed) = parse_font_weight(value) {
            weight = Some(parsed);
        } else if let Some(keyword) = ident(value) {
            if let Some(parsed) = parse_font_size_keyword(&keyword) {
                size = Some(parsed);
            } else if matches!(keyword.as_str(), "normal" | "small-caps") {
                // Ignore font-variant keywords.
            }
        } else if let Some(parsed) = parse_length(value) {
            size = Some(parsed);
        }
        i += 1;
    }

    if size.is_some() && i < values.len() && matches!(values[i], ComponentValue::Delim('/')) {
        i += 1;
        if i < values.len() {
            line_height = parse_line_height(values[i]);
            i += 1;
        }
    }
    while i < values.len() {
        match values[i] {
            ComponentValue::Ident(name) => family_parts.push(name.clone()),
            ComponentValue::StringLiteral(text) => family_parts.push(text.clone()),
            _ => {}
        }
        i += 1;
    }

    if let Some(size) = size {
        out.push((PropertyId::FontSize, PropertyValue::FontSize(size)));
    }
    if let Some(weight) = weight {
        out.push((PropertyId::FontWeight, PropertyValue::FontWeight(weight)));
    }
    if let Some(style) = style {
        out.push((PropertyId::FontStyle, PropertyValue::FontStyle(style)));
    }
    if let Some(line_height) = line_height {
        out.push((
            PropertyId::LineHeight,
            PropertyValue::LineHeight(line_height),
        ));
    }
    if !family_parts.is_empty() {
        out.push((
            PropertyId::FontFamily,
            PropertyValue::FontFamily(family_parts.join(", ")),
        ));
    }
}
