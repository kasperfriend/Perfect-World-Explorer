//! Offline stand-in for the `egui` surface the Imaginator's page uses.
//! Harness for `tools/imaginator-check.sh`; the real crate ships.
#![allow(dead_code, unused_variables)]

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color32(pub [u8; 4]);
impl Color32 {
    pub const GRAY: Color32 = Color32([128, 128, 128, 255]);
    pub const LIGHT_GREEN: Color32 = Color32([144, 238, 144, 255]);
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Color32 {
        Color32([r, g, b, 255])
    }
}

pub struct RichText;
impl From<RichText> for String {
    fn from(_: RichText) -> String {
        String::new()
    }
}
impl RichText {
    pub fn new(_text: impl Into<String>) -> Self {
        RichText
    }
    pub fn small(self) -> Self {
        self
    }
    pub fn weak(self) -> Self {
        self
    }
    pub fn color(self, _c: Color32) -> Self {
        self
    }
}

pub struct Response;
impl Response {
    pub fn clicked(&self) -> bool {
        false
    }
    pub fn changed(&self) -> bool {
        false
    }
    pub fn on_hover_text(self, _text: impl Into<String>) -> Self {
        self
    }
}

pub struct Slider<'a> {
    value: &'a mut i32,
}
impl<'a> From<Slider<'a>> for Response {
    fn from(_: Slider<'a>) -> Response {
        Response
    }
}
impl<'a> Slider<'a> {
    /// Generic like the real one: the module slides `u32` and `f32` fields.
    pub fn new<T>(value: &'a mut T, _range: std::ops::RangeInclusive<T>) -> Self {
        let _ = value;
        Slider {
            value: Box::leak(Box::new(0i32)),
        }
    }
    pub fn text(self, _text: &str) -> Self {
        self
    }
    pub fn suffix(self, _suffix: &str) -> Self {
        self
    }
    pub fn logarithmic(self, _on: bool) -> Self {
        self
    }
    pub fn custom_formatter(self, _f: impl Fn(f64, usize) -> String) -> Self {
        self
    }
}

pub struct SelectableLabel {
    _selected: bool,
}
impl SelectableLabel {
    pub fn new(_selected: bool, _text: impl Into<String>) -> Self {
        SelectableLabel { _selected: false }
    }
}

pub struct Ui;
impl Ui {
    pub fn label(&mut self, _text: impl Into<String>) -> Response {
        Response
    }
    pub fn heading(&mut self, _text: impl Into<String>) -> Response {
        Response
    }
    pub fn add_space(&mut self, _amount: f32) {}
    pub fn separator(&mut self) {}
    pub fn checkbox(&mut self, _value: &mut bool, _text: impl Into<String>) -> Response {
        Response
    }
    pub fn selectable_label(&mut self, _selected: bool, _text: impl Into<String>) -> Response {
        Response
    }
    pub fn button(&mut self, _text: impl Into<String>) -> Response {
        Response
    }
    pub fn add_enabled(&mut self, _enabled: bool, _widget: SelectableLabel) -> Response {
        Response
    }
    pub fn add(&mut self, _widget: impl Into<Response>) -> Response {
        Response
    }
    pub fn horizontal(&mut self, _add: impl FnOnce(&mut Ui)) {}
}
