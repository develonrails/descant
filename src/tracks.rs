//! A page of tracks: a playlist, Liked Songs or an album.
//!
//! Header with cover, title and a Play button over a virtualised list: rows
//! exist only for what is on screen, and their thumbnails come through the
//! image cache at the size shown. The list is the scrolled window's own
//! child (inside a box it would build a row for every track) and the header
//! rides along as the header of its one section. Further pages load when
//! the end of the list comes near, not all at once. The playing track is marked; a right-click
//! opens a row's menu (queue, like, playlists, artist, album).

use std::rc::Rc;

use adw::prelude::*;
use descant_core::track::{Cover, TrackInfo};
use descant_spotify::library;
use gtk::{gio, glib};
use tokio::sync::mpsc;

use crate::{
    images,
    nav::{Destination, Nav, track_uri},
    runtime,
    widgets::{THUMB, cover_image, format_duration, pick, with_header},
};

/// The cover in the header, in logical pixels.
const HEADER_COVER: i32 = 128;
/// Rows per playlist request; one extended-metadata batch each.
const PAGE: u32 = 100;

/// Where the tracks come from.
#[derive(Clone)]
pub enum Source {
    Playlist {
        uri: String,
    },
    Liked,
    Album {
        uri: String,
    },
    /// Up Next: rows play by skipping ahead to them.
    Queue {
        uris: Vec<String>,
    },
}

impl Source {
    fn context(&self, nav: &Nav) -> String {
        match self {
            Source::Playlist { uri } | Source::Album { uri } => uri.clone(),
            Source::Liked => format!("spotify:user:{}:collection", nav.session().username()),
            Source::Queue { .. } => String::new(),
        }
    }
}

pub struct TrackPage {
    pub page: adw::NavigationPage,
    store: gio::ListStore,
    list: gtk::ListView,
}

impl TrackPage {
    /// The rows themselves. A track change has to reach the rows that are
    /// already on screen, and the model cannot do that: asking it to report
    /// the same items again leaves GTK with the same objects and nothing to
    /// rebind.
    pub fn list(&self) -> &gtk::ListView {
        &self.list
    }
}

