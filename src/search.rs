//! The Search page: a search field in the header bar, results in sections.

use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use descant_spotify::search::{self, SearchResults};
use gtk::glib;

use crate::{
    grid::{self, Card},
    nav::{Destination, Nav},
    runtime,
    widgets::{section_title, song_list, song_row},
};

/// Results per kind.
const LIMIT: u32 = 10;

pub fn page(nav: &Nav) -> adw::NavigationPage {
    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Songs, artists, albums, playlists")
        .search_delay(350)
        .hexpand(true)
        .build();
    let clamp = adw::Clamp::builder()
        .maximum_size(480)
        .child(&entry)
        .build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&clamp));

    let results = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let idle = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("Search Spotify")
        .description("Find songs, artists, albums and playlists.")
        .build();
    let nothing = adw::StatusPage::builder()
        .icon_name("edit-find-symbolic")
        .title("No Results")
        .build();
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&idle, Some("idle"));
    stack.add_named(&spinner, Some("searching"));
    stack.add_named(&nothing, Some("nothing"));
    stack.add_named(
        &gtk::ScrolledWindow::builder()
            .child(&results)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some("results"),
    );
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    let page = adw::NavigationPage::builder()
        .title("Search")
        .child(&view)
        .build();

    let generation = Rc::new(Cell::new(0u64));
    let links = nav.clone();
    let nav = nav.clone();
    entry.connect_search_changed(move |entry| {
        let term = entry.text().trim().to_string();
        let current = generation.get() + 1;
        generation.set(current);
        if term.is_empty() {
            stack.set_visible_child_name("idle");
            return;
        }
        stack.set_visible_child_name("searching");
        let (nav, stack, results, generation) = (
            nav.clone(),
            stack.clone(),
            results.clone(),
            generation.clone(),
        );
        glib::spawn_future_local(async move {
            let session = nav.session();
            let found = runtime()
                .spawn(async move { search::search(&session, &term, LIMIT).await })
                .await;
            if generation.get() != current {
                return; // the user typed on
            }
            match found {
                Ok(Ok(found)) if !found.is_empty() => {
                    show(&nav, &results, &found);
                    stack.set_visible_child_name("results");
                }
                Ok(Err(e)) => {
                    log::warn!("search: {e}");
                    stack.set_visible_child_name("nothing");
                }
                _ => stack.set_visible_child_name("nothing"),
            }
        });
    });
    // A pasted Spotify link plays (a track) or opens (anything else) on Enter.
    entry.connect_activate(move |entry| {
        let Some(item) = descant_core::link::SpotifyRef::parse(&entry.text()) else {
            return;
        };
        let (uri, name) = (item.to_string(), String::new());
        use descant_core::link::Kind;
        match item.kind {
            Kind::Track => links.play(&uri, None),
            Kind::Album => links.open(Destination::Album { uri, name }),
            Kind::Artist => links.open(Destination::Artist { uri, name }),
            Kind::Playlist => links.open(Destination::Playlist {
                uri,
                name,
                covers: vec![],
            }),
        }
        entry.set_text("");
    });
    let focus = entry.clone();
    page.connect_shown(move |_| {
        focus.grab_focus();
    });
    page
}

fn show(nav: &Nav, results: &gtk::Box, found: &SearchResults) {
    while let Some(child) = results.first_child() {
        results.remove(&child);
    }
    if !found.tracks.is_empty() {
        results.append(&section_title("Songs"));
        let list = song_list();
        for track in &found.tracks {
            let subtitle = format!("{} · {}", track.artist_line(), track.album);
            let row = song_row(track, &subtitle, None, &[]);
            let (nav, uri) = (nav.clone(), track.uri.clone());
            row.connect_activated(move |_| nav.play(&uri, None));
            list.append(&row);
        }
        results.append(&list);
    }
    let groups: [(&str, Vec<Card>); 3] = [
        ("Artists", found.artists.iter().map(Card::artist).collect()),
        ("Albums", found.albums.iter().map(Card::album).collect()),
        (
            "Playlists",
            found.playlists.iter().map(Card::playlist).collect(),
        ),
    ];
    for (title, cards) in groups {
        if !cards.is_empty() {
            results.append(&section_title(title));
            results.append(&grid::flow(nav, &cards));
        }
    }
}
