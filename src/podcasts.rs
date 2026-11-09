//! Podcasts in the window: a show's page, the episode rows every list uses,
//! the Podcasts and Your Episodes places, and the podcast parts of Home and
//! Search. The rules (new, played, where to go on) are `descant_core::podcast`.

use std::rc::Rc;

use adw::prelude::*;
use descant_core::{
    podcast::{self, Date, Progress},
    supervisor::Command,
};
use descant_spotify::podcasts::{self, Episode, ShowSummary};
use gtk::glib;

use crate::{
    grid::{self, Card},
    images,
    nav::Nav,
    runtime,
    widgets::{cover, cover_image, pick, section_title, song_list, with_header},
};

const SHOW_COVER: i32 = 128;
const EPISODE_COVER: i32 = 56;
const CARD_COVER: i32 = 64;
/// Episodes read at a time on a show's page: a screenful and a bit (each is
/// ~7 KB of answer and a cover of its own).
const PAGE: u32 = 20;
/// Home reads the newest few episodes of this many followed shows, one
/// request each.
const HOME_SHOWS: usize = 8;
const PER_SHOW: u32 = 3;
/// How long Home keeps those: new episodes come a few times a day at most.
const HOME_KEPT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Today, in the listener's own time zone.
fn today() -> Date {
    glib::DateTime::now_local()
        .map(|t| (t.year(), t.month() as u32, t.day_of_month() as u32))
        .unwrap_or((1970, 1, 1))
}

/// What a row says under the title: "Today · 35 min · 23 min left", with the
/// show first where the list mixes shows.
pub fn meta_line(e: &Episode, today: Date, with_show: bool) -> String {
    let mut parts = Vec::new();
    if with_show && !e.show.is_empty() {
        parts.push(e.show.clone());
    }
    if let Some(released) = e.released {
        parts.push(podcast::released_label(released, today));
    }
    parts.push(podcast::duration_label(e.duration_ms));
    match e.progress {
        Progress::InProgress { left_ms, .. } => parts.push(podcast::left_label(left_ms)),
        Progress::Played => parts.push("Played".into()),
        Progress::NotStarted
            if e.released
                .is_some_and(|d| podcast::is_new(e.progress, d, today)) =>
        {
            parts.push("New".into())
        }
        Progress::NotStarted => {}
    }
    parts.join(" · ")
}

/// An episode to start at, in the list it plays on in.
type Start = (Rc<Vec<Episode>>, usize);

/// Plays `list` from `index`, where that episode was left off.
fn play_from(nav: &Nav, list: &[Episode], index: usize) {
    nav.send(Command::PlayTracks {
        tracks: list.iter().map(|e| e.uri.clone()).collect(),
        start: index,
        position_ms: podcast::start_at(list[index].progress),
    });
}

/// An episode in a boxed list: the cover, the title over when and how long,
/// a bar for how far along, a button for Your Episodes and one to play. The
/// row plays too.
pub fn episode_row(
    nav: &Nav,
    list: &Rc<Vec<Episode>>,
    index: usize,
    with_show: bool,
) -> adw::ActionRow {
    let e = &list[index];
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&e.name))
        .subtitle(glib::markup_escape_text(&meta_line(e, today(), with_show)))
        .title_lines(1)
        .subtitle_lines(1)
        .activatable(true)
        .build();
    row.add_prefix(&cover(&e.covers, EPISODE_COVER, "cover"));
    if matches!(e.progress, Progress::InProgress { .. }) {
        row.add_suffix(
            &gtk::ProgressBar::builder()
                .fraction(podcast::fraction(e.progress, e.duration_ms))
                .width_request(60)
                .valign(gtk::Align::Center)
                .build(),
        );
    }
    row.add_suffix(&save_button(nav, &e.uri));
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Play")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    row.add_suffix(&play);
    let (n, l) = (nav.clone(), list.clone());
    play.connect_clicked(move |_| play_from(&n, &l, index));
    let (n, l) = (nav.clone(), list.clone());
    row.connect_activated(move |_| play_from(&n, &l, index));
    row
}

/// Saves an episode to Your Episodes, or takes it out; shows which it is.
fn save_button(nav: &Nav, uri: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let show = |button: &gtk::Button, saved: bool| {
        if saved {
            button.set_icon_name("user-bookmarks-symbolic");
            button.add_css_class("accent");
            button.set_tooltip_text(Some("Remove from Your Episodes"));
        } else {
            button.set_icon_name("bookmark-new-symbolic");
            button.remove_css_class("accent");
            button.set_tooltip_text(Some("Save to Your Episodes"));
        }
    };
    show(&button, nav.library.saved_episodes.borrow().contains(uri));
    let (nav, uri) = (nav.clone(), uri.to_string());
    button.connect_clicked(move |button| {
        let saved = !nav.library.saved_episodes.borrow().contains(&uri);
        let (nav, uri, button) = (nav.clone(), uri.clone(), button.clone());
        glib::spawn_future_local(async move {
            if nav.set_saved_episode(uri, saved).await {
                show(&button, saved);
            }
        });
    });
    button
}