impl TrackPage {
    /// Builds the page and starts loading it.
    pub fn new(nav: &Nav, source: Source, title: &str, covers: &[Cover]) -> Rc<TrackPage> {
        let album_page = matches!(source, Source::Album { .. });
        let cover = cover_image(HEADER_COVER, "cover-large");
        images::show(&cover, pick(covers, HEADER_COVER, &cover));
        let heading = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-1"])
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        let play = gtk::Button::builder()
            .label("Play")
            .halign(gtk::Align::Start)
            .css_classes(["pill", "suggested-action"])
            .build();
        // Up Next has no context of its own to play.
        play.set_visible(!matches!(source, Source::Queue { .. }));
        let spinner = adw::Spinner::builder()
            .width_request(16)
            .height_request(16)
            .build();
        let buttons = gtk::Box::builder().spacing(12).build();
        buttons.append(&play);
        buttons.append(&spinner);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::End)
            .build();
        text.append(&heading);
        text.append(&subtitle);
        text.append(&buttons);
        let header = gtk::Box::builder()
            .spacing(18)
            .margin_top(18)
            .margin_bottom(12)
            .margin_start(18)
            .margin_end(18)
            .build();
        header.append(&cover);
        header.append(&text);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let menu = Rc::new(MenuContext {
            nav: nav.clone(),
            this_playlist: match &source {
                Source::Playlist { uri } if nav.own_playlists().iter().any(|(u, _)| u == uri) => {
                    Some(uri.clone())
                }
                _ => None,
            },
        });
        let factory = gtk::SignalListItemFactory::new();
        let rows_menu = menu.clone();
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let row = TrackRow::new(!album_page).root;
            add_menu(&row, &rows_menu);
            item.set_child(Some(&row));
        });
        let current = nav.current.clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
            let (Some(row), Some(object)) = (item.child(), item.item()) else {
                return;
            };
            let object = object
                .downcast::<glib::BoxedAnyObject>()
                .expect("track object");
            let track = object.borrow::<TrackInfo>();
            let playing = *current.borrow() == track.uri;
            TrackRow::bind(&row, item.position() + 1, &track, playing);
        });
        // One section holding every track, so the header can be its header.
        let sections = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
        sections.set_section_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
        let headers = gtk::SignalListItemFactory::new();
        headers.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListHeader>().expect("list header");
            if header.parent().is_none() {
                item.set_child(Some(&header));
            }
        });
        headers.connect_teardown(|_, item| {
            let item = item.downcast_ref::<gtk::ListHeader>().expect("list header");
            item.set_child(None::<&gtk::Widget>);
        });
        let list = gtk::ListView::builder()
            .model(&gtk::NoSelection::new(Some(sections)))
            .factory(&factory)
            .header_factory(&headers)
            .css_classes(["navigation-sidebar", "track-list"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        // Two screens before the end, ask the loader for the next page.
        let (wants_more, more) = mpsc::unbounded_channel::<()>();
        let near_end = move |a: &gtk::Adjustment| {
            if a.value() + 2.0 * a.page_size() >= a.upper() {
                let _ = wants_more.send(());
            }
        };
        let adjustment = scroller.vadjustment();
        adjustment.connect_value_changed(near_end.clone());
        adjustment.connect_changed(near_end);
        let page = adw::NavigationPage::builder()
            .title(title)
            .child(&with_header(&scroller))
            .build();

        let view = Rc::new(TrackPage { page, store, list });
        let context = source.context(nav);
        let n = nav.clone();
        let ctx = context.clone();
        play.connect_clicked(move |_| n.play(&ctx, None));
        let queue = matches!(source, Source::Queue { .. });
        let (n, store) = (nav.clone(), view.store.downgrade());
        view.list.connect_activate(move |_, position| {
            if queue {
                // the n-th upcoming track is n + 1 skips away
                n.send(descant_core::supervisor::Command::Skip(position as i32 + 1));
                return;
            }
            let item = store.upgrade().and_then(|s| s.item(position));
            if let Some(object) = item.and_downcast::<glib::BoxedAnyObject>() {
                n.play(&context, Some(&object.borrow::<TrackInfo>().uri));
            }
        });
        let liked_page = matches!(source, Source::Liked);
        view.list.insert_action_group(
            "track",
            Some(&track_actions(
                nav,
                &view.store,
                menu.this_playlist.clone(),
                liked_page,
            )),
        );
        view.load(
            nav.clone(),
            source,
            Header {
                heading,
                cover,
                subtitle,
                spinner,
            },
            more,
        );
        view
    }

    fn load(
        self: &Rc<Self>,
        nav: Nav,
        source: Source,
        header: Header,
        mut more: mpsc::UnboundedReceiver<()>,
    ) {
        // The list model lives exactly as long as the page is shown; the
        // TrackPage value itself is not kept (closures hold nothing strongly).
        let store = self.store.downgrade();
        let page = self.page.downgrade();
        let Header {
            heading,
            cover,
            subtitle,
            spinner,
        } = header;
        // A pasted link opens a page without a name: take Spotify's.
        let name_if_missing = move |name: &str| {
            if heading.label().is_empty() {
                heading.set_label(name);
                if let Some(page) = page.upgrade() {
                    page.set_title(name);
                }
            }
        };
        glib::spawn_future_local(async move {
            let session = nav.session();
            match source {
                Source::Playlist { uri } => {
                    let mut offset = 0;
                    loop {
                        if offset > 0 && !end_is_near(&mut more).await {
                            return;
                        }
                        let (s, u) = (session.clone(), uri.clone());
                        let page = runtime()
                            .spawn(async move { library::playlist(&s, &u, offset, PAGE).await })
                            .await;
                        let Some(store) = store.upgrade() else { return };
                        let Ok(Ok(page)) = page else {
                            log::warn!("could not load playlist {uri}");
                            break;
                        };
                        if offset == 0 {
                            name_if_missing(&page.name);
                            subtitle.set_label(&playlist_line(&page.owner, page.total));
                            // Mosaic playlists list no cover: use the first album's.
                            if cover.widget_name().is_empty() {
                                let covers = match page.tracks.first() {
                                    _ if !page.covers.is_empty() => &page.covers,
                                    Some(first) => &first.covers,
                                    None => &page.covers,
                                };
                                images::show(&cover, pick(covers, HEADER_COVER, &cover));
                            }
                        }
                        append(&store, page.tracks);
                        spinner.set_visible(false);
                        offset += PAGE;
                        if offset >= page.total {
                            break;
                        }
                    }
                }
                Source::Liked => {
                    // Liked tracks, newest first (read at login, shared, not
                    // copied), described a hundred at a time.
                    let known = nav.library.liked_songs.borrow().clone();
                    let ids = match known {
                        Some(ids) => ids,
                        None => {
                            let s = session.clone();
                            match runtime()
                                .spawn(async move { library::collection(&s).await })
                                .await
                            {
                                Ok(Ok(collection)) => {
                                    nav.library.set_collection(collection);
                                    nav.library.liked_songs.borrow().clone().unwrap_or_default()
                                }
                                _ => {
                                    log::warn!("could not load Liked Songs");
                                    spinner.set_visible(false);
                                    return;
                                }
                            }
                        }
                    };
                    subtitle.set_label(&songs(ids.len() as u32));
                    for (i, chunk) in ids.chunks(PAGE as usize).enumerate() {
                        if i > 0 && !end_is_near(&mut more).await {
                            return;
                        }
                        let chunk: Vec<String> = chunk.iter().map(track_uri).collect();
                        let s = session.clone();
                        let tracks = runtime()
                            .spawn(async move { library::tracks(&s, &chunk).await })
                            .await;
                        let Some(store) = store.upgrade() else { return };
                        let Ok(Ok(tracks)) = tracks else { break };
                        if i == 0 {
                            let first = tracks.first().map(|t| t.covers.clone());
                            images::show(
                                &cover,
                                pick(&first.unwrap_or_default(), HEADER_COVER, &cover),
                            );
                        }
                        append(&store, tracks);
                        spinner.set_visible(false);
                    }
                }
                Source::Queue { uris } => {
                    let s = session.clone();
                    let found = runtime()
                        .spawn(async move { library::tracks(&s, &uris).await })
                        .await;
                    let Some(store) = store.upgrade() else { return };
                    if let Ok(Ok(tracks)) = found {
                        subtitle.set_label(&songs(tracks.len() as u32));
                        append(&store, tracks);
                    }
                }
                Source::Album { uri } => {
                    let (s, u) = (session.clone(), uri.clone());
                    let page = runtime()
                        .spawn(async move { library::album(&s, &u).await })
                        .await;
                    let Some(store) = store.upgrade() else { return };
                    match page {
                        Ok(Ok(page)) => {
                            name_if_missing(&page.album.name);
                            if cover.widget_name().is_empty() {
                                images::show(
                                    &cover,
                                    pick(&page.album.covers, HEADER_COVER, &cover),
                                );
                            }
                            let artists = page.album.artists.join(", ");
                            let year = page
                                .album
                                .year
                                .map(|y| format!(" · {y}"))
                                .unwrap_or_default();
                            subtitle.set_label(&format!(
                                "{artists}{year} · {}",
                                songs(page.tracks.len() as u32)
                            ));
                            if let Some(first) = page.album.artists.first().cloned() {
                                link_artist(&subtitle, &nav, &page, &first);
                            }
                            append(&store, page.tracks);
                        }
                        _ => log::warn!("could not load album {uri}"),
                    }
                }
            }
            spinner.set_visible(false);
        });
    }
}

