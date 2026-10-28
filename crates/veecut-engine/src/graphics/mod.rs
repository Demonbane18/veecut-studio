//! Rust renderer for captions and text items (cosmic-text + tiny-skia), shared by export and the desktop preview.
//!
//! TEMP: API shell so the compositor and the desktop editor compile while the graphics owner
//! implements it. The graphics owner replaces this whole file; keep these signatures.

use crate::project::{Canvas, Project, Rect};

pub struct Graphics;

impl Graphics {
    pub fn new() -> Self {
        Graphics
    }

    /// Draws every visible text and caption item at timeline time `t`. The pixmap has the
    /// canvas aspect ratio at any size; drawing scales to it.
    pub fn draw(&mut self, _project: &Project, _t: f64, _pixmap: &mut tiny_skia::PixmapMut) {}

    pub fn font_families(&self) -> Vec<String> {
        Vec::new()
    }
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new()
    }
}

/// Area clear of platform buttons and captions for this canvas, normalized.
pub fn safe_area(_canvas: &Canvas) -> Rect {
    Rect::FULL
}