/// A show: cover, name and maker, Play and Follow, what it is about, then
/// its episodes, newest first, a page at a time.
pub fn show_page(nav: &Nav, uri: &str, name: &str) -> adw::NavigationPage {
    let art = cover_image(SHOW_COVER, "cover-large");
    let kicker = gtk::Label::builder()
        .label("PODCAST")
        .xalign(0.0)
        .css_classes(["caption-heading", "dim-label"])
        .build();
    let heading = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["title-1"])
        .build();
    let maker = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text("Play")
        .css_classes(["circular", "suggested-action", "play-large"])
        .sensitive(false)
        .build();
    let follow = gtk::Button::builder()
        .label("Follow")
        .css_classes(["pill"])
        .sensitive(false)
        .build();
    let buttons = gtk::Box::builder().spacing(10).margin_top(6).build();
    buttons.append(&play);
    buttons.append(&follow);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::End)
        .build();
    text.append(&kicker);
    text.append(&heading);
    text.append(&maker);
    text.append(&buttons);
    let header = gtk::Box::builder().spacing(18).build();
    header.append(&art);
    header.append(&text);
    let about = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .lines(4)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label"])
        .visible(false)
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    body.append(&header);
    body.append(&about);
    body.append(&section_title("Episodes"));
    let rows = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    body.append(&rows);
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .build();
    body.append(&spinner);
    let more = gtk::Button::builder()
        .label("Show More")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .visible(false)
        .build();
    body.append(&more);

    let scroller = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let page = adw::NavigationPage::builder()
        .title(name)
        .child(&with_header(&scroller))
        .build();

    // The maker and the number of episodes arrive from two requests.
    let line: Rc<std::cell::RefCell<(String, Option<u32>)>> = Rc::default();
    let show_line = {
        let (maker, line) = (maker.clone(), line.clone());
        move || {
            let (publisher, count) = &*line.borrow();
            let count = count.map(podcast::episode_count_label);
            maker.set_label(&match (publisher.is_empty(), count) {
                (false, Some(count)) => format!("{publisher} · {count}"),
                (true, Some(count)) => count,
                _ => publisher.clone(),
            });
        }
    };

    // The episode Play starts: the first left off, else the newest.
    let first: Rc<std::cell::RefCell<Option<Start>>> = Rc::default();
    let (n, f) = (nav.clone(), first.clone());
    play.connect_clicked(move |_| {
        if let Some((list, index)) = &*f.borrow() {
            play_from(&n, list, *index);
        }
    });

    let (n, u) = (nav.clone(), uri.to_string());
    follow.connect_clicked(move |button| {
        let followed = !n.library.followed_shows.borrow().contains(&u);
        let (n, u, button) = (n.clone(), u.clone(), button.clone());
        glib::spawn_future_local(async move {
            if n.set_followed(u, followed).await {
                show_followed(&button, followed);
            }
        });
    });

    // The show itself.
    let (n, u, weak_page) = (nav.clone(), uri.to_string(), page.downgrade());
    let (shown, refresh) = (line.clone(), show_line.clone());
    glib::spawn_future_local(async move {
        let session = n.session();
        let found = runtime()
            .spawn(async move { podcasts::show(&session, &u).await })
            .await;
        let Ok(Ok(show)) = found else {
            log::warn!("could not load the show");
            return;
        };
        images::show(&art, pick(&show.show.covers, SHOW_COVER, &art));
        if heading.label().is_empty() {
            heading.set_label(&show.show.name);
            if let Some(page) = weak_page.upgrade() {
                page.set_title(&show.show.name);
            }
        }
        shown.borrow_mut().0 = show.show.publisher.clone();
        refresh();
        if !show.description.is_empty() {
            about.set_label(&show.description);
            about.set_visible(true);
        }
        n.podcast_state().await;
        if show.followed {
            n.library
                .followed_shows
                .borrow_mut()
                .insert(show.show.uri.clone());
        }
        show_followed(
            &follow,
            n.library.followed_shows.borrow().contains(&show.show.uri),
        );
        follow.set_sensitive(true);
    });

    // Its episodes, a page at a time. Show More's own handler keeps the
    // loader, so the loader is handed the button instead of holding it: a
    // button its own handler holds is never freed, nor the page around it.
    // Where the next page starts, once one is known.
    let next_page: Rc<std::cell::Cell<Option<u32>>> = Rc::default();
    let load = {
        let (nav, uri, next_page) = (nav.clone(), uri.to_string(), next_page.clone());
        move |more: &gtk::Button, offset: u32| {
            let (nav, uri, rows, spinner, more, first, play, line, refresh, next_page) = (
                nav.clone(),
                uri.clone(),
                rows.clone(),
                spinner.clone(),
                more.clone(),
                first.clone(),
                play.clone(),
                line.clone(),
                show_line.clone(),
                next_page.clone(),
            );
            more.set_visible(false);
            spinner.set_visible(true);
            glib::spawn_future_local(async move {
                let session = nav.session();
                let found = runtime()
                    .spawn(async move { podcasts::episodes(&session, &uri, offset, PAGE).await })
                    .await;
                spinner.set_visible(false);
                let Ok(Ok(found)) = found else {
                    log::warn!("could not load the episodes");
                    return;
                };
                if offset == 0 {
                    line.borrow_mut().1 = Some(found.total);
                    refresh();
                }
                let list = Rc::new(found.episodes);
                if first.borrow().is_none() && !list.is_empty() {
                    let index = list
                        .iter()
                        .position(|e| matches!(e.progress, Progress::InProgress { .. }))
                        .unwrap_or(0);
                    *first.borrow_mut() = Some((list.clone(), index));
                    play.set_sensitive(true);
                }
                let boxed = song_list();
                for index in 0..list.len() {
                    boxed.append(&episode_row(&nav, &list, index, false));
                }
                rows.append(&boxed);
                next_page.set(found.next.filter(|n| *n < found.total));
                more.set_visible(next_page.get().is_some());
            });
        }
    };
    load(&more, 0);
    more.connect_clicked(move |more| {
        if let Some(next) = next_page.take() {
            load(more, next);
        }
    });
    page
}

