//! Grids of albums and artists, and the cards in them.
//!
//! The Albums and Artists pages are `GtkGridView`s (only visible cards
//! exist); the artist page, Home's shelves and search use `GtkFlowBox`es of
//! the same cards, whose covers come as the cards near the screen.

use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use descant_core::track::Cover;
use descant_spotify::{
    library::{AlbumSummary, ArtistSummary, PlaylistSummary},
    podcasts::ShowSummary,
};
use gtk::{gdk, gio, glib};

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
    let cover = label(widget, card);
    images::show(&cover, pick(&card.covers, CARD, &cover));
}

/// Gives a card everything but its cover; returns the image the cover goes in.
fn label(widget: &gtk::Widget, card: &Card) -> gtk::Image {
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
    cover
}

/// A flow box of cards that open their album or artist; for short lists.
/// Their covers come as they near the screen ([`covers_in_view`]).
pub fn flow(nav: &Nav, cards: &[Card]) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .max_children_per_line(12)
        .valign(gtk::Align::Start)
        .build();
    for card in cards {
        let widget = card_widget();
        label(widget.upcast_ref(), card);
        flow.append(&widget);
    }
    let (nav, cards) = (nav.clone(), Rc::new(cards.to_vec()));
    covers_in_view(&flow, cards.clone());
    flow.connect_child_activated(move |_, child| {
        if let Some(card) = cards.get(child.index() as usize) {
            nav.open(card.destination());
        }
    });
    flow
}

/// How far outside the visible part of a page a card fetches its cover, in
/// logical pixels: one card ahead.
const AHEAD: f32 = CARD as f32;

/// Which cards of a flow box still wait for their cover, and the frame
/// clock that checks them while the box is on screen.
struct Pending {
    cards: Vec<usize>,
    watch: Option<(gdk::FrameClock, glib::SignalHandlerId)>,
}

impl Pending {
    fn stop(&mut self) {
        if let Some((clock, handler)) = self.watch.take() {
            clock.disconnect(handler);
        }
    }
}

/// Gives the cards of `flow` their covers once they come within [`AHEAD`]
/// of what every scrolled window around them shows. A shelf on Home shows
/// five or six of its twenty cards and an artist can have a hundred albums;
/// the cards nobody scrolls to fetch and decode nothing.
///
/// Checked after each frame the window paints while the box is on screen
/// and covers are still to come: scrolling and resizing both paint, and
/// nothing is checked once every card has its cover.
fn covers_in_view(flow: &gtk::FlowBox, cards: Rc<Vec<Card>>) {
    let pending = Rc::new(RefCell::new(Pending {
        cards: (0..cards.len()).collect(),
        watch: None,
    }));
    let p = pending.clone();
    flow.connect_map(move |flow| {
        let Some(clock) = flow.frame_clock() else {
            return;
        };
        let mut state = p.borrow_mut();
        if state.cards.is_empty() || state.watch.is_some() {
            return;
        }
        let (weak, cards, check) = (flow.downgrade(), cards.clone(), p.clone());
        let handler = clock.connect_after_paint(move |_| {
            let mut state = check.borrow_mut();
            match weak.upgrade() {
                Some(flow) => reveal(&flow, &cards, &mut state.cards),
                None => state.cards.clear(),
            }
            if state.cards.is_empty() {
                state.stop();
            }
        });
        state.watch = Some((clock, handler));
    });
    flow.connect_unmap(move |_| pending.borrow_mut().stop());
}

/// Shows the covers of the `waiting` cards that are near the screen, and
/// takes them off the list.
fn reveal(flow: &gtk::FlowBox, cards: &[Card], waiting: &mut Vec<usize>) {
    let scrollers: Vec<gtk::ScrolledWindow> = std::iter::successors(flow.parent(), |w| w.parent())
        .filter_map(|w| w.downcast::<gtk::ScrolledWindow>().ok())
        .collect();
    waiting.retain(|&i| {
        let (Some(child), Some(card)) = (flow.child_at_index(i as i32), cards.get(i)) else {
            return false;
        };
        let near = |scroller: &gtk::ScrolledWindow| {
            child.compute_bounds(scroller).is_some_and(|b| {
                overlaps(b.x(), b.width(), scroller.width() as f32, AHEAD)
                    && overlaps(b.y(), b.height(), scroller.height() as f32, AHEAD)
            })
        };
        if !scrollers.iter().all(near) {
            return true;
        }
        if let Some(cover) = child
            .child()
            .and_then(|c| c.first_child())
            .and_downcast::<gtk::Image>()
        {
            images::show(&cover, pick(&card.covers, CARD, &cover));
        }
        false
    });
}

/// Whether a span from `start`, `length` long, comes within `margin` of a
/// view `0..view` along the same axis.
fn overlaps(start: f32, length: f32, view: f32, margin: f32) -> bool {
    start < view + margin && start + length > -margin
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_near_the_view_count_as_in_it() {
        // a 160 px card in a 600 px view, one card's margin either side
        assert!(overlaps(0.0, 160.0, 600.0, 160.0));
        assert!(overlaps(700.0, 160.0, 600.0, 160.0)); // just past the end
        assert!(!overlaps(760.0, 160.0, 600.0, 160.0)); // a card further
        assert!(overlaps(-300.0, 160.0, 600.0, 160.0)); // scrolled just past
        assert!(!overlaps(-320.0, 160.0, 600.0, 160.0));
    }
}
