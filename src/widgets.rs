//! Small pieces every page builds with: covers, headers, section titles and
//! song rows.

use adw::prelude::*;
use descant_core::track::{Cover, TrackInfo, cover_at_least};
use gtk::glib;

use crate::images;

/// Liked, and the button that likes: Lucide's heart, filled and hollow
/// (`data/icons/actions`). Adwaita has none since GNOME 51.
pub const LIKED_ICON: &str = "heart-filled-symbolic";
pub const LIKE_ICON: &str = "heart-outline-symbolic";
/// An episode's jumps back and ahead: circling arrows, seconds drawn inside.
pub const SKIP_BACK_ICON: &str = "skip-back-circle-symbolic";
pub const SKIP_FORWARD_ICON: &str = "skip-forward-circle-symbolic";

/// Thumbnail size in song rows, in logical pixels.
pub const THUMB: i32 = 40;

/// A square cover of exactly `size` logical pixels, with rounded corners.
///
/// A `GtkImage` with a pixel size, not a `GtkPicture`: a picture takes the
/// image's own dimensions as its natural size, so a large cover would grow
/// the row or the bar around it.
pub fn cover_image(size: i32, class: &str) -> gtk::Image {
    gtk::Image::builder()
        .pixel_size(size)
        .css_classes([class])
        .overflow(gtk::Overflow::Hidden)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build()
}

/// The cover URL to fetch for an image of `size` logical pixels.
pub fn pick<'a>(covers: &'a [Cover], size: i32, widget: &impl IsA<gtk::Widget>) -> Option<&'a str> {
    cover_at_least(covers, device_pixels(size, widget)).map(|c| c.url.as_str())
}

/// The screen pixels `size` logical pixels take where `widget` is shown.
/// Fractional scales count: at 150 % a 160 px cover fills 240 pixels, not
/// the 320 that GTK's whole-number scale factor (2) would make of it.
pub fn device_pixels(size: i32, widget: &impl IsA<gtk::Widget>) -> u32 {
    let shown = widget.native().and_then(|n| n.surface()).map(|s| s.scale());
    // Not on screen yet (a card being built): the scale of the first monitor.
    let scale = shown
        .or_else(|| {
            let monitors = gtk::gdk::Display::default()?.monitors();
            let first = monitors.item(0).and_downcast::<gtk::gdk::Monitor>()?;
            Some(first.scale())
        })
        .unwrap_or_else(|| f64::from(widget.scale_factor()))
        .max(1.0);
    (f64::from(size) * scale).ceil() as u32
}

/// A cover image showing the right size of `covers`.
pub fn cover(covers: &[Cover], size: i32, class: &str) -> gtk::Image {
    let image = cover_image(size, class);
    images::show(&image, pick(covers, size, &image));
    image
}

/// `child` under a plain header bar.
pub fn with_header(child: &impl IsA<gtk::Widget>) -> adw::ToolbarView {
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(child));
    view
}

/// A section heading on a scrolling page.
pub fn section_title(title: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .margin_top(12)
        .css_classes(["title-3"])
        .build()
}

/// An empty boxed list for song rows.
pub fn song_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build()
}

/// A song in a boxed list: an optional number, the thumbnail, the title over
/// `subtitle`, then `extra` and the duration on the right.
pub fn song_row(
    track: &TrackInfo,
    subtitle: &str,
    number: Option<usize>,
    extra: &[gtk::Widget],
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&track.title))
        .subtitle(glib::markup_escape_text(subtitle))
        .activatable(true)
        .build();
    if let Some(n) = number {
        row.add_prefix(
            &gtk::Label::builder()
                .label(n.to_string())
                .width_chars(2)
                .css_classes(["dim-label", "numeric"])
                .build(),
        );
    }
    row.add_prefix(&cover(&track.covers, THUMB, "cover"));
    for widget in extra {
        row.add_suffix(widget);
    }
    row.add_suffix(
        &gtk::Label::builder()
            .label(format_duration(track.duration_ms))
            .css_classes(["dim-label", "numeric"])
            .build(),
    );
    row
}

pub fn format_duration(ms: u32) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_player() {
        assert_eq!(format_duration(284_000), "4:44");
        assert_eq!(format_duration(65_400), "1:05");
        assert_eq!(format_duration(0), "0:00");
    }
}
