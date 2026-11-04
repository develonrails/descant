//! An artist: portrait, popular tracks, albums and singles.

use adw::prelude::*;
use descant_spotify::library;
use gtk::glib;

use crate::{
    grid::{self, Card},
    images,
    nav::Nav,
    runtime,
    widgets::{cover_image, pick, section_title, song_list, song_row, with_header},
};

const PORTRAIT: i32 = 128;
/// Singles can run into the hundreds; the page shows the newest few.
const SINGLES_SHOWN: usize = 24;

pub fn page(nav: &Nav, uri: &str, name: &str) -> adw::NavigationPage {
    let portrait = cover_image(PORTRAIT, "portrait");
    let heading = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["title-1"])
        .build();
    let play = gtk::Button::builder()
        .label("Play")
        .halign(gtk::Align::Start)
        .css_classes(["pill", "suggested-action"])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .valign(gtk::Align::Center)
        .build();
    text.append(&heading);
    text.append(&play);
    let header = gtk::Box::builder().spacing(18).build();
    header.append(&portrait);
    header.append(&text);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    body.append(&header);
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .build();
    body.append(&spinner);

    let scroller = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let page = adw::NavigationPage::builder()
        .title(name)
        .child(&with_header(&scroller))
        .build();

    let n = nav.clone();
    let context = uri.to_string();
    play.connect_clicked(move |_| n.play(&context, None));

    let (nav, uri, weak_page) = (nav.clone(), uri.to_string(), page.downgrade());
    glib::spawn_future_local(async move {
        let (session, u) = (nav.session(), uri.clone());
        let loaded = runtime()
            .spawn(async move { library::artist(&session, &u, SINGLES_SHOWN).await })
            .await;
        spinner.set_visible(false);
        let Ok(Ok(artist)) = loaded else {
            log::warn!("could not load artist {uri}");
            return;
        };
        images::show(&portrait, pick(&artist.artist.covers, PORTRAIT, &portrait));
        // A pasted link opens the page without a name: take Spotify's.
        if heading.label().is_empty() {
            heading.set_label(&artist.artist.name);
            if let Some(page) = weak_page.upgrade() {
                page.set_title(&artist.artist.name);
            }
        }

        if !artist.top_tracks.is_empty() {
            body.append(&section_title("Popular"));
            let list = song_list();
            for track in &artist.top_tracks {
                let row = song_row(track, &track.album, None, &[]);
                let (n, ctx, t) = (nav.clone(), uri.clone(), track.uri.clone());
                row.connect_activated(move |_| n.play(&ctx, Some(&t)));
                list.append(&row);
            }
            body.append(&list);
        }
        for (title, albums) in [
            ("Albums", artist.albums.as_slice()),
            ("Singles and EPs", artist.singles.as_slice()),
        ] {
            if albums.is_empty() {
                continue;
            }
            body.append(&section_title(title));
            let cards: Vec<Card> = albums.iter().map(Card::album).collect();
            body.append(&grid::flow(&nav, &cards));
        }
    });
    page
}
