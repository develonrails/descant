//! Home: a greeting, quick tiles, and shelves that fill as they arrive.
//!
//! Laid out like Spotifast's home (the user's reference): Liked Songs and the
//! first playlists as tiles, then Made for You, Recently played, Your top
//! artists and Your top songs. Each section has its place from the start and
//! fills in when its data comes, so nothing jumps around.

use adw::prelude::*;
use descant_core::{supervisor::Command, track::TrackInfo};
use descant_spotify::home::{self, Item};
use gtk::glib;

use crate::{
    grid::{self, Card},
    nav::{Destination, Nav, QUICK, SLOW, kept},
    widgets::{LIKED_ICON, cover, section_title, song_list, song_row, with_header},
};

const TILE_COVER: i32 = 56;
const TILES: usize = 8;

pub fn page(nav: &Nav) -> adw::NavigationPage {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    body.append(
        &gtk::Label::builder()
            .label(greeting(
                glib::DateTime::now_local().map(|t| t.hour()).unwrap_or(12),
            ))
            .xalign(0.0)
            .css_classes(["title-1"])
            .build(),
    );
    body.append(&tiles(nav));
    let continuing = section_slot(&body, "Continue listening");
    let new_episodes = section_slot(&body, "New episodes");

    // Placeholders in their final order, filled when their data arrives.
    let made = section_slot(&body, "Made for you");
    let recent = section_slot(&body, "Recently played");
    let artists = section_slot(&body, "Your top artists");
    let songs = section_slot(&body, "Your top songs");

    let scroller = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    crate::podcasts::home_shelves(nav, &continuing, &new_episodes);
    let page = adw::NavigationPage::builder()
        .title("Home")
        .child(&with_header(&scroller))
        .build();

    let n = nav.clone();
    glib::spawn_future_local(async move {
        let Some(s) = n.session() else { return };
        let fetch = || async move { home::made_for_you(&s).await };
        if let Some(list) = kept(&n.library.home.made_for_you, SLOW, fetch).await {
            fill(
                &made,
                &shelf(&n, &list.iter().map(Card::playlist).collect::<Vec<_>>()),
            );
        }
    });
    let n = nav.clone();
    glib::spawn_future_local(async move {
        let Some(s) = n.session() else { return };
        let fetch = || async move { home::recently_played(&s, 20).await };
        if let Some(items) = kept(&n.library.home.recently_played, QUICK, fetch).await {
            let cards: Vec<Card> = items
                .iter()
                .map(|item| match item {
                    Item::Playlist(p) => Card::playlist(p),
                    Item::Album(a) => Card::album(a),
                    Item::Artist(a) => Card::artist(a),
                })
                .collect();
            fill(&recent, &shelf(&n, &cards));
        }
    });
    let n = nav.clone();
    glib::spawn_future_local(async move {
        let Some(s) = n.session() else { return };
        let fetch = || async move { home::top(&s, 10).await };
        if let Some(top) = kept(&n.library.home.top, SLOW, fetch).await {
            fill(
                &artists,
                &shelf(
                    &n,
                    &top.artists.iter().map(Card::artist).collect::<Vec<_>>(),
                ),
            );
            fill(&songs, &top_songs(&n, &top.tracks));
        }
    });
    page
}

/// "Good morning" before noon, "Good afternoon" until six, then "Good evening".
pub fn greeting(hour: i32) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// Liked Songs and the first playlists, as wide tiles in a grid.
fn tiles(nav: &Nav) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    let mut destinations = vec![Destination::Liked];
    flow.append(&tile("Liked Songs", None));
    for p in nav.library.playlists.borrow().iter().take(TILES - 1) {
        let tile = tile(&p.name, Some(&p.covers));
        if p.covers.is_empty()
            && let Some(image) = tile.first_child().and_downcast::<gtk::Image>()
        {
            nav.library.await_cover(&p.uri, &image);
        }
        flow.append(&tile);
        destinations.push(Destination::Playlist {
            uri: p.uri.clone(),
            name: p.name.clone(),
            covers: p.covers.clone(),
        });
    }
    let nav = nav.clone();
    flow.connect_child_activated(move |_, child| {
        if let Some(destination) = destinations.get(child.index() as usize) {
            nav.open(destination.clone());
        }
    });
    flow
}

fn tile(name: &str, covers: Option<&[descant_core::track::Cover]>) -> gtk::Box {
    let art: gtk::Widget = match covers {
        Some(covers) => cover(covers, TILE_COVER, "cover").upcast(),
        None => {
            let heart = gtk::Image::builder()
                .icon_name(LIKED_ICON)
                .pixel_size(24)
                .width_request(TILE_COVER)
                .height_request(TILE_COVER)
                .css_classes(["liked-tile"])
                .build();
            heart.upcast()
        }
    };
    let label = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["heading"])
        .build();
    let tile = gtk::Box::builder()
        .spacing(12)
        .css_classes(["card", "home-tile"])
        .build();
    tile.append(&art);
    tile.append(&label);
    tile
}

/// A titled, initially hidden slot for a section.
fn section_slot(body: &gtk::Box, title: &str) -> gtk::Box {
    let slot = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .build();
    slot.append(&section_title(title));
    body.append(&slot);
    slot
}

fn fill(slot: &gtk::Box, content: &impl IsA<gtk::Widget>) {
    slot.append(content);
    slot.set_visible(true);
}

/// One row of cards that scrolls sideways.
fn shelf(nav: &Nav, cards: &[Card]) -> gtk::ScrolledWindow {
    let flow = grid::flow(nav, cards);
    let n = cards.len().max(1) as u32;
    flow.set_min_children_per_line(n);
    flow.set_max_children_per_line(n);
    flow.set_homogeneous(true);
    flow.set_halign(gtk::Align::Start);
    gtk::ScrolledWindow::builder()
        .child(&flow)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .build()
}

fn top_songs(nav: &Nav, tracks: &[TrackInfo]) -> gtk::ListBox {
    let list = song_list();
    let uris: Vec<String> = tracks.iter().map(|t| t.uri.clone()).collect();
    for (i, track) in tracks.iter().enumerate() {
        let mut extra: Vec<gtk::Widget> = vec![
            gtk::Label::builder()
                .label(&track.album)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(28)
                .css_classes(["dim-label"])
                .build()
                .upcast(),
        ];
        if nav.library.liked.borrow().contains(&track.uri) {
            extra.push(
                gtk::Image::builder()
                    .icon_name(LIKED_ICON)
                    .css_classes(["accent"])
                    .build()
                    .upcast(),
            );
        }
        let row = song_row(track, &track.artist_line(), Some(i + 1), &extra);
        let (nav, uris) = (nav.clone(), uris.clone());
        row.connect_activated(move |_| {
            nav.send(Command::PlayTracks {
                tracks: uris.clone(),
                start: i,
                position_ms: 0,
            });
        });
        list.append(&row);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greetings_follow_the_time_of_day() {
        assert_eq!(greeting(7), "Good morning");
        assert_eq!(greeting(12), "Good afternoon");
        assert_eq!(greeting(20), "Good evening");
        assert_eq!(greeting(2), "Good evening");
    }
}
