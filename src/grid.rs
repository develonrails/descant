//! Grids of albums and artists, and the cards in them.
//!
//! The Albums and Artists pages are `GtkGridView`s (only visible cards
//! exist); the artist page uses small `GtkFlowBox`es of the same cards.

use adw::prelude::*;
use descant_core::track::Cover;
use descant_spotify::{
    library::{AlbumSummary, ArtistSummary, PlaylistSummary},
    podcasts::ShowSummary,
};
use gtk::{gio, glib};

use crate::{
    images,
    nav::{Destination, Nav},
    widgets::{cover_image, pick, with_header},
};

pub const CARD: i32 = 160;

/// What a card shows.
#[derive(Clone)]
pub struct Card {
    pub uri: String,
    pub title: String,
    pub subtitle: String,
    pub covers: Vec<Cover>,
    pub round: bool,
}

impl Card {
    pub fn album(album: &AlbumSummary) -> Card {
        let year = album.year.map(|y| format!(" · {y}")).unwrap_or_default();
        Card {
            uri: album.uri.clone(),
            title: album.name.clone(),
            subtitle: format!("{}{year}", album.artists.join(", ")),
            covers: album.covers.clone(),
            round: false,
        }
    }

    pub fn artist(artist: &ArtistSummary) -> Card {
        Card {
            uri: artist.uri.clone(),
            title: artist.name.clone(),
            subtitle: String::new(),
            covers: artist.covers.clone(),
            round: true,
        }
    }

    pub fn playlist(playlist: &PlaylistSummary) -> Card {
        Card {
            uri: playlist.uri.clone(),
            title: playlist.name.clone(),
            subtitle: if !playlist.description.is_empty() {
                playlist.description.clone()
            } else if playlist.owner == "spotify" {
                "Spotify".to_string()
            } else {
                playlist.owner.clone()
            },
            covers: playlist.covers.clone(),
            round: false,
        }
    }

    pub fn show(show: &ShowSummary) -> Card {
        Card {
            uri: show.uri.clone(),
            title: show.name.clone(),
            subtitle: show.publisher.clone(),
            covers: show.covers.clone(),
            round: false,
        }
    }

    pub fn destination(&self) -> Destination {
        let (uri, name) = (self.uri.clone(), self.title.clone());
        if self.uri.starts_with("spotify:artist:") {
            Destination::Artist { uri, name }
        } else if self.uri.starts_with("spotify:show:") {
            Destination::Show { uri, name }
        } else if self.uri.starts_with("spotify:playlist:") {
            Destination::Playlist {
                uri,
                name,
                covers: self.covers.clone(),
            }
        } else {
            Destination::Album { uri, name }
        }
    }
}

/// An empty card; [`fill`] gives it content.
pub fn card_widget() -> gtk::Box {
    let cover = cover_image(CARD, "cover-large");
    let title = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(18)
        .css_classes(["heading"])
        .build();
    let subtitle = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(18)
        .css_classes(["dim-label", "caption"])
        .build();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .width_request(CARD)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    card.append(&cover);
    card.append(&title);
    card.append(&subtitle);
    card
}

pub fn fill(widget: &gtk::Widget, card: &Card) {
    let cover = widget
        .first_child()
        .and_downcast::<gtk::Image>()
        .expect("cover");
    let title = cover
        .next_sibling()
        .and_downcast::<gtk::Label>()
        .expect("title");
    let subtitle = title
        .next_sibling()
        .and_downcast::<gtk::Label>()
        .expect("subtitle");
    if card.round {
        cover.add_css_class("portrait");
        title.set_xalign(0.5);
    } else {
        cover.remove_css_class("portrait");
        title.set_xalign(0.0);
    }
    title.set_label(&card.title);
    subtitle.set_label(&card.subtitle);
    subtitle.set_visible(!card.subtitle.is_empty());
    images::show(&cover, pick(&card.covers, CARD, &cover));
}

/// A flow box of cards that open their album or artist; for short lists.
pub fn flow(nav: &Nav, cards: &[Card]) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .max_children_per_line(12)
        .valign(gtk::Align::Start)
        .build();
    for card in cards {
        let widget = card_widget();
        fill(widget.upcast_ref(), card);
        flow.append(&widget);
    }
    let (nav, cards) = (nav.clone(), cards.to_vec());
    flow.connect_child_activated(move |_, child| {
        if let Some(card) = cards.get(child.index() as usize) {
            nav.open(card.destination());
        }
    });
    flow
}

/// A full-page grid of cards, filled when `load` returns. `load` runs on
/// the main loop; reads from Spotify belong on the runtime (see [`kept`]).
///
/// [`kept`]: crate::nav::kept
pub fn grid_page<F>(nav: &Nav, title: &str, empty: &str, load: F) -> adw::NavigationPage
where
    F: std::future::Future<Output = Option<Vec<Card>>> + 'static,
{
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        item.set_child(Some(&card_widget()));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let (Some(widget), Some(object)) = (item.child(), item.item()) {
            let object = object.downcast::<glib::BoxedAnyObject>().expect("card");
            fill(&widget, &object.borrow::<Card>());
        }
    });
    let grid = gtk::GridView::builder()
        .model(&gtk::NoSelection::new(Some(store.clone())))
        .factory(&factory)
        .max_columns(12)
        .single_click_activate(true)
        .margin_start(12)
        .margin_end(12)
        .build();
    let n = nav.clone();
    let s = store.clone();
    grid.connect_activate(move |_, position| {
        if let Some(object) = s.item(position).and_downcast::<glib::BoxedAnyObject>() {
            n.open(object.borrow::<Card>().destination());
        }
    });
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&spinner, Some("loading"));
    stack.add_named(
        &gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some("grid"),
    );
    stack.add_named(
        &adw::StatusPage::builder()
            .title(empty)
            .icon_name("folder-music-symbolic")
            .build(),
        Some("empty"),
    );
    let page = adw::NavigationPage::builder()
        .title(title)
        .child(&with_header(&stack))
        .build();

    glib::spawn_future_local(async move {
        match load.await {
            Some(cards) if !cards.is_empty() => {
                let objects: Vec<glib::BoxedAnyObject> =
                    cards.into_iter().map(glib::BoxedAnyObject::new).collect();
                store.extend_from_slice(&objects);
                stack.set_visible_child_name("grid");
            }
            _ => stack.set_visible_child_name("empty"),
        }
    });
    page
}