/// Waits until the end of the list comes near; false once the page is gone.
/// Signals from before the last page arrived are stale and skipped.
async fn end_is_near(more: &mut mpsc::UnboundedReceiver<()>) -> bool {
    while more.try_recv().is_ok() {}
    more.recv().await.is_some()
}

/// The header widgets the loader fills in.
struct Header {
    heading: gtk::Label,
    cover: gtk::Image,
    subtitle: gtk::Label,
    spinner: adw::Spinner,
}

fn append(store: &gio::ListStore, tracks: Vec<TrackInfo>) {
    let objects: Vec<glib::BoxedAnyObject> =
        tracks.into_iter().map(glib::BoxedAnyObject::new).collect();
    store.extend_from_slice(&objects);
}

/// What a row's menu needs to know.
struct MenuContext {
    nav: Nav,
    /// The user's own playlist this page shows, if it is one.
    this_playlist: Option<String>,
}

/// A row's widget name is `<track uri>#<position>`; the menu's actions take
/// it as their target.
fn split_target(target: &str) -> (&str, usize) {
    match target.rsplit_once('#') {
        Some((uri, position)) => (uri, position.parse().unwrap_or(0)),
        None => (target, 0),
    }
}

/// Right-click (or long-press) a row for its menu.
fn add_menu(row: &gtk::Box, context: &Rc<MenuContext>) {
    let open = |row: &gtk::Box, context: &MenuContext, x: f64, y: f64| {
        let target = row.widget_name().to_string();
        let (uri, _) = split_target(&target);
        if !uri.starts_with("spotify:track:") {
            return;
        }
        let item = |label: &str, action: &str, value: &str| {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&value.to_variant()));
            item
        };
        let menu = gio::Menu::new();
        let first = gio::Menu::new();
        first.append_item(&item("Add to Queue", "track.queue", &target));
        let liked = context.nav.library.is_liked(uri);
        first.append_item(&item(
            if liked {
                "Remove from Liked Songs"
            } else {
                "Save to Liked Songs"
            },
            "track.like",
            &target,
        ));
        let playlists = gio::Menu::new();
        for (playlist, name) in context.nav.own_playlists() {
            if Some(&playlist) != context.this_playlist.as_ref() {
                playlists.append_item(&item(&name, "track.add-to", &format!("{playlist} {uri}")));
            }
        }
        if playlists.n_items() > 0 {
            first.append_submenu(Some("Add to Playlist"), &playlists);
        }
        if context.this_playlist.is_some() {
            first.append_item(&item("Remove from This Playlist", "track.remove", &target));
        }
        menu.append_section(None, &first);
        let go = gio::Menu::new();
        go.append_item(&item("Go to Artist", "track.artist", &target));
        go.append_item(&item("Go to Album", "track.album", &target));
        menu.append_section(None, &go);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(row);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    };
    // The gestures reach their row through the gesture: a clone of the row in
    // its own controller would keep both alive forever.
    let row_of = |g: &gtk::Gesture| g.widget().and_downcast::<gtk::Box>();
    let click = gtk::GestureClick::builder()
        .button(gtk::gdk::BUTTON_SECONDARY)
        .build();
    let c = context.clone();
    click.connect_pressed(move |g, _, x, y| {
        if let Some(row) = row_of(g.upcast_ref()) {
            open(&row, &c, x, y)
        }
    });
    row.add_controller(click);
    let press = gtk::GestureLongPress::new();
    let c = context.clone();
    press.connect_pressed(move |g, x, y| {
        if let Some(row) = row_of(g.upcast_ref()) {
            open(&row, &c, x, y)
        }
    });
    row.add_controller(press);
}