fn show_followed(button: &gtk::Button, followed: bool) {
    if followed {
        button.set_label("Following");
        button.set_tooltip_text(Some("Unfollow"));
    } else {
        button.set_label("Follow");
        button.set_tooltip_text(None);
    }
}

/// The Podcasts place: the shows followed.
pub fn podcasts_page(nav: &Nav) -> adw::NavigationPage {
    let session = nav.session();
    grid::grid_page(nav, "Podcasts", "No Followed Podcasts", async move {
        let shows = podcasts::followed(&session).await.ok()?;
        Some(shows.iter().map(Card::show).collect())
    })
}

/// The Your Episodes place: episodes saved for later, newest saved first.
pub fn saved_page(nav: &Nav) -> adw::NavigationPage {
    let stack = gtk::Stack::new();
    stack.add_named(
        &adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .build(),
        Some("loading"),
    );
    stack.add_named(
        &adw::StatusPage::builder()
            .title("No Saved Episodes")
            .description("Save an episode to listen to it later")
            .icon_name("user-bookmarks-symbolic")
            .build(),
        Some("empty"),
    );
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    stack.add_named(
        &gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some("list"),
    );
    let page = adw::NavigationPage::builder()
        .title("Your Episodes")
        .child(&with_header(&stack))
        .build();
    let nav = nav.clone();
    glib::spawn_future_local(async move {
        let session = nav.session();
        let found = runtime()
            .spawn(async move { podcasts::saved_episodes(&session).await })
            .await;
        match found {
            Ok(Ok(episodes)) if !episodes.is_empty() => {
                nav.library
                    .saved_episodes
                    .borrow_mut()
                    .extend(episodes.iter().map(|e| e.uri.clone()));
                let list = Rc::new(episodes);
                let boxed = song_list();
                for index in 0..list.len() {
                    boxed.append(&episode_row(&nav, &list, index, true));
                }
                body.append(&boxed);
                stack.set_visible_child_name("list");
            }
            _ => stack.set_visible_child_name("empty"),
        }
    });
    page
}

/// The podcast parts of search: shows as cards, then episodes.
pub fn search_results(nav: &Nav, results: &gtk::Box, shows: &[ShowSummary], episodes: &[Episode]) {
    if !shows.is_empty() {
        results.append(&section_title("Podcasts"));
        let cards: Vec<Card> = shows.iter().map(Card::show).collect();
        results.append(&grid::flow(nav, &cards));
    }
    if !episodes.is_empty() {
        results.append(&section_title("Episodes"));
        let list = Rc::new(episodes.to_vec());
        let boxed = song_list();
        for index in 0..list.len() {
            boxed.append(&episode_row(nav, &list, index, true));
        }
        results.append(&boxed);
    }
}

