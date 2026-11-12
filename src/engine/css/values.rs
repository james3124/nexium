//! Typed CSS values: lengths, colors, enums and parsing helpers.

use super::parser::ComponentValue;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 720.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinearLength {
    pub px: f32,
    pub percent: f32,
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    pub vmin: f32,
    pub vmax: f32,
}

impl LinearLength {
    pub const ZERO: LinearLength = LinearLength {
        px: 0.0,
        percent: 0.0,
        em: 0.0,
        rem: 0.0,
        vw: 0.0,
        vh: 0.0,
        vmin: 0.0,
        vmax: 0.0,
    };

    fn add(self, other: LinearLength) -> Self {
        Self {
            px: self.px + other.px,
            percent: self.percent + other.percent,
            em: self.em + other.em,
            rem: self.rem + other.rem,
            vw: self.vw + other.vw,
            vh: self.vh + other.vh,
            vmin: self.vmin + other.vmin,
            vmax: self.vmax + other.vmax,
        }
    }

    fn scale(self, factor: f32) -> Self {
        Self {
            px: self.px * factor,
            percent: self.percent * factor,
            em: self.em * factor,
            rem: self.rem * factor,
            vw: self.vw * factor,
            vh: self.vh * factor,
            vmin: self.vmin * factor,
            vmax: self.vmax * factor,
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Self::ZERO
    }

    pub fn resolve(
        self,
        font_size: f32,
        root_font_size: f32,
        viewport: Viewport,
        percent_base: f32,
    ) -> f32 {
        self.px
            + self.percent * percent_base / 100.0
            + self.em * font_size
            + self.rem * root_font_size
            + self.vw * viewport.width / 100.0
            + self.vh * viewport.height / 100.0
            + self.vmin * viewport.width.min(viewport.height) / 100.0
            + self.vmax * viewport.width.max(viewport.height) / 100.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Auto,
    Px(f32),
    Percent(f32),
    Em(f32),
    Rem(f32),
    Pt(f32),
    Vw(f32),
    Vh(f32),
    Vmin(f32),
    Vmax(f32),
    Calc(LinearLength),
    Min([LinearLength; 4], usize),
    Max([LinearLength; 4], usize),
    Clamp([LinearLength; 3]),
}

impl Length {
    pub const ZERO: Length = Length::Px(0.0);

    pub fn is_auto(self) -> bool {
        matches!(self, Length::Auto)
    }

    pub fn resolve(
        self,
        font_size: f32,
        root_font_size: f32,
        viewport: Viewport,
        percent_base: f32,
    ) -> Option<f32> {
        Some(match self {
            Length::Auto => return None,
            Length::Px(value) => value,
            Length::Percent(value) => percent_base * value / 100.0,
            Length::Em(value) => value * font_size,
            Length::Rem(value) => value * root_font_size,
            Length::Pt(value) => value * 96.0 / 72.0,
            Length::Vw(value) => viewport.width * value / 100.0,
            Length::Vh(value) => viewport.height * value / 100.0,
            Length::Vmin(value) => viewport.width.min(viewport.height) * value / 100.0,
            Length::Vmax(value) => viewport.width.max(viewport.height) * value / 100.0,
            Length::Calc(linear) => {
                linear.resolve(font_size, root_font_size, viewport, percent_base)
            }
            Length::Min(lengths, count) => {
                if count == 0 {
                    return None;
                }
                let mut result = f32::INFINITY;
                for length in &lengths[..count] {
                    result = result.min(length.resolve(
                        font_size,
                        root_font_size,
                        viewport,
                        percent_base,
                    ));
                }
                result
            }
            Length::Max(lengths, count) => {
                if count == 0 {
                    return None;
                }
                let mut result = f32::NEG_INFINITY;
                for length in &lengths[..count] {
                    result = result.max(length.resolve(
                        font_size,
                        root_font_size,
                        viewport,
                        percent_base,
                    ));
                }
                result
            }
            Length::Clamp([min, preferred, max]) => {
                let min = min.resolve(font_size, root_font_size, viewport, percent_base);
                let preferred =
                    preferred.resolve(font_size, root_font_size, viewport, percent_base);
                let max = max.resolve(font_size, root_font_size, viewport, percent_base);
                preferred.max(min).min(max)
            }
        })
    }

    pub fn resolve_or(
        self,
        font_size: f32,
        root_font_size: f32,
        viewport: Viewport,
        percent_base: f32,
        auto: f32,
    ) -> f32 {
        self.resolve(font_size, root_font_size, viewport, percent_base)
            .unwrap_or(auto)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GridTrack {
    Auto,
    Length(Length),
    Fr(f32),
    MinContent,
    MaxContent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridPlacement {
    #[default]
    Auto,
    Line(i32),
    Span(u32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridAutoFlow {
    #[default]
    Row,
    Column,
    RowDense,
    ColumnDense,
}

impl GridAutoFlow {
    pub fn is_column(self) -> bool {
        matches!(self, Self::Column | Self::ColumnDense)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Length(Length),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_hex(value: &str) -> Option<Self> {
        let hex = value.trim();
        let parse = |s: &str| u8::from_str_radix(s, 16).ok();
        match hex.len() {
            3 => Some(Color {
                r: parse(&hex[0..1])? * 17,
                g: parse(&hex[1..2])? * 17,
                b: parse(&hex[2..3])? * 17,
                a: 255,
            }),
            4 => Some(Color {
                r: parse(&hex[0..1])? * 17,
                g: parse(&hex[1..2])? * 17,
                b: parse(&hex[2..3])? * 17,
                a: parse(&hex[3..4])? * 17,
            }),
            6 => Some(Color {
                r: parse(&hex[0..2])?,
                g: parse(&hex[2..4])?,
                b: parse(&hex[4..6])?,
                a: 255,
            }),
            8 => Some(Color {
                r: parse(&hex[0..2])?,
                g: parse(&hex[2..4])?,
                b: parse(&hex[4..6])?,
                a: parse(&hex[6..8])?,
            }),
            _ => None,
        }
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintColor {
    CurrentColor,
    Rgba(Color),
}

impl PaintColor {
    pub fn resolve(self, current_color: Color) -> Color {
        match self {
            PaintColor::CurrentColor => current_color,
            PaintColor::Rgba(color) => color,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    Flex,
    Grid,
    ListItem,
    Table,
    TableRow,
    TableRowGroup,
    TableCell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Scroll,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Nowrap,
    Pre,
    PreWrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDecoration {
    None,
    Underline,
    LineThrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JustifyContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignItems {
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundRepeat {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundSizeMode {
    Auto,
    Cover,
    Contain,
    Explicit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackgroundSize {
    pub mode: BackgroundSizeMode,
    pub width: Length,
    pub height: Length,
}

impl Default for BackgroundSize {
    fn default() -> Self {
        Self {
            mode: BackgroundSizeMode::Auto,
            width: Length::Auto,
            height: Length::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyleType {
    Disc,
    Circle,
    Square,
    Decimal,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignSelf {
    Auto,
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Edges<T> {
    pub const fn all(value: T) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn vertical(&self) -> (T, T) {
        (self.top, self.bottom)
    }
}

impl Edges<Length> {
    pub const ZERO: Edges<Length> = Edges::all(Length::Px(0.0));
}

impl Edges<f32> {
    pub const ZERO: Edges<f32> = Edges::all(0.0);
}

pub fn significant(values: &[ComponentValue]) -> Vec<&ComponentValue> {
    values
        .iter()
        .filter(|value| !matches!(value, ComponentValue::Whitespace))
        .collect()
}

pub fn ident(value: &ComponentValue) -> Option<String> {
    match value {
        ComponentValue::Ident(name) => Some(name.to_ascii_lowercase()),
        _ => None,
    }
}

pub fn number(value: &ComponentValue) -> Option<f32> {
    match value {
        ComponentValue::Number(n) => Some(*n as f32),
        _ => None,
    }
}

pub fn percentage(value: &ComponentValue) -> Option<f32> {
    match value {
        ComponentValue::Percentage(n) => Some(*n as f32),
        _ => None,
    }
}

pub fn parse_length(value: &ComponentValue) -> Option<Length> {
    match value {
        ComponentValue::Number(n) if *n == 0.0 => Some(Length::Px(0.0)),
        ComponentValue::Percentage(n) => Some(Length::Percent(*n as f32)),
        ComponentValue::Dimension { value, unit } => {
            let value = *value as f32;
            Some(match unit.as_str() {
                "px" => Length::Px(value),
                "em" => Length::Em(value),
                "rem" => Length::Rem(value),
                "pt" => Length::Pt(value),
                "vw" => Length::Vw(value),
                "vh" => Length::Vh(value),
                "vmin" => Length::Vmin(value),
                "vmax" => Length::Vmax(value),
                "in" => Length::Px(value * 96.0),
                "cm" => Length::Px(value * 96.0 / 2.54),
                "mm" => Length::Px(value * 96.0 / 25.4),
                "q" => Length::Px(value * 96.0 / 101.6),
                "pc" => Length::Px(value * 16.0),
                _ => return None,
            })
        }
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("calc") => {
            parse_calc(args).map(Length::Calc)
        }
        ComponentValue::Function { name, args }
            if name.eq_ignore_ascii_case("min") || name.eq_ignore_ascii_case("max") =>
        {
            let lengths = parse_comma_lengths(args)?;
            let count = lengths.len().min(4);
            let mut arr = [LinearLength::ZERO; 4];
            for (slot, length) in arr.iter_mut().zip(lengths.iter()) {
                *slot = length_to_linear(*length)?;
            }
            if name.eq_ignore_ascii_case("min") {
                Some(Length::Min(arr, count))
            } else {
                Some(Length::Max(arr, count))
            }
        }
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("clamp") => {
            let lengths = parse_comma_lengths(args)?;
            if lengths.len() != 3 {
                return None;
            }
            Some(Length::Clamp([
                length_to_linear(lengths[0])?,
                length_to_linear(lengths[1])?,
                length_to_linear(lengths[2])?,
            ]))
        }
        ComponentValue::Ident(name) if name.eq_ignore_ascii_case("auto") => Some(Length::Auto),
        _ => None,
    }
}

fn parse_comma_lengths(args: &[ComponentValue]) -> Option<Vec<Length>> {
    let mut lengths = Vec::new();
    let mut start = 0;
    for index in 0..=args.len() {
        if index == args.len() || matches!(args[index], ComponentValue::Comma) {
            let group = significant(&args[start..index]);
            if group.len() != 1 {
                return None;
            }
            lengths.push(parse_length(group[0])?);
            start = index + 1;
        }
    }
    if lengths.is_empty() {
        None
    } else {
        Some(lengths)
    }
}

fn length_to_linear(length: Length) -> Option<LinearLength> {
    Some(match length {
        Length::Px(value) => LinearLength {
            px: value,
            ..LinearLength::ZERO
        },
        Length::Percent(value) => LinearLength {
            percent: value,
            ..LinearLength::ZERO
        },
        Length::Em(value) => LinearLength {
            em: value,
            ..LinearLength::ZERO
        },
        Length::Rem(value) => LinearLength {
            rem: value,
            ..LinearLength::ZERO
        },
        Length::Pt(value) => LinearLength {
            px: value * 96.0 / 72.0,
            ..LinearLength::ZERO
        },
        Length::Vw(value) => LinearLength {
            vw: value,
            ..LinearLength::ZERO
        },
        Length::Vh(value) => LinearLength {
            vh: value,
            ..LinearLength::ZERO
        },
        Length::Vmin(value) => LinearLength {
            vmin: value,
            ..LinearLength::ZERO
        },
        Length::Vmax(value) => LinearLength {
            vmax: value,
            ..LinearLength::ZERO
        },
        Length::Calc(linear) => linear,
        Length::Auto | Length::Min(..) | Length::Max(..) | Length::Clamp(..) => return None,
    })
}

fn linear_from_dimension(value: f32, unit: &str) -> Option<LinearLength> {
    let mut linear = LinearLength::ZERO;
    match unit {
        "px" => linear.px = value,
        "em" => linear.em = value,
        "rem" => linear.rem = value,
        "vw" => linear.vw = value,
        "vh" => linear.vh = value,
        "vmin" => linear.vmin = value,
        "vmax" => linear.vmax = value,
        "pt" => linear.px = value * 96.0 / 72.0,
        "in" => linear.px = value * 96.0,
        "cm" => linear.px = value * 96.0 / 2.54,
        "mm" => linear.px = value * 96.0 / 25.4,
        "q" => linear.px = value * 96.0 / 101.6,
        "pc" => linear.px = value * 16.0,
        _ => return None,
    }
    Some(linear)
}

#[derive(Clone, Copy, Debug)]
enum CalcValue {
    Length(LinearLength),
    Number(f32),
}

impl CalcValue {
    fn as_length(self) -> Option<LinearLength> {
        match self {
            CalcValue::Length(length) => Some(length),
            CalcValue::Number(number) => {
                if number == 0.0 {
                    Some(LinearLength::ZERO)
                } else {
                    None
                }
            }
        }
    }

    fn as_number(self) -> Option<f32> {
        match self {
            CalcValue::Number(number) => Some(number),
            CalcValue::Length(_) => None,
        }
    }

    fn negate(self) -> Self {
        match self {
            CalcValue::Length(length) => CalcValue::Length(length.scale(-1.0)),
            CalcValue::Number(number) => CalcValue::Number(-number),
        }
    }
}

struct CalcParser<'a> {
    values: Vec<&'a ComponentValue>,
    position: usize,
}

pub fn parse_calc(args: &[ComponentValue]) -> Option<LinearLength> {
    let mut parser = CalcParser {
        values: significant(args),
        position: 0,
    };
    let value = parser.expression()?;
    if parser.position != parser.values.len() {
        return None;
    }
    value.as_length()
}

impl<'a> CalcParser<'a> {
    fn peek(&self) -> Option<&ComponentValue> {
        self.values.get(self.position).copied()
    }

    fn expression(&mut self) -> Option<CalcValue> {
        let mut value = self.term()?;
        while let Some(ComponentValue::Delim(op @ ('+' | '-'))) = self.peek().cloned() {
            self.position += 1;
            let right = self.term()?;
            let left = value.as_length()?;
            let right = right.as_length()?;
            value = CalcValue::Length(if op == '+' {
                left.add(right)
            } else {
                left.add(right.scale(-1.0))
            });
        }
        Some(value)
    }

    fn term(&mut self) -> Option<CalcValue> {
        let mut value = self.factor()?;
        while let Some(ComponentValue::Delim(op @ ('*' | '/'))) = self.peek().cloned() {
            self.position += 1;
            let right = self.factor()?;
            value = if op == '*' {
                match (value, right) {
                    (CalcValue::Length(length), CalcValue::Number(number)) => {
                        CalcValue::Length(length.scale(number))
                    }
                    (CalcValue::Number(number), CalcValue::Length(length)) => {
                        CalcValue::Length(length.scale(number))
                    }
                    (CalcValue::Number(left), CalcValue::Number(right)) => {
                        CalcValue::Number(left * right)
                    }
                    _ => return None,
                }
            } else {
                let divisor = right.as_number()?;
                if divisor == 0.0 {
                    return None;
                }
                match value {
                    CalcValue::Length(length) => CalcValue::Length(length.scale(1.0 / divisor)),
                    CalcValue::Number(number) => CalcValue::Number(number / divisor),
                }
            };
        }
        Some(value)
    }

    fn factor(&mut self) -> Option<CalcValue> {
        let value = self.peek()?.clone();
        match value {
            ComponentValue::Delim('-') => {
                self.position += 1;
                Some(self.factor()?.negate())
            }
            ComponentValue::Delim('+') => {
                self.position += 1;
                self.factor()
            }
            ComponentValue::LParen => {
                self.position += 1;
                let value = self.expression()?;
                if matches!(self.peek(), Some(ComponentValue::RParen)) {
                    self.position += 1;
                    Some(value)
                } else {
                    None
                }
            }
            ComponentValue::Number(number) => {
                self.position += 1;
                Some(CalcValue::Number(number as f32))
            }
            ComponentValue::Percentage(percent) => {
                self.position += 1;
                let mut linear = LinearLength::ZERO;
                linear.percent = percent as f32;
                Some(CalcValue::Length(linear))
            }
            ComponentValue::Dimension { value, unit } => {
                self.position += 1;
                linear_from_dimension(value as f32, &unit).map(CalcValue::Length)
            }
            ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("calc") => {
                self.position += 1;
                parse_calc(&args).map(CalcValue::Length)
            }
            _ => None,
        }
    }
}

pub fn parse_lengths(values: &[ComponentValue]) -> Option<Vec<Length>> {
    let mut out = Vec::new();
    for value in significant(values) {
        if matches!(value, ComponentValue::Comma) {
            continue;
        }
        out.push(parse_length(value)?);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Expand a 1-4 value shorthand into top/right/bottom/left.
pub fn expand_four<T: Copy>(values: &[T]) -> Option<[T; 4]> {
    match values {
        [all] => Some([*all, *all, *all, *all]),
        [vertical, horizontal] => Some([*vertical, *horizontal, *vertical, *horizontal]),
        [top, horizontal, bottom] => Some([*top, *horizontal, *bottom, *horizontal]),
        [top, right, bottom, left] => Some([*top, *right, *bottom, *left]),
        _ => None,
    }
}

pub fn parse_font_size_keyword(name: &str) -> Option<Length> {
    Some(match name {
        "xx-small" => Length::Px(9.0),
        "x-small" => Length::Px(10.0),
        "small" => Length::Px(13.0),
        "medium" => Length::Px(16.0),
        "large" => Length::Px(18.0),
        "x-large" => Length::Px(24.0),
        "xx-large" => Length::Px(32.0),
        "xxx-large" => Length::Px(48.0),
        "smaller" => Length::Em(0.833),
        "larger" => Length::Em(1.2),
        _ => return None,
    })
}

pub fn parse_url(value: &ComponentValue) -> Option<String> {
    match value {
        ComponentValue::Url(url) => Some(url.clone()),
        ComponentValue::Function { name, args } if name.eq_ignore_ascii_case("url") => {
            for arg in significant(args) {
                match arg {
                    ComponentValue::StringLiteral(text) => return Some(text.clone()),
                    ComponentValue::Ident(text) => return Some(text.clone()),
                    ComponentValue::Url(text) => return Some(text.clone()),
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

pub fn parse_color(value: &ComponentValue) -> Option<PaintColor> {
    match value {
        ComponentValue::Hash { value, .. } => Color::from_hex(value).map(PaintColor::Rgba),
        ComponentValue::Ident(name) => {
            let name = name.to_ascii_lowercase();
            if name == "transparent" {
                return Some(PaintColor::Rgba(Color::TRANSPARENT));
            }
            if name == "currentcolor" {
                return Some(PaintColor::CurrentColor);
            }
            named_color(&name).map(PaintColor::Rgba)
        }
        ComponentValue::Function { name, args } => {
            let name = name.to_ascii_lowercase();
            if name == "rgb" || name == "rgba" {
                parse_rgb(args).map(PaintColor::Rgba)
            } else if name == "hsl" || name == "hsla" {
                parse_hsl(args).map(PaintColor::Rgba)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn color_channel(value: &ComponentValue) -> Option<f32> {
    match value {
        ComponentValue::Number(n) => Some(*n as f32),
        ComponentValue::Percentage(p) => Some((*p as f32) * 255.0 / 100.0),
        _ => None,
    }
}

fn color_alpha(value: &ComponentValue) -> Option<f32> {
    match value {
        ComponentValue::Number(n) => Some(*n as f32),
        ComponentValue::Percentage(p) => Some((*p as f32) / 100.0),
        _ => None,
    }
}

fn parse_rgb(args: &[ComponentValue]) -> Option<Color> {
    let values = significant(args);
    let mut parts: Vec<&ComponentValue> = Vec::new();
    let mut alpha: Option<f32> = None;
    let mut slash = false;
    for value in values {
        match value {
            ComponentValue::Comma => {}
            ComponentValue::Delim('/') => slash = true,
            other => {
                if slash {
                    alpha = color_alpha(other);
                } else {
                    parts.push(other);
                }
            }
        }
    }
    if parts.len() < 3 {
        return None;
    }
    let r = color_channel(parts[0])?.clamp(0.0, 255.0) as u8;
    let g = color_channel(parts[1])?.clamp(0.0, 255.0) as u8;
    let b = color_channel(parts[2])?.clamp(0.0, 255.0) as u8;
    if alpha.is_none() && parts.len() >= 4 {
        alpha = color_alpha(parts[3]);
    }
    let a = (alpha.unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color::rgba(r, g, b, a))
}

fn parse_hsl(args: &[ComponentValue]) -> Option<Color> {
    let values = significant(args);
    let mut parts: Vec<&ComponentValue> = Vec::new();
    let mut alpha: Option<f32> = None;
    let mut slash = false;
    for value in values {
        match value {
            ComponentValue::Comma => {}
            ComponentValue::Delim('/') => slash = true,
            other => {
                if slash {
                    alpha = color_alpha(other);
                } else {
                    parts.push(other);
                }
            }
        }
    }
    if parts.len() < 3 {
        return None;
    }
    let hue = match parts[0] {
        ComponentValue::Number(n) => *n as f32,
        ComponentValue::Dimension { value, unit } if unit == "deg" => *value as f32,
        ComponentValue::Dimension { value, unit } if unit == "turn" => *value as f32 * 360.0,
        _ => return None,
    };
    let saturation = percentage(parts[1])? / 100.0;
    let lightness = percentage(parts[2])? / 100.0;
    if alpha.is_none() && parts.len() >= 4 {
        alpha = color_alpha(parts[3]);
    }
    let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
    let a = (alpha.unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color::rgba(r, g, b, a))
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let hue = hue.rem_euclid(360.0) / 360.0;
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    if saturation == 0.0 {
        let value = (lightness * 255.0).round() as u8;
        return (value, value, value);
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let channel = |mut t: f32| -> u8 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let value = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (value * 255.0).round() as u8
    };
    (
        channel(hue + 1.0 / 3.0),
        channel(hue),
        channel(hue - 1.0 / 3.0),
    )
}

pub fn named_color(name: &str) -> Option<Color> {
    let rgb = NAMED_COLORS.iter().find(|(n, _)| *n == name)?.1;
    Some(Color::rgba(
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
        255,
    ))
}

static NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xF0F8FF),
    ("antiquewhite", 0xFAEBD7),
    ("aqua", 0x00FFFF),
    ("aquamarine", 0x7FFFD4),
    ("azure", 0xF0FFFF),
    ("beige", 0xF5F5DC),
    ("bisque", 0xFFE4C4),
    ("black", 0x000000),
    ("blanchedalmond", 0xFFEBCD),
    ("blue", 0x0000FF),
    ("blueviolet", 0x8A2BE2),
    ("brown", 0xA52A2A),
    ("burlywood", 0xDEB887),
    ("cadetblue", 0x5F9EA0),
    ("chartreuse", 0x7FFF00),
    ("chocolate", 0xD2691E),
    ("coral", 0xFF7F50),
    ("cornflowerblue", 0x6495ED),
    ("cornsilk", 0xFFF8DC),
    ("crimson", 0xDC143C),
    ("cyan", 0x00FFFF),
    ("darkblue", 0x00008B),
    ("darkcyan", 0x008B8B),
    ("darkgoldenrod", 0xB8860B),
    ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xA9A9A9),
    ("darkkhaki", 0xBDB76B),
    ("darkmagenta", 0x8B008B),
    ("darkolivegreen", 0x556B2F),
    ("darkorange", 0xFF8C00),
    ("darkorchid", 0x9932CC),
    ("darkred", 0x8B0000),
    ("darksalmon", 0xE9967A),
    ("darkseagreen", 0x8FBC8F),
    ("darkslateblue", 0x483D8B),
    ("darkslategray", 0x2F4F4F),
    ("darkslategrey", 0x2F4F4F),
    ("darkturquoise", 0x00CED1),
    ("darkviolet", 0x9400D3),
    ("deeppink", 0xFF1493),
    ("deepskyblue", 0x00BFFF),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1E90FF),
    ("firebrick", 0xB22222),
    ("floralwhite", 0xFFFAF0),
    ("forestgreen", 0x228B22),
    ("fuchsia", 0xFF00FF),
    ("gainsboro", 0xDCDCDC),
    ("ghostwhite", 0xF8F8FF),
    ("gold", 0xFFD700),
    ("goldenrod", 0xDAA520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xADFF2F),
    ("grey", 0x808080),
    ("honeydew", 0xF0FFF0),
    ("hotpink", 0xFF69B4),
    ("indianred", 0xCD5C5C),
    ("indigo", 0x4B0082),
    ("ivory", 0xFFFFF0),
    ("khaki", 0xF0E68C),
    ("lavender", 0xE6E6FA),
    ("lavenderblush", 0xFFF0F5),
    ("lawngreen", 0x7CFC00),
    ("lemonchiffon", 0xFFFACD),
    ("lightblue", 0xADD8E6),
    ("lightcoral", 0xF08080),
    ("lightcyan", 0xE0FFFF),
    ("lightgoldenrodyellow", 0xFAFAD2),
    ("lightgray", 0xD3D3D3),
    ("lightgreen", 0x90EE90),
    ("lightgrey", 0xD3D3D3),
    ("lightpink", 0xFFB6C1),
    ("lightsalmon", 0xFFA07A),
    ("lightseagreen", 0x20B2AA),
    ("lightskyblue", 0x87CEFA),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xB0C4DE),
    ("lightyellow", 0xFFFFE0),
    ("lime", 0x00FF00),
    ("limegreen", 0x32CD32),
    ("linen", 0xFAF0E6),
    ("magenta", 0xFF00FF),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66CDAA),
    ("mediumblue", 0x0000CD),
    ("mediumorchid", 0xBA55D3),
    ("mediumpurple", 0x9370DB),
    ("mediumseagreen", 0x3CB371),
    ("mediumslateblue", 0x7B68EE),
    ("mediumspringgreen", 0x00FA9A),
    ("mediumturquoise", 0x48D1CC),
    ("mediumvioletred", 0xC71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xF5FFFA),
    ("mistyrose", 0xFFE4E1),
    ("moccasin", 0xFFE4B5),
    ("navajowhite", 0xFFDEAD),
    ("navy", 0x000080),
    ("oldlace", 0xFDF5E6),
    ("olive", 0x808000),
    ("olivedrab", 0x6B8E23),
    ("orange", 0xFFA500),
    ("orangered", 0xFF4500),
    ("orchid", 0xDA70D6),
    ("palegoldenrod", 0xEEE8AA),
    ("palegreen", 0x98FB98),
    ("paleturquoise", 0xAFEEEE),
    ("palevioletred", 0xDB7093),
    ("papayawhip", 0xFFEFD5),
    ("peachpuff", 0xFFDAB9),
    ("peru", 0xCD853F),
    ("pink", 0xFFC0CB),
    ("plum", 0xDDA0DD),
    ("powderblue", 0xB0E0E6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xFF0000),
    ("rosybrown", 0xBC8F8F),
    ("royalblue", 0x4169E1),
    ("saddlebrown", 0x8B4513),
    ("salmon", 0xFA8072),
    ("sandybrown", 0xF4A460),
    ("seagreen", 0x2E8B57),
    ("seashell", 0xFFF5EE),
    ("sienna", 0xA0522D),
    ("silver", 0xC0C0C0),
    ("skyblue", 0x87CEEB),
    ("slateblue", 0x6A5ACD),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xFFFAFA),
    ("springgreen", 0x00FF7F),
    ("steelblue", 0x4682B4),
    ("tan", 0xD2B48C),
    ("teal", 0x008080),
    ("thistle", 0xD8BFD8),
    ("tomato", 0xFF6347),
    ("turquoise", 0x40E0D0),
    ("violet", 0xEE82EE),
    ("wheat", 0xF5DEB3),
    ("white", 0xFFFFFF),
    ("whitesmoke", 0xF5F5F5),
    ("yellow", 0xFFFF00),
    ("yellowgreen", 0x9ACD32),
];