/// What the row menu does. Targets are `<uri>#<position>`, or
/// `<playlist uri> <track uri>` for adding to a playlist.
fn track_actions(
    nav: &Nav,
    store: &gio::ListStore,
    this_playlist: Option<String>,
    liked_page: bool,
) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let weak_store = store.downgrade();
    let find = {
        let store = weak_store.clone();
        move |target: &str| -> Option<TrackInfo> {
            let (uri, position) = split_target(target);
            let object = store
                .upgrade()?
                .item(position as u32)
                .and_downcast::<glib::BoxedAnyObject>()?;
            let track = object.borrow::<TrackInfo>();
            (track.uri == uri).then(|| track.clone())
        }
    };
    let action = |name: &str, f: Box<dyn Fn(String)>| {
        let a = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        a.connect_activate(move |_, target| {
            if let Some(target) = target.and_then(|t| t.get::<String>()) {
                f(target);
            }
        });
        group.add_action(&a);
    };

    let n = nav.clone();
    action(
        "queue",
        Box::new(move |target| {
            let (uri, _) = split_target(&target);
            n.send(descant_core::supervisor::Command::AddToQueue(
                uri.to_string(),
            ));
            n.toast("Added to the queue");
        }),
    );

    let (n, store) = (nav.clone(), weak_store.clone());
    action(
        "like",
        Box::new(move |target| {
            let (uri, position) = split_target(&target);
            let (uri, liked) = (uri.to_string(), !n.library.is_liked(uri));
            let (n, store) = (n.clone(), store.clone());
            glib::spawn_future_local(async move {
                // On Liked Songs, an unliked track leaves the list.
                if n.set_liked(uri, liked).await
                    && liked_page
                    && !liked
                    && let Some(store) = store.upgrade()
                {
                    store.remove(position as u32);
                }
            });
        }),
    );

    let n = nav.clone();
    action(
        "add-to",
        Box::new(move |target| {
            let Some((playlist, track)) = target.split_once(' ') else {
                return;
            };
            let name = n
                .own_playlists()
                .into_iter()
                .find(|(u, _)| u == playlist)
                .map(|(_, name)| name)
                .unwrap_or_default();
            let (n, playlist, track) = (n.clone(), playlist.to_string(), track.to_string());
            glib::spawn_future_local(async move {
                let s = n.session();
                let tracks = vec![track];
                match runtime()
                    .spawn(async move {
                        descant_spotify::edit::add_to_playlist(&s, &playlist, &tracks).await
                    })
                    .await
                {
                    Ok(Ok(())) => n.toast(format!("Added to {name}")),
                    _ => n.toast("Spotify did not take that change"),
                }
            });
        }),
    );

    let (n, store) = (nav.clone(), weak_store);
    action(
        "remove",
        Box::new(move |target| {
            let Some(playlist) = this_playlist.clone() else {
                return;
            };
            let (uri, position) = split_target(&target);
            let Some(rows) = store.upgrade() else { return };
            // Which copy of the track this row is, counting the rows above it.
            let occurrence = (0..position as u32)
                .filter_map(|i| rows.item(i).and_downcast::<glib::BoxedAnyObject>())
                .filter(|o| o.borrow::<TrackInfo>().uri == uri)
                .count();
            let (n, store, uri) = (n.clone(), store.clone(), uri.to_string());
            glib::spawn_future_local(async move {
                let s = n.session();
                let u = uri.clone();
                let done = runtime()
                    .spawn(async move {
                        descant_spotify::edit::remove_from_playlist(&s, &playlist, &u, occurrence)
                            .await
                    })
                    .await;
                match done {
                    Ok(Ok(())) => {
                        if let Some(store) = store.upgrade() {
                            store.remove(position as u32);
                        }
                        n.toast("Removed from the playlist");
                    }
                    Ok(Err(e)) if e.kind == librespot_core::error::ErrorKind::Aborted => {
                        n.toast("The playlist changed meanwhile; open it again and retry")
                    }
                    _ => n.toast("Spotify did not take that change"),
                }
            });
        }),
    );

    let (n, f) = (nav.clone(), find.clone());
    action(
        "artist",
        Box::new(move |target| {
            if let Some(t) = f(&target)
                && let (Some(artist), Some(name)) = (t.artist_uris.first(), t.artists.first())
            {
                n.open(Destination::Artist {
                    uri: artist.clone(),
                    name: name.clone(),
                });
            }
        }),
    );
    let n = nav.clone();
    action(
        "album",
        Box::new(move |target| {
            if let Some(t) = find(&target)
                && !t.album_uri.is_empty()
            {
                n.open(Destination::Album {
                    uri: t.album_uri.clone(),
                    name: t.album.clone(),
                });
            }
        }),
    );
    group
}

