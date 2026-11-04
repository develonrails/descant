//! A QR code, drawn crisp at any size.
//!
//! The pairing link becomes a module matrix (pure, tested) that a
//! `GtkDrawingArea` paints as filled squares on white with the standard
//! quiet zone, so the phone's camera reads it in dark mode too.

use std::{cell::RefCell, rc::Rc};

use gtk::prelude::*;

/// The modules of a QR code: `width` per side, dark ones `true`, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matrix {
    pub width: usize,
    pub dark: Vec<bool>,
}

/// Modules of white around the code; the standard asks for four.
const QUIET_ZONE: usize = 4;

pub fn matrix(data: &str) -> Option<Matrix> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let width = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Some(Matrix { width, dark })
}

/// A drawing area showing whatever [`show`] last gave it.
pub struct QrView {
    pub area: gtk::DrawingArea,
    current: Rc<RefCell<Option<Matrix>>>,
}

impl QrView {
    pub fn new(size: i32) -> QrView {
        let area = gtk::DrawingArea::builder()
            .content_width(size)
            .content_height(size)
            .halign(gtk::Align::Center)
            .build();
        area.update_property(&[gtk::accessible::Property::Label(
            "QR code to scan with your phone",
        )]);
        let current: Rc<RefCell<Option<Matrix>>> = Rc::default();
        let drawn = current.clone();
        area.set_draw_func(move |_, cr, width, height| {
            let side = width.min(height) as f64;
            let x0 = (width as f64 - side) / 2.0;
            let y0 = (height as f64 - side) / 2.0;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.rectangle(x0, y0, side, side);
            let _ = cr.fill();
            let Some(m) = drawn.borrow().clone() else {
                return;
            };
            let total = (m.width + 2 * QUIET_ZONE) as f64;
            let module = (side / total).floor().max(1.0);
            let offset = ((side - module * m.width as f64) / 2.0).floor();
            cr.set_source_rgb(0.0, 0.0, 0.0);
            for (i, dark) in m.dark.iter().enumerate() {
                if *dark {
                    let (row, col) = (i / m.width, i % m.width);
                    cr.rectangle(
                        x0 + offset + col as f64 * module,
                        y0 + offset + row as f64 * module,
                        module,
                        module,
                    );
                }
            }
            let _ = cr.fill();
        });
        QrView { area, current }
    }

    pub fn show(&self, data: &str) {
        *self.current.borrow_mut() = matrix(data);
        self.area.queue_draw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_link_becomes_a_square_matrix() {
        let m = matrix("https://spotify.com/pair?code=ABCDEF").unwrap();
        assert_eq!(m.dark.len(), m.width * m.width);
        assert!(
            (21..=41).contains(&m.width),
            "a small version, easy to scan"
        );
    }

    #[test]
    fn it_has_finder_patterns_in_three_corners() {
        let m = matrix("https://spotify.com/pair?code=ABCDEF").unwrap();
        let at = |r: usize, c: usize| m.dark[r * m.width + c];
        let w = m.width;
        // the outer ring of each 7x7 finder pattern is dark, its inner ring light
        for (r, c) in [(0, 0), (0, w - 7), (w - 7, 0)] {
            assert!(
                at(r, c) && at(r + 6, c + 6),
                "outer corners of the finder at {r},{c}"
            );
            assert!(!at(r + 1, c + 1), "light ring inside the finder at {r},{c}");
            assert!(at(r + 3, c + 3), "dark centre of the finder at {r},{c}");
        }
    }
}