/// Home's Continue listening (episodes left off) and New episodes (new and
/// unheard), from the newest episodes of the first followed shows.
pub fn home_shelves(nav: &Nav, continuing: &gtk::Box, new: &gtk::Box) {
    let (nav, continuing, new) = (nav.clone(), continuing.clone(), new.clone());
    glib::spawn_future_local(async move {
        // Nothing followed, nothing to read: one cheap look, once per login.
        nav.podcast_state().await;
        if nav.library.followed_shows.borrow().is_empty() {
            return;
        }
        let episodes = match nav.library.home.episodes.get(HOME_KEPT) {
            Some(kept) => kept,
            None => {
                let session = nav.session();
                let read = runtime()
                    .spawn(async move {
                        let shows = podcasts::followed(&session).await.ok()?;
                        let first = &shows[..shows.len().min(HOME_SHOWS)];
                        Some(podcasts::latest(&session, first, PER_SHOW).await)
                    })
                    .await;
                let Ok(Some(episodes)) = read else { return };
                let kept = Rc::new(episodes);
                nav.library.home.episodes.set(kept.clone());
                kept
            }
        };
        let today = today();
        let mut left_off: Vec<Episode> = episodes
            .iter()
            .filter(|e| matches!(e.progress, Progress::InProgress { .. }))
            .cloned()
            .collect();
        left_off.sort_by_key(|e| std::cmp::Reverse(e.released));
        left_off.truncate(4);
        if !left_off.is_empty() {
            continuing.append(&continue_cards(&nav, Rc::new(left_off)));
            continuing.set_visible(true);
        }
        let mut fresh: Vec<Episode> = episodes
            .iter()
            .filter(|e| {
                e.released
                    .is_some_and(|d| podcast::is_new(e.progress, d, today))
            })
            .cloned()
            .collect();
        fresh.sort_by_key(|e| std::cmp::Reverse(e.released));
        fresh.truncate(5);
        if !fresh.is_empty() {
            let list = Rc::new(fresh);
            let boxed = song_list();
            for index in 0..list.len() {
                boxed.append(&episode_row(&nav, &list, index, true));
            }
            new.append(&boxed);
            new.set_visible(true);
        }
    });
}

/// Episodes left off, as small cards: cover, title, show, and what is left.
fn continue_cards(nav: &Nav, list: Rc<Vec<Episode>>) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(12)
        .row_spacing(12)
        .build();
    for e in list.iter() {
        let card = gtk::Box::builder()
            .spacing(12)
            .css_classes(["card", "continue-card"])
            .build();
        card.append(&cover(&e.covers, CARD_COVER, "cover"));
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        text.append(
            &gtk::Label::builder()
                .label(&e.name)
                .xalign(0.0)
                .wrap(true)
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["heading"])
                .build(),
        );
        text.append(
            &gtk::Label::builder()
                .label(&e.show)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        let progress = gtk::Box::builder().spacing(8).build();
        progress.append(
            &gtk::ProgressBar::builder()
                .fraction(podcast::fraction(e.progress, e.duration_ms))
                .valign(gtk::Align::Center)
                .hexpand(true)
                .build(),
        );
        if let Progress::InProgress { left_ms, .. } = e.progress {
            progress.append(
                &gtk::Label::builder()
                    .label(podcast::left_label(left_ms))
                    .css_classes(["dim-label", "caption", "numeric"])
                    .build(),
            );
        }
        text.append(&progress);
        card.append(&text);
        flow.append(&card);
    }
    let nav = nav.clone();
    flow.connect_child_activated(move |_, child| {
        let index = child.index() as usize;
        if index < list.len() {
            play_from(&nav, &list, index);
        }
    });
    flow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(progress: Progress, released: Date) -> Episode {
        Episode {
            uri: "spotify:episode:a".into(),
            name: "A Landslide Victory for Democrats".into(),
            show: "The Daily".into(),
            show_uri: "spotify:show:s".into(),
            duration_ms: 2_079_973,
            released: Some(released),
            covers: vec![],
            progress,
        }
    }

    #[test]
    fn a_row_says_when_how_long_and_how_far() {
        let today = (2026, 11, 4);
        let left_off = Progress::InProgress {
            position_ms: 724_000,
            left_ms: 1_355_973,
        };
        assert_eq!(
            meta_line(&episode(left_off, (2026, 11, 4)), today, false),
            "Today · 35 min · 23 min left"
        );
        assert_eq!(
            meta_line(&episode(Progress::NotStarted, (2026, 11, 3)), today, true),
            "The Daily · Yesterday · 35 min · New"
        );
        assert_eq!(
            meta_line(&episode(Progress::Played, (2026, 10, 31)), today, false),
            "Oct 31 · 35 min · Played"
        );
        assert_eq!(
            meta_line(&episode(Progress::NotStarted, (2026, 9, 1)), today, false),
            "Sep 1 · 35 min",
            "not new any more"
        );
    }
}