/// Clicking an album's artist line opens the (first) artist.
fn link_artist(label: &gtk::Label, nav: &Nav, page: &library::AlbumPage, name: &str) {
    let Some(uri) = page
        .tracks
        .iter()
        .find_map(|t| t.artist_uris.first().cloned())
    else {
        return;
    };
    let click = gtk::GestureClick::new();
    let (nav, name) = (nav.clone(), name.to_string());
    click.connect_released(move |_, _, _, _| {
        nav.open(Destination::Artist {
            uri: uri.clone(),
            name: name.clone(),
        })
    });
    label.add_controller(click);
    label.set_cursor_from_name(Some("pointer"));
    label.set_tooltip_text(Some("Open the artist"));
}

fn songs(n: u32) -> String {
    if n == 1 {
        "1 song".to_string()
    } else {
        format!("{n} songs")
    }
}

fn playlist_line(owner: &str, total: u32) -> String {
    let by = if owner == "spotify" { "Spotify" } else { owner };
    format!("{by} · {}", songs(total))
}

/// A row: number, thumbnail (not on album pages), title over artists, album,
/// duration. The playing track's title is accented.
struct TrackRow {
    root: gtk::Box,
}

impl TrackRow {
    fn new(with_thumbnail: bool) -> TrackRow {
        let number = gtk::Label::builder()
            .width_chars(3)
            .xalign(1.0)
            .css_classes(["dim-label", "numeric"])
            .build();
        let thumb = cover_image(THUMB, "cover");
        thumb.set_visible(with_thumbnail);
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["title"])
            .build();
        let artists = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        let names = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        names.append(&title);
        names.append(&artists);
        let album = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(18)
            .max_width_chars(28)
            .css_classes(["dim-label"])
            .visible(with_thumbnail)
            .build();
        let duration = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .build();
        let root = gtk::Box::builder()
            .spacing(12)
            .margin_top(2)
            .margin_bottom(2)
            .margin_start(6)
            .margin_end(12)
            .css_classes(["track-row"])
            .build();
        for widget in [
            number.upcast_ref::<gtk::Widget>(),
            thumb.upcast_ref(),
            names.upcast_ref(),
            album.upcast_ref(),
            duration.upcast_ref(),
        ] {
            root.append(widget);
        }
        TrackRow { root }
    }

    /// Fills a row built by [`TrackRow::new`]; its widgets in the order appended.
    fn bind(row: &gtk::Widget, number: u32, track: &TrackInfo, playing: bool) {
        row.set_widget_name(&format!("{}#{}", track.uri, number - 1));
        let number_label = row.first_child();
        let thumb = number_label.as_ref().and_then(|w| w.next_sibling());
        let names = thumb.as_ref().and_then(|w| w.next_sibling());
        let album = names.as_ref().and_then(|w| w.next_sibling());
        let duration = album.as_ref().and_then(|w| w.next_sibling());
        let title = names.as_ref().and_then(|w| w.first_child());
        let artists = title.as_ref().and_then(|w| w.next_sibling());
        let label = |w: Option<gtk::Widget>| w.and_downcast::<gtk::Label>().expect("label");

        label(number_label).set_label(&number.to_string());
        let title = label(title);
        title.set_label(&track.title);
        if playing {
            row.add_css_class("playing");
        } else {
            row.remove_css_class("playing");
        }
        label(artists).set_label(&track.artist_line());
        label(album).set_label(&track.album);
        label(duration).set_label(&format_duration(track.duration_ms));
        if let Some(thumb) = thumb.and_downcast::<gtk::Image>()
            && thumb.is_visible()
        {
            images::show(&thumb, pick(&track.covers, THUMB, &thumb));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_name_spotify_and_count_songs() {
        assert_eq!(playlist_line("spotify", 50), "Spotify · 50 songs");
        assert_eq!(playlist_line("1234567890", 1), "1234567890 · 1 song");
    }

    #[test]
    fn row_targets_carry_the_track_and_its_position() {
        assert_eq!(split_target("spotify:track:a#12"), ("spotify:track:a", 12));
        assert_eq!(split_target("spotify:track:a"), ("spotify:track:a", 0));
    }
}
