//! The smallest `egui` that lets `src/diagnostics.rs` compile under the offline
//! harness, so its tests can actually run.
//!
//! `diagnostics` has hundreds of lines of `#[test]` code and the harness stubbed
//! the module out entirely, so none of it had ever executed — the same gap the
//! harness comments record for `gfx_player` before `player` existed. Stubbing the
//! whole module is the cheap way out and hides whatever is inside it.
//!
//! This is not an `egui` implementation and must never be used by the shipped
//! build: it exists only to satisfy the handful of builder calls `ui()` makes.
//! Widgets return themselves and do nothing, which is exactly right for a test
//! that never draws a frame.

#![allow(dead_code)]

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Key {
    #[default]
    Enter,
}

/// `RichText` is a widget, not text: real `egui` has `impl Widget for RichText`,
/// so `ui.label(RichText::new(..))` compiles there and must compile here too.
/// The offline build would otherwise reject the shipped call sites.
#[derive(Clone, Default)]
pub struct RichText;
impl RichText {
    pub fn new(_text: impl Into<String>) -> Self {
        Self
    }
    pub fn small(self) -> Self {
        self
    }
    pub fn weak(self) -> Self {
        self
    }
    pub fn monospace(self) -> Self {
        self
    }
    pub fn strong(self) -> Self {
        self
    }
    pub fn italics(self) -> Self {
        self
    }
}

#[derive(Clone, Copy, Default)]
pub struct Response;
impl Response {
    pub fn clicked(&self) -> bool {
        false
    }
    pub fn hovered(&self) -> bool {
        false
    }
    pub fn lost_focus(&self) -> bool {
        false
    }
    pub fn on_hover_text(self, _text: impl Into<String>) -> Self {
        self
    }
    pub fn changed(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CursorIcon;

pub struct Ui;
impl Ui {
    pub fn label(&mut self, _item: impl Widget) -> Response {
        Response
    }
    pub fn label_with(&mut self, _item: impl Into<String>) -> Response {
        Response
    }
    pub fn monospace(&mut self, _item: impl Widget) -> Response {
        Response
    }
    pub fn button(&mut self, _text: impl Widget) -> Response {
        Response
    }
    pub fn add_sized(&mut self, _size: [f32; 2], _widget: impl Widget) -> Response {
        Response
    }
    pub fn selectable_value<T>(
        &mut self,
        _current: &mut T,
        _value: T,
        _text: impl Widget,
    ) -> Response {
        Response
    }
    pub fn checkbox(&mut self, _checked: &mut bool, _text: impl Into<String>) -> Response {
        Response
    }
    pub fn add(&mut self, _widget: impl Widget) -> Response {
        Response
    }
    pub fn separator(&mut self) -> Response {
        Response
    }
    pub fn add_space(&mut self, _amount: f32) -> Response {
        Response
    }
    pub fn horizontal(&mut self, _add: impl FnOnce(&mut Ui)) -> Response {
        Response
    }
    pub fn horizontal_wrapped(&mut self, _add: impl FnOnce(&mut Ui)) -> Response {
        Response
    }
    pub fn vertical(&mut self, _add: impl FnOnce(&mut Ui)) -> Response {
        Response
    }
    pub fn available_width(&self) -> f32 {
        0.
    }
    pub fn input<R>(&self, _reader: impl FnOnce(&InputState) -> R) -> R
    where
        R: Default,
    {
        R::default()
    }
    pub fn output_mut<R>(&mut self, _writer: impl FnOnce(&mut Output) -> R) -> R
    where
        R: Default,
    {
        R::default()
    }
}

pub struct InputState;
impl InputState {
    pub fn key_pressed(&self, _key: Key) -> bool {
        false
    }
}

#[derive(Default)]
pub struct Output {
    pub cursor_icon: CursorIcon,
    /// Real `egui` puts a string to be copied here; the settings page writes the
    /// log tail into it for the Copy button.
    pub copied_text: String,
}

#[derive(Clone, Copy, Default)]
pub struct Size;
impl Size {
    pub const fn new(_w: f32, _h: f32) -> Self {
        Self
    }
}

pub trait Widget {}
impl Widget for RichText {}
impl Widget for String {}
impl Widget for &str {}
impl Widget for TextEdit {}
impl Widget for SelectableLabel {}
impl<R> Widget for R where R: FnOnce(&mut Ui) -> Response + Copy {}

pub struct TextEdit;
impl TextEdit {
    pub fn singleline(_text: &mut String) -> Self {
        Self
    }
    pub fn hint_text(self, _text: impl Into<String>) -> Self {
        self
    }
    pub fn desired_width(self, _width: f32) -> Self {
        self
    }
}

pub struct SelectableLabel;
impl SelectableLabel {
    pub fn new(_selected: bool, _text: impl Into<String>) -> Self {
        Self
    }
}

pub struct CollapsingHeader;
impl CollapsingHeader {
    pub fn new(_text: impl Into<String>) -> Self {
        Self
    }
    pub fn default_open(self, _open: bool) -> Self {
        self
    }
    pub fn show(self, _ui: &mut Ui, _add: impl FnOnce(&mut Ui)) -> Response {
        Response
    }
}

pub struct ScrollArea;
impl ScrollArea {
    pub fn vertical() -> Self {
        Self
    }
    pub fn max_height(self, _height: f32) -> Self {
        self
    }
    pub fn id_source(self, _source: impl std::hash::Hash) -> Self {
        self
    }
    pub fn stick_to_bottom(self, _stick: bool) -> Self {
        self
    }
    pub fn auto_shrink(self, _shrink: [bool; 2]) -> Self {
        self
    }
    pub fn show(self, _ui: &mut Ui, _add: impl FnOnce(&mut Ui)) -> Response {
        Response
    }
}
