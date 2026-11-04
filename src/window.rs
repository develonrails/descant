//! The window: pairing with a phone, then the player.
//!
//! The player is laid out like Riff (Diegovsky/riff): a sidebar with Home,
//! Search, the library and the user's playlists; on the right a navigation
//! stack (a sidebar pick replaces it, an album or artist opens on top of it);
//! the now-playing bar across the bottom. Narrow windows collapse the sidebar.
//! Pages are built when opened and dropped when left.

use std::{cell::RefCell, rc::Rc, time::Duration};

use adw::prelude::*;
use descant_core::{
    supervisor::{Command, Notice, Status},
    track::TrackInfo,
    volume,
};
use descant_playback::{
    controller::{self, ControllerConfig, ControllerHandle, ResumePoint, Session, Update},
    equalizer::EqualizerSetting,
};
use descant_spotify::{library, pairing, store};
use gtk::{gio, glib};
use librespot_core::authentication::Credentials;
use tokio::sync::mpsc;

use crate::{
    artist,
    grid::{self, Card},
    home, images,
    mpris::{self, Mpris},
    nav::{Destination, Event, Library, Nav},
    qr::QrView,
    runtime, search,
    settings::{self, KEEP_PLAYING, VOLUME},
    tracks::{Source, TrackPage},
    widgets::{cover_image, pick, with_header},
};

/// Cover art in the now-playing bar and in the sidebar, in logical pixels.
const COVER_SIZE: i32 = 48;
const SIDEBAR_COVER: i32 = 32;

/// The fixed sidebar entries: id, label, icon.
const PLACES: &[(&str, &str, &str)] = &[
    ("home", "Home", "go-home-symbolic"),
    ("search", "Search", "system-search-symbolic"),
    ("liked", "Liked Songs", "starred-symbolic"),
    ("albums", "Albums", "media-optical-symbolic"),
    ("artists", "Artists", "avatar-default-symbolic"),
];

pub fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let ui = Ui::build(app);
    ui.window.present();
    ui.start();
}

struct Ui {
    app: adw::Application,
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    toasts: adw::ToastOverlay,
    // pairing
    qr: QrView,
    code: gtk::Label,
    open_link: gtk::Button,
    pairing_status: gtk::Label,
    // layout
    split: adw::NavigationSplitView,
    sidebar: gtk::ListBox,
    content: adw::NavigationView,
    home: adw::NavigationPage,
    // now playing
    cover: gtk::Image,
    title: gtk::Label,
    artist: gtk::Label,
    /// The artist line of the playing track; a status message may stand in for it.
    artist_text: RefCell<String>,
    /// Shown inside the play button while a track loads, so nothing moves.
    loading: adw::Spinner,
    play_pause: gtk::Button,
    // state
    controller: RefCell<Option<ControllerHandle>>,
    /// The session, liked tracks and the user's playlists, shared with every page.
    library: Rc<Library>,
    like: gtk::Button,
    shuffle: gtk::ToggleButton,
    repeat: gtk::Button,
    repeat_mode: std::cell::Cell<descant_core::supervisor::Repeat>,
    volume: gtk::Scale,
    mute: gtk::Button,
    /// The bar's top edge: where the track stands, and a way to seek.
    progress: gtk::Scale,
    time: gtk::Label,
    /// The playing track's length, for the edge's range and the clock.
    duration_ms: std::cell::Cell<u32>,
    /// Set while the pointer drags the edge, so the tick leaves it alone.
    seeking: std::cell::Cell<bool>,
    seek_pending: RefCell<Option<glib::SourceId>>,
    /// Set while the slider follows the engine, so that move is not sent back.
    volume_following: std::cell::Cell<bool>,
    volume_before_mute: std::cell::Cell<f64>,
    volume_pending: RefCell<Option<glib::SourceId>>,
    status: RefCell<Status>,
    current: Rc<RefCell<String>>,
    /// What comes after the current track.
    queue: RefCell<Vec<String>>,
    /// Track lists of open pages, to re-mark the playing track.
    track_lists: RefCell<Vec<glib::WeakRef<gio::ListStore>>>,
    inhibit: RefCell<Option<u32>>,
    pairing_url: RefCell<String>,
    blocked_shown: RefCell<bool>,
    mpris: RefCell<Option<Mpris>>,
    settings: gio::Settings,
    /// The equalizer curve the audio path plays with (`None`: off).
    equalizer: EqualizerSetting,
    /// Where playback stands, and since when, to save on the way out.
    resume: RefCell<Option<(ResumePoint, bool, std::time::Instant)>>,
    /// Lets pages reach the Ui without owning it.
    me: RefCell<std::rc::Weak<Ui>>,
}

impl Ui {
    fn build(app: &adw::Application) -> Rc<Ui> {
        let settings = settings::settings();
        // GNOME's usual three; logging out lives in Preferences.
        let items = gio::Menu::new();
        items.append(Some("_Preferences"), Some("win.preferences"));
        items.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
        items.append(Some("_About Descant"), Some("win.about"));
        let menu = gio::Menu::new();
        menu.append_section(None, &items);
        // Quit only when closing the window no longer stops Descant.
        show_quit(&menu, settings.boolean(KEEP_PLAYING));
        let m = menu.clone();
        settings.connect_changed(Some(KEEP_PLAYING), move |settings, key| {
            show_quit(&m, settings.boolean(key));
        });
        let framed = |child: &gtk::Widget, with_menu: bool| {
            let header = adw::HeaderBar::new();
            if with_menu {
                header.pack_end(
                    &gtk::MenuButton::builder()
                        .icon_name("open-menu-symbolic")
                        .menu_model(&menu)
                        .tooltip_text("Main Menu")
                        .primary(true)
                        .build(),
                );
            }
            let view = adw::ToolbarView::new();
            view.add_top_bar(&header);
            view.set_content(Some(child));
            view
        };

        // ---- pairing page ----
        let qr = QrView::new(220);
        let code = gtk::Label::builder()
            .css_classes(["title-1", "monospace"])
            .selectable(true)
            .build();
        let pairing_status = gtk::Label::builder()
            .label("Asking Spotify for a code…")
            .css_classes(["dim-label"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let open_link = gtk::Button::builder()
            .label("Open in Browser")
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .sensitive(false)
            .build();
        let manual = gtk::Label::builder()
            .label("No camera at hand? Enter the code at spotify.com/pair")
            .css_classes(["dim-label", "caption"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let pairing_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        pairing_box.append(
            &gtk::Frame::builder()
                .child(&qr.area)
                .halign(gtk::Align::Center)
                .css_classes(["card"])
                .build(),
        );
        pairing_box.append(&code);
        pairing_box.append(&pairing_status);
        pairing_box.append(&open_link);
        pairing_box.append(&manual);
        let pairing_page = adw::StatusPage::builder()
            .title("Log In With Your Phone")
            .description("Scan the code with the phone that has Spotify on it, then confirm there.")
            .child(&pairing_box)
            .build();

        // ---- sidebar ----
        let sidebar = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        for (id, label, icon) in PLACES {
            sidebar.append(&sidebar_row(id, label, Some(icon), None));
        }
        sidebar.append(&section_header("Playlists"));
        let sidebar_scroller = gtk::ScrolledWindow::builder()
            .child(&sidebar)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let sidebar_page = adw::NavigationPage::builder()
            .title("Descant")
            .child(&{
                let header = adw::HeaderBar::new();
                header.pack_start(
                    &gtk::Button::builder()
                        .icon_name("list-add-symbolic")
                        .tooltip_text("New Playlist")
                        .action_name("win.new-playlist")
                        .build(),
                );
                header.pack_end(
                    &gtk::MenuButton::builder()
                        .icon_name("open-menu-symbolic")
                        .menu_model(&menu)
                        .tooltip_text("Main Menu")
                        .primary(true)
                        .build(),
                );
                let view = adw::ToolbarView::new();
                view.add_top_bar(&header);
                view.set_content(Some(&sidebar_scroller));
                view
            })
            .build();

        // ---- content: a navigation stack, Home at the bottom ----
        // Until the session and the playlists are in (about a second), Home
        // is just a spinner; the real Home replaces it.
        let connecting = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let home = adw::NavigationPage::builder()
            .title("Home")
            .tag("home")
            .child(&with_header(&connecting))
            .build();
        let content = adw::NavigationView::new();
        content.add(&home);
        let content_page = adw::NavigationPage::builder()
            .title("Descant")
            .child(&content)
            .build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content_page)
            .min_sidebar_width(200.0)
            .vexpand(true)
            .build();

        // ---- now-playing bar ----
        let cover = cover_image(COVER_SIZE, "cover");
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            // Bounded, so a long title cannot shove the transport off centre.
            .max_width_chars(24)
            .css_classes(["heading"])
            .build();
        let artist = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(24)
            .css_classes(["dim-label"])
            .build();
        let labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        labels.append(&title);
        labels.append(&artist);
        let previous = round_button("media-skip-backward-symbolic", "Previous", "win.previous");
        let play_pause = round_button("media-playback-start-symbolic", "Play", "win.play-pause");
        // Filled, not flat: a flat suggested button draws a white icon on nothing.
        play_pause.remove_css_class("flat");
        play_pause.add_css_class("suggested-action");
        let next = round_button("media-skip-forward-symbolic", "Next", "win.next");
        let shuffle = gtk::ToggleButton::builder()
            .icon_name("media-playlist-shuffle-symbolic")
            .tooltip_text("Shuffle")
            .valign(gtk::Align::Center)
            .css_classes(["circular", "flat"])
            .build();
        let repeat = round_button(
            "media-playlist-repeat-symbolic",
            "Repeat: Off",
            "win.repeat",
        );
        let up_next = round_button("view-list-symbolic", "Up Next", "win.queue");
        let mute = round_button("audio-volume-high-symbolic", "Mute", "win.mute");
        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        volume.set_width_request(110);
        volume.set_valign(gtk::Align::Center);
        volume.set_draw_value(false);
        volume.set_tooltip_text(Some("Volume"));
        volume.update_property(&[gtk::accessible::Property::Label("Volume")]);
        let like = round_button("non-starred-symbolic", "Save to Liked Songs", "win.like");
        // The bar's top edge doubles as the position: a hair-thin scale that
        // thickens under the pointer, so it can also be dragged to seek.
        let progress = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1000.0);
        progress.set_draw_value(false);
        progress.add_css_class("progress-edge");
        progress.update_property(&[gtk::accessible::Property::Label("Position")]);
        let time = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .valign(gtk::Align::Center)
            .build();
        // A CenterBox, not a row: the transport keeps the middle of the
        // window whatever the title on its left or the volume on its right
        // happen to be wide.
        let playing = gtk::Box::builder().spacing(12).build();
        playing.append(&cover);
        playing.append(&labels);
        playing.append(&time);
        let transport = gtk::Box::builder().spacing(12).build();
        transport.append(&shuffle);
        transport.append(&previous);
        transport.append(&play_pause);
        transport.append(&next);
        transport.append(&repeat);
        let aside = gtk::Box::builder().spacing(12).build();
        aside.append(&like);
        aside.append(&up_next);
        aside.append(&mute);
        aside.append(&volume);
        let bar = gtk::CenterBox::builder()
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        bar.set_start_widget(Some(&playing));
        bar.set_center_widget(Some(&transport));
        bar.set_end_widget(Some(&aside));

        let player_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        player_page.append(&split);
        player_page.append(&progress);
        player_page.append(&bar);

        // ---- window ----
        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&framed(loading.upcast_ref(), false), Some("loading"));
        stack.add_named(&framed(pairing_page.upcast_ref(), true), Some("pairing"));
        stack.add_named(&player_page, Some("player"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Descant")
            .default_width(1024)
            .default_height(700)
            .width_request(360)
            .height_request(400)
            .content(&toasts)
            .build();
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);

        let ui = Rc::new(Ui {
            app: app.clone(),
            window,
            stack,
            toasts,
            qr,
            code,
            open_link,
            pairing_status,
            split,
            sidebar,
            content,
            home,
            cover,
            title,
            artist,
            artist_text: RefCell::new(String::new()),
            loading: adw::Spinner::builder()
                .width_request(16)
                .height_request(16)
                .build(),
            play_pause,
            controller: RefCell::new(None),
            library: Rc::default(),
            like,
            shuffle,
            repeat,
            repeat_mode: Default::default(),
            volume,
            mute,
            progress,
            time,
            duration_ms: Default::default(),
            seeking: Default::default(),
            seek_pending: RefCell::new(None),
            volume_following: Default::default(),
            volume_before_mute: std::cell::Cell::new(50.0),
            volume_pending: RefCell::new(None),
            status: RefCell::new(Status::Idle),
            current: Rc::default(),
            queue: RefCell::new(Vec::new()),
            track_lists: RefCell::new(Vec::new()),
            inhibit: RefCell::new(None),
            pairing_url: RefCell::new(String::new()),
            blocked_shown: RefCell::new(false),
            mpris: RefCell::new(None),
            equalizer: std::sync::Arc::new(std::sync::Mutex::new(settings::equalizer(&settings))),
            settings,
            resume: RefCell::new(None),
            me: RefCell::new(std::rc::Weak::new()),
        });
        *ui.me.borrow_mut() = Rc::downgrade(&ui);
        ui.connect();
        ui
    }

    fn connect(self: &Rc<Self>) {
        // Between updates the place runs on by itself, so it is redrawn on a
        // tick rather than waiting for the engine to say something.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(500), move || match weak.upgrade() {
            Some(ui) => {
                ui.show_position();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        let ui = self.clone();
        self.progress.connect_change_value(move |_, _, value| {
            let ms = value.max(0.0) as u32;
            // Show the place being dragged to at once; send it once the
            // pointer settles, so a drag is one seek and not fifty.
            ui.seeking.set(true);
            ui.time.set_label(&clock(ms, ui.duration_ms.get()));
            if let Some(pending) = ui.seek_pending.borrow_mut().take() {
                pending.remove();
            }
            let settled = ui.clone();
            let id = glib::timeout_add_local_once(Duration::from_millis(150), move || {
                settled.seek_pending.borrow_mut().take();
                settled.seeking.set(false);
                settled.send(Command::Seek(ms));
            });
            *ui.seek_pending.borrow_mut() = Some(id);
            glib::Propagation::Proceed
        });
        let ui = self.clone();
        self.open_link.connect_clicked(move |_| {
            let url = ui.pairing_url.borrow().clone();
            gtk::UriLauncher::new(&url).launch(Some(&ui.window), gio::Cancellable::NONE, |_| {});
        });
        let ui = self.clone();
        self.sidebar.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                ui.navigate(&row.widget_name());
            }
        });

        let action = |name: &str, ui: Rc<Ui>, f: fn(&Rc<Ui>)| {
            let a = gio::SimpleAction::new(name, None);
            let window = ui.window.clone();
            a.connect_activate(move |_, _| f(&ui));
            window.add_action(&a);
        };
        action("play-pause", self.clone(), |ui| {
            let command = match &*ui.status.borrow() {
                Status::Playing { .. } | Status::Loading { .. } => Command::Pause,
                _ => Command::Play,
            };
            ui.send(command);
        });
        action("next", self.clone(), |ui| ui.send(Command::Skip(1)));
        action("previous", self.clone(), |ui| ui.send(Command::Skip(-1)));
        action("queue", self.clone(), |ui| ui.open_queue());
        action("like", self.clone(), |ui| ui.toggle_like());
        action("repeat", self.clone(), |ui| ui.cycle_repeat());
        action("mute", self.clone(), |ui| ui.toggle_mute());
        let ui = self.clone();
        self.volume
            .connect_value_changed(move |scale| ui.volume_moved(scale.value()));
        let ui = self.clone();
        self.shuffle.connect_toggled(move |button| {
            ui.send(Command::Shuffle(button.is_active()));
        });
        action("new-playlist", self.clone(), |ui| ui.new_playlist());
        action("preferences", self.clone(), |ui| ui.preferences());
        action("shortcuts", self.clone(), |ui| ui.shortcuts());
        action("about", self.clone(), |ui| ui.about());

        let quit = gio::SimpleAction::new("quit", None);
        let app = self.app.clone();
        quit.connect_activate(move |_, _| app.quit());
        self.app.add_action(&quit);

        // On the way out, however Descant stops: remember the place, counted
        // up to now if music plays, and write the settings before exiting.
        let ui = self.clone();
        self.app.connect_shutdown(move |_| {
            if let Some((mut point, playing, since)) = ui.resume.borrow().clone() {
                if playing {
                    let ran = since.elapsed().as_millis().min(u32::MAX as u128) as u32;
                    point.position_ms = point.position_ms.saturating_add(ran);
                }
                settings::set_resume(&ui.settings, Some(&point));
            }
            gio::Settings::sync();
        });

        // Closing is not always quitting: with "keep playing" on, the close
        // button means "put it away". The hidden window keeps Descant running
        // (music, media keys, Connect); opening Descant again or raising it from
        // the media controls brings the window back.
        let ui = self.clone();
        self.window.connect_close_request(move |window| {
            if ui.settings.boolean(KEEP_PLAYING) && ui.controller.borrow().is_some() {
                window.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }

    /// What pages get: the session, and ways back into the Ui that do not
    /// keep it alive.
    fn nav(&self) -> Option<Nav> {
        let session = self.library.session.borrow().clone()?;
        let me = self.me.borrow().clone();
        let to_send = me.clone();
        let send = Rc::new(move |command: Command| {
            if let Some(ui) = to_send.upgrade() {
                ui.send(command);
            }
        });
        let open = Rc::new(move |destination: Destination| {
            if let Some(ui) = me.upgrade() {
                ui.open(destination);
            }
        });
        let to_tell = self.me.borrow().clone();
        let event = Rc::new(move |event: Event| {
            if let Some(ui) = to_tell.upgrade() {
                ui.event(event);
            }
        });
        Some(Nav::new(
            session,
            send,
            open,
            event,
            self.current.clone(),
            self.library.clone(),
        ))
    }

    /// A sidebar row was picked: a place (`home`, `search`, …) or a playlist
    /// (`playlist:<n>`, an index into the loaded playlists).
    fn navigate(self: &Rc<Self>, id: &str) {
        self.split.set_show_content(true);
        if id == "home" {
            match self.nav() {
                Some(nav) => self.content.replace(&[home::page(&nav)]),
                None => self.content.replace(std::slice::from_ref(&self.home)),
            }
            return;
        }
        let Some(nav) = self.nav() else {
            self.toast("Still connecting to Spotify…");
            return;
        };
        let page = if let Some(i) = id
            .strip_prefix("playlist:")
            .and_then(|i| i.parse::<usize>().ok())
        {
            let Some(summary) = self.library.playlists.borrow().get(i).cloned() else {
                return;
            };
            self.track_page(
                &nav,
                Source::Playlist { uri: summary.uri },
                &summary.name,
                &summary.covers,
            )
        } else {
            match id {
                "search" => search::page(&nav),
                "liked" => self.track_page(&nav, Source::Liked, "Liked Songs", &[]),
                "albums" => {
                    let session = nav.session();
                    let saved = nav
                        .library
                        .collection
                        .borrow()
                        .as_ref()
                        .map(|c| c.albums.clone());
                    grid::grid_page(&nav, "Albums", "No Saved Albums", async move {
                        let uris = match saved {
                            Some(uris) => uris,
                            None => library::collection(&session).await.ok()?.albums,
                        };
                        let albums = library::albums(&session, &uris).await.ok()?;
                        Some(albums.iter().map(Card::album).collect())
                    })
                }
                "artists" => {
                    let session = nav.session();
                    grid::grid_page(&nav, "Artists", "No Followed Artists", async move {
                        let artists = library::followed_artists(&session).await.ok()?;
                        Some(artists.iter().map(Card::artist).collect())
                    })
                }
                _ => return,
            }
        };
        self.content.replace(&[page]);
    }

    /// An album or artist, opened on top of the current page.
    fn open(self: &Rc<Self>, destination: Destination) {
        let Some(nav) = self.nav() else { return };
        let page = match destination {
            Destination::Liked => self.track_page(&nav, Source::Liked, "Liked Songs", &[]),
            Destination::Album { uri, name } => {
                self.track_page(&nav, Source::Album { uri }, &name, &[])
            }
            Destination::Artist { uri, name } => artist::page(&nav, &uri, &name),
            Destination::Playlist { uri, name, covers } => {
                self.track_page(&nav, Source::Playlist { uri }, &name, &covers)
            }
        };
        self.content.push(&page);
    }

    /// Up Next, on top of whatever is open.
    fn open_queue(self: &Rc<Self>) {
        let Some(nav) = self.nav() else { return };
        let uris = self.queue.borrow().clone();
        let page = self.track_page(&nav, Source::Queue { uris }, "Up Next", &[]);
        self.split.set_show_content(true);
        self.content.push(&page);
    }

    fn track_page(
        &self,
        nav: &Nav,
        source: Source,
        title: &str,
        covers: &[descant_core::track::Cover],
    ) -> adw::NavigationPage {
        let page = TrackPage::new(nav, source, title, covers);
        let mut lists = self.track_lists.borrow_mut();
        lists.retain(|weak| weak.upgrade().is_some());
        lists.push(page.store().downgrade());
        page.page.clone()
    }

    /// Stored credentials go straight to the player; otherwise pair.
    fn start(self: &Rc<Self>) {
        self.stack.set_visible_child_name("loading");
        self.start_mpris();
        images::prune_disk_cache();
        let ui = self.clone();
        glib::spawn_future_local(async move {
            match runtime().spawn(store::load()).await {
                Ok(Ok(Some(credentials))) => ui.show_player(credentials),
                Ok(Err(e)) => {
                    log::warn!("{e}");
                    ui.toast("Could not read the keyring; log in again");
                    ui.show_pairing();
                }
                _ => ui.show_pairing(),
            }
        });
    }

    fn start_mpris(self: &Rc<Self>) {
        let send = |command: fn() -> Command| {
            let ui = self.clone();
            Box::new(move || ui.send(command())) as Box<dyn Fn()>
        };
        let (toggle, raise, app) = (self.clone(), self.clone(), self.app.clone());
        let actions = mpris::Actions {
            play_pause: Box::new(move || {
                let _ = WidgetExt::activate_action(&toggle.window, "win.play-pause", None);
            }),
            play: send(|| Command::Play),
            pause: send(|| Command::Pause),
            next: send(|| Command::Skip(1)),
            previous: send(|| Command::Skip(-1)),
            raise: Box::new(move || raise.window.present()),
            quit: Box::new(move || app.quit()),
        };
        *self.mpris.borrow_mut() = Mpris::start(&self.app, actions);
    }

    fn show_pairing(self: &Rc<Self>) {
        self.stack.set_visible_child_name("pairing");
        self.open_link.set_sensitive(false);
        self.pairing_status.set_label("Asking Spotify for a code…");
        let (events, mut rx) = mpsc::unbounded_channel();
        runtime().spawn(pair(events));
        let ui = self.clone();
        glib::spawn_future_local(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    PairingEvent::Code(code) => {
                        ui.qr.show(&code.url);
                        ui.code.set_label(&spaced(&code.user_code));
                        *ui.pairing_url.borrow_mut() = code.url;
                        ui.open_link.set_sensitive(true);
                        ui.pairing_status.set_label("Waiting for your phone…");
                    }
                    PairingEvent::Problem(message) => ui.pairing_status.set_label(&message),
                    PairingEvent::Done(credentials) => {
                        ui.show_player(credentials);
                        break;
                    }
                }
            }
        });
    }

    fn show_player(self: &Rc<Self>, credentials: Credentials) {
        self.stack.set_visible_child_name("player");
        self.title.set_label("Nothing playing");
        self.artist.set_label("");
        if let Some(home) = self.sidebar.row_at_index(0) {
            self.sidebar.select_row(Some(&home));
        }
        let (updates, mut rx) = mpsc::unbounded_channel();
        let handle = {
            let _runtime = runtime().enter();
            let saved = f64::from(self.settings.int(VOLUME));
            self.show_volume(saved);
            let config = ControllerConfig {
                initial_volume: volume::from_percent(saved),
                resume: settings::resume(&self.settings),
                equalizer: self.equalizer.clone(),
                ..ControllerConfig::default()
            };
            controller::spawn(credentials, config, updates)
        };
        *self.controller.borrow_mut() = Some(handle);
        let ui = self.clone();
        glib::spawn_future_local(async move {
            while let Some(update) = rx.recv().await {
                ui.update(update);
            }
        });
    }

    fn update(self: &Rc<Self>, update: Update) {
        match update {
            Update::Status(status) => self.show_status(status),
            Update::Notice(notice) => self.show_notice(notice),
            Update::NowPlaying(info) => self.show_track(info),
            Update::Session(session) => self.session_ready(session),
            Update::Queue(upcoming) => *self.queue.borrow_mut() = upcoming,
            Update::Volume(level) => self.show_volume(volume::to_percent(level)),
            Update::Resume { point, playing } => {
                settings::set_resume(&self.settings, Some(&point));
                *self.resume.borrow_mut() = Some((point, playing, std::time::Instant::now()));
            }
            Update::Error(message) => {
                log::warn!("{message}");
                self.toast(&message);
            }
        }
    }

    /// A (new) session: (re)load the collection and the playlists, and show
    /// the track the last run stopped at.
    fn session_ready(self: &Rc<Self>, session: Session) {
        *self.library.session.borrow_mut() = Some(session.clone());
        self.show_resumed(&session);
        let ui = self.clone();
        glib::spawn_future_local(async move {
            match runtime()
                .spawn(async move { library::collection(&session).await })
                .await
            {
                Ok(Ok(collection)) => {
                    ui.library.set_collection(collection);
                    ui.show_like();
                }
                Ok(Err(e)) => log::warn!("collection: {e}"),
                Err(_) => {}
            }
        });
        self.load_playlists();
    }

    /// Before anything plays, the bar shows where the last run stopped; Play
    /// goes on from there.
    fn show_resumed(self: &Rc<Self>, session: &Session) {
        if !self.current.borrow().is_empty() {
            return;
        }
        let Some(point) = settings::resume(&self.settings) else {
            return;
        };
        let (ui, s) = (self.clone(), session.clone());
        glib::spawn_future_local(async move {
            let uris = vec![point.track];
            let found = runtime()
                .spawn(async move { library::tracks(&s, &uris).await })
                .await;
            // Something may have started playing meanwhile.
            if let Ok(Ok(mut tracks)) = found
                && let Some(info) = tracks.pop()
                && ui.current.borrow().is_empty()
            {
                ui.show_track(info);
            }
        });
    }

    /// (Re)loads the playlists into the sidebar.
    fn load_playlists(self: &Rc<Self>) {
        let Some(session) = self.library.session.borrow().clone() else {
            return;
        };
        let ui = self.clone();
        glib::spawn_future_local(async move {
            match runtime()
                .spawn(async move { library::playlists(&session).await })
                .await
            {
                Ok(Ok(playlists)) => ui.show_playlists(playlists),
                Ok(Err(e)) => {
                    log::warn!("playlists: {e}");
                    ui.toast("Could not load your playlists");
                }
                Err(_) => {}
            }
        });
    }

    fn show_playlists(&self, playlists: Vec<library::PlaylistSummary>) {
        // Drop the rows of an earlier load: everything after the section header.
        let first = PLACES.len() as i32 + 1;
        while let Some(row) = self.sidebar.row_at_index(first) {
            self.sidebar.remove(&row);
        }
        for (i, playlist) in playlists.iter().enumerate() {
            let name = if playlist.name.is_empty() {
                "Untitled"
            } else {
                &playlist.name
            };
            self.sidebar.append(&sidebar_row(
                &format!("playlist:{i}"),
                name,
                None,
                Some(&playlist.covers),
            ));
        }
        *self.library.playlists.borrow_mut() = playlists;
        if let Some(ui) = self.me.borrow().upgrade() {
            ui.fill_missing_covers();
        }
        // Home's tiles show the playlists: rebuild it if it is open.
        let home_open = self
            .sidebar
            .selected_row()
            .is_some_and(|r| r.widget_name() == "home");
        if home_open && let Some(nav) = self.nav() {
            self.content.replace(&[home::page(&nav)]);
        }
    }

    /// Playlists Spotify draws a mosaic for arrive without a picture, so
    /// their rows would stay blank. Ask for the first row of each and use
    /// that track's cover, the way the playlist page already does. One at a
    /// time and only for the ones that need it, so opening the app does not
    /// turn into a burst of requests.
    fn fill_missing_covers(self: &Rc<Self>) {
        let Some(session) = self.library.session.borrow().clone() else {
            return;
        };
        let missing: Vec<(usize, String)> = self
            .library
            .playlists
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, p)| p.covers.is_empty())
            .map(|(i, p)| (i, p.uri.clone()))
            .collect();
        if missing.is_empty() {
            return;
        }
        let ui = self.clone();
        glib::spawn_future_local(async move {
            for (index, uri) in missing {
                let session = session.clone();
                let asked = uri.clone();
                let Ok(Ok(page)) = runtime()
                    .spawn(async move { library::playlist(&session, &asked, 0, 1).await })
                    .await
                else {
                    continue;
                };
                // The playlist itself may name one where the rootlist did not.
                let covers = match (page.covers.is_empty(), page.tracks.first()) {
                    (false, _) => page.covers,
                    (true, Some(first)) => first.covers.clone(),
                    (true, None) => continue,
                };
                if !covers.is_empty() {
                    ui.set_playlist_cover(index, &covers);
                }
            }
        });
    }

    /// Puts a cover on a sidebar row that went without one.
    fn set_playlist_cover(&self, index: usize, covers: &[descant_core::track::Cover]) {
        if let Some(playlist) = self.library.playlists.borrow_mut().get_mut(index) {
            playlist.covers = covers.to_vec();
        }
        let row = index
            .try_into()
            .ok()
            .and_then(|i: i32| self.sidebar.row_at_index(PLACES.len() as i32 + 1 + i));
        let Some(image) = row
            .and_then(|r| r.child())
            .and_downcast::<gtk::Box>()
            .and_then(|content| content.first_child())
            .and_downcast::<gtk::Image>()
        else {
            return;
        };
        // The placeholder icon was drawn small; a cover fills the row.
        image.set_pixel_size(SIDEBAR_COVER);
        images::show(&image, pick(covers, SIDEBAR_COVER, &image));
    }

    fn show_status(self: &Rc<Self>, status: Status) {
        let playing = matches!(status, Status::Playing { .. } | Status::Loading { .. });
        // While loading, a spinner sits inside the play button: same size, no jump.
        if matches!(status, Status::Loading { .. }) {
            self.play_pause.set_child(Some(&self.loading));
        } else {
            self.play_pause.set_icon_name(if playing {
                "media-playback-pause-symbolic"
            } else {
                "media-playback-start-symbolic"
            });
        }
        self.play_pause
            .set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
        // A state worth knowing stands in for the artist line, in the accent
        // colour; the bar keeps its two lines and its height.
        let line = match &status {
            Status::Throttled { .. } => {
                Some("Spotify is pacing playback; resuming by itself".into())
            }
            Status::Reconnecting { .. } => Some("Reconnecting…".into()),
            Status::OutputFailed { message } => Some(format!("No sound output: {message}")),
            Status::Blocked => Some("This account cannot play here".into()),
            _ => None,
        };
        match line {
            Some(message) => {
                self.artist.add_css_class("accent");
                self.artist.remove_css_class("dim-label");
                self.artist.set_label(&message);
            }
            None => {
                self.artist.remove_css_class("accent");
                self.artist.add_css_class("dim-label");
                self.artist.set_label(&self.artist_text.borrow());
            }
        }
        self.set_inhibit(matches!(status, Status::Playing { .. }));
        if let Some(mpris) = &*self.mpris.borrow() {
            mpris.status(&status);
        }
        *self.status.borrow_mut() = status;
    }

    fn show_notice(self: &Rc<Self>, notice: Notice) {
        match notice {
            Notice::Throttled { retry_in_secs } => self.toast(&format!(
                "Spotify is limiting how fast tracks can start; trying again in {retry_in_secs} s"
            )),
            Notice::Recovered => self.toast("Playing again"),
            Notice::TrackUnavailable { .. } => self.toast("This track can't be played"),
            Notice::AudioOutputFailed { message } => {
                self.toast(&format!("The sound output failed: {message}"))
            }
            Notice::ConnectionLost => self.toast("Connection lost; reconnecting"),
            Notice::Blocked => self.blocked(),
        }
    }

    /// What pages tell the window.
    fn event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Toast(message) => self.toast(&message),
            Event::LogOut => self.log_out(),
            Event::LikedChanged => self.show_like(),
            Event::PlaylistsChanged => self.load_playlists(),
        }
    }

    /// The slider moved: by the user (sent after a short pause, and saved),
    /// or following the engine (only the icon changes).
    fn volume_moved(self: &Rc<Self>, percent: f64) {
        self.mute.set_icon_name(volume::icon(percent));
        if self.volume_following.get() {
            return;
        }
        if let Some(pending) = self.volume_pending.borrow_mut().take() {
            pending.remove();
        }
        let ui = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(120), move || {
            ui.volume_pending.borrow_mut().take();
            ui.send(Command::SetVolume(volume::from_percent(percent)));
            if let Err(e) = ui.settings.set_int(VOLUME, percent.round() as i32) {
                log::warn!("volume not saved: {e}");
            }
        });
        *self.volume_pending.borrow_mut() = Some(id);
    }

    /// Where the track stands now: the last place the engine named, plus the
    /// time that has run since, while playing.
    fn position_ms(&self) -> u32 {
        match &*self.resume.borrow() {
            Some((point, playing, since)) => {
                let ran = if *playing {
                    u32::try_from(since.elapsed().as_millis()).unwrap_or(u32::MAX)
                } else {
                    0
                };
                point.position_ms.saturating_add(ran)
            }
            None => 0,
        }
    }

    /// Moves the edge and the clock to that place, unless it is being dragged.
    fn show_position(&self) {
        if self.seeking.get() {
            return;
        }
        let duration = self.duration_ms.get();
        let position = self.position_ms().min(duration);
        self.progress.set_value(f64::from(position));
        self.time.set_label(&clock(position, duration));
    }

    /// Shows a level that came from elsewhere, without sending it back.
    fn show_volume(&self, percent: f64) {
        self.volume_following.set(true);
        self.volume.set_value(percent);
        self.volume_following.set(false);
        self.mute.set_icon_name(volume::icon(percent));
    }

    fn toggle_mute(&self) {
        let now = self.volume.value();
        if now > 0.0 {
            self.volume_before_mute.set(now);
            self.volume.set_value(0.0);
        } else {
            self.volume
                .set_value(self.volume_before_mute.get().max(5.0));
        }
    }

    /// Off → the whole context → this track → off.
    fn cycle_repeat(&self) {
        use descant_core::supervisor::Repeat;
        let mode = self.repeat_mode.get().next();
        self.repeat_mode.set(mode);
        let (icon, tip) = match mode {
            Repeat::Off => ("media-playlist-repeat-symbolic", "Repeat: Off"),
            Repeat::Context => ("media-playlist-repeat-symbolic", "Repeat: All"),
            Repeat::Track => ("media-playlist-repeat-song-symbolic", "Repeat: This Song"),
        };
        self.repeat.set_icon_name(icon);
        self.repeat.set_tooltip_text(Some(tip));
        if mode == Repeat::Off {
            self.repeat.remove_css_class("accent");
        } else {
            self.repeat.add_css_class("accent");
        }
        self.send(Command::Repeat(mode));
    }

    /// The heart follows whether the playing track is liked.
    fn show_like(&self) {
        let liked = self
            .library
            .liked
            .borrow()
            .contains(&*self.current.borrow());
        // Filled or hollow says it; the colour only underlines it.
        if liked {
            self.like.set_icon_name("starred-symbolic");
            self.like.add_css_class("accent");
            self.like.set_tooltip_text(Some("Remove from Liked Songs"));
        } else {
            self.like.set_icon_name("non-starred-symbolic");
            self.like.remove_css_class("accent");
            self.like.set_tooltip_text(Some("Save to Liked Songs"));
        }
    }

    fn toggle_like(self: &Rc<Self>) {
        let uri = self.current.borrow().clone();
        let Some(nav) = self.nav() else { return };
        if !uri.starts_with("spotify:track:") {
            return;
        }
        let liked = !self.library.liked.borrow().contains(&uri);
        glib::spawn_future_local(async move {
            nav.set_liked(uri, liked).await;
        });
    }

    fn new_playlist(self: &Rc<Self>) {
        let Some(session) = self.library.session.borrow().clone() else {
            return;
        };
        let entry = gtk::Entry::builder()
            .placeholder_text("Playlist name")
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading("New Playlist")
            .extra_child(&entry)
            .default_response("create")
            .close_response("cancel")
            .build();
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("create", "_Create");
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            let name = entry.text().trim().to_string();
            if response != "create" || name.is_empty() {
                return;
            }
            let (ui, session) = (ui.clone(), session.clone());
            glib::spawn_future_local(async move {
                let n = name.clone();
                let done = runtime()
                    .spawn(
                        async move { descant_spotify::edit::create_playlist(&session, &n).await },
                    )
                    .await;
                match done {
                    Ok(Ok(_)) => {
                        ui.toast(&format!("Created {name}"));
                        ui.event(Event::PlaylistsChanged);
                    }
                    _ => ui.toast("Spotify did not create the playlist"),
                }
            });
        });
        dialog.present(Some(&self.window));
    }

    fn show_track(self: &Rc<Self>, info: TrackInfo) {
        if let Some(mpris) = &*self.mpris.borrow() {
            mpris.track(&info);
        }
        self.title.set_label(&info.title);
        let artists = info.artist_line();
        if !self.artist.has_css_class("accent") {
            self.artist.set_label(&artists);
        }
        *self.artist_text.borrow_mut() = artists;
        images::show(&self.cover, pick(&info.covers, COVER_SIZE, &self.cover));
        self.duration_ms.set(info.duration_ms);
        self.progress
            .set_range(0.0, f64::from(info.duration_ms.max(1)));
        self.show_position();
        // Mark the playing track in every open list, and follow it with the heart.
        *self.current.borrow_mut() = info.uri;
        self.show_like();
        let mut lists = self.track_lists.borrow_mut();
        lists.retain(|weak| match weak.upgrade() {
            Some(store) => {
                let n = store.n_items();
                store.items_changed(0, n, n);
                true
            }
            None => false,
        });
    }

    fn send(&self, command: Command) {
        if let Some(controller) = &*self.controller.borrow() {
            controller.send(command);
        }
    }

    /// Keeps the session awake while music plays.
    fn set_inhibit(&self, on: bool) {
        let mut cookie = self.inhibit.borrow_mut();
        match (on, cookie.is_some()) {
            (true, false) => {
                let id = self.app.inhibit(
                    Some(&self.window),
                    gtk::ApplicationInhibitFlags::SUSPEND,
                    Some("Playing music"),
                );
                *cookie = (id != 0).then_some(id);
            }
            (false, true) => {
                if let Some(id) = cookie.take() {
                    self.app.uninhibit(id);
                }
            }
            _ => {}
        }
    }

    fn blocked(self: &Rc<Self>) {
        if self.blocked_shown.replace(true) {
            return;
        }
        let dialog = adw::AlertDialog::builder()
            .heading("This Account Can't Play Here")
            .body(
                "Spotify protects the audio of this account with its own DRM (PlayPlay), \
                 which only Spotify's official apps can play. Descant will not work around it.",
            )
            .build();
        dialog.add_response("ok", "_OK");
        dialog.present(Some(&self.window));
    }

    fn log_out(self: &Rc<Self>) {
        self.controller.borrow_mut().take(); // the controller stops once its handle is gone
        self.library.clear();
        // The next account starts fresh.
        self.resume.borrow_mut().take();
        settings::set_resume(&self.settings, None);
        self.current.borrow_mut().clear();
        self.show_playlists(Vec::new());
        self.content.replace(std::slice::from_ref(&self.home));
        self.set_inhibit(false);
        let ui = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(Err(e)) = runtime().spawn(store::clear()).await {
                log::warn!("{e}");
            }
            ui.show_pairing();
        });
    }

    /// The choices that outlive a session.
    fn preferences(self: &Rc<Self>) {
        let nav = self.nav();
        crate::preferences::present(&self.window, &self.settings, &self.equalizer, nav.as_ref());
    }

    fn shortcuts(self: &Rc<Self>) {
        let dialog = adw::ShortcutsDialog::new();
        let section = |title: &str, items: &[(&str, &str)]| {
            let section = adw::ShortcutsSection::new(Some(title));
            for (name, accelerator) in items {
                section.add(adw::ShortcutsItem::new(name, accelerator));
            }
            section
        };
        dialog.add(section(
            "Playback",
            &[
                ("Play or Pause", "<Control>space"),
                ("Next Track", "<Control>Right"),
                ("Previous Track", "<Control>Left"),
            ],
        ));
        dialog.add(section(
            "General",
            &[
                ("Preferences", "<Control>comma"),
                ("Keyboard Shortcuts", "<Control>question"),
                ("Quit", "<Control>q"),
            ],
        ));
        dialog.present(Some(&self.window));
    }

    fn about(self: &Rc<Self>) {
        let about = adw::AboutDialog::builder()
            .application_name("Descant")
            .application_icon(crate::APP_ID)
            .developer_name("Mitchell de Jong")
            .version(env!("CARGO_PKG_VERSION"))
            .license_type(gtk::License::MitX11)
            .website("https://github.com/develonrails/descant")
            .issue_url("https://github.com/develonrails/descant/issues")
            .comments(
                "A small, fast client for Spotify, built with Rust, GTK and librespot. \
                 Not affiliated with Spotify.",
            )
            .build();
        about.add_acknowledgement_section(
            Some("Plays Through"),
            &["librespot https://github.com/librespot-org/librespot"],
        );
        about.present(Some(&self.window));
    }

    fn toast(&self, message: &str) {
        self.toasts
            .add_toast(adw::Toast::builder().title(message).timeout(4).build());
    }
}

/// Adds the Quit section to the main menu, or takes it away.
fn show_quit(menu: &gio::Menu, shown: bool) {
    let present = menu.n_items() > 1;
    if shown && !present {
        let quit = gio::Menu::new();
        quit.append(Some("_Quit"), Some("app.quit"));
        menu.append_section(None, &quit);
    } else if !shown && present {
        menu.remove(1);
    }
}

/// A selectable sidebar row with an icon or a cover; its widget name carries the id.
/// `1:23 / 4:55`, and nothing at all when no track is loaded.
fn clock(position_ms: u32, duration_ms: u32) -> String {
    if duration_ms == 0 {
        return String::new();
    }
    format!("{} / {}", mmss(position_ms), mmss(duration_ms))
}

fn mmss(ms: u32) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn sidebar_row(
    id: &str,
    label: &str,
    icon: Option<&str>,
    covers: Option<&[descant_core::track::Cover]>,
) -> gtk::ListBoxRow {
    let content = gtk::Box::builder().spacing(10).build();
    match covers {
        Some(covers) if !covers.is_empty() => {
            let cover = cover_image(SIDEBAR_COVER, "cover");
            images::show(&cover, pick(covers, SIDEBAR_COVER, &cover));
            content.append(&cover);
        }
        Some(_) => {
            // Spotify sends no picture at all for a playlist it draws a
            // mosaic for. The row keeps a cover-shaped placeholder, which
            // `set_playlist_cover` fills in once a cover is known.
            let image = gtk::Image::builder()
                .icon_name("view-list-symbolic")
                .pixel_size(16)
                .css_classes(["cover"])
                .overflow(gtk::Overflow::Hidden)
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .build();
            image.set_size_request(SIDEBAR_COVER, SIDEBAR_COVER);
            content.append(&image);
        }
        None => content.append(&gtk::Image::from_icon_name(
            icon.unwrap_or("view-list-symbolic"),
        )),
    }
    content.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    gtk::ListBoxRow::builder().name(id).child(&content).build()
}

/// A heading between sidebar sections; not selectable.
fn section_header(title: &str) -> gtk::ListBoxRow {
    gtk::ListBoxRow::builder()
        .child(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .margin_top(12)
                .css_classes(["heading", "dim-label"])
                .build(),
        )
        .selectable(false)
        .activatable(false)
        .build()
}

fn round_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .action_name(action)
        .valign(gtk::Align::Center)
        .css_classes(["circular", "flat"])
        .build()
}

/// "ABCDEF" → "ABC DEF": easier to read off and type.
fn spaced(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

enum PairingEvent {
    Code(pairing::PairingCode),
    Problem(String),
    Done(Credentials),
}

/// Keeps a fresh code on screen until the phone confirms, then stores the
/// credentials. Ends when the UI stops listening.
async fn pair(events: mpsc::UnboundedSender<PairingEvent>) {
    while !events.is_closed() {
        match pairing::begin().await {
            Ok((code, pending)) => {
                let lifetime = code.expires_in;
                let _ = events.send(PairingEvent::Code(code));
                match tokio::time::timeout(lifetime, pending.finish()).await {
                    Ok(Ok(account)) => {
                        log::info!("paired as {}", account.username);
                        if let Err(e) = store::save(&account.credentials).await {
                            log::warn!("{e}");
                        }
                        let _ = events.send(PairingEvent::Done(account.credentials));
                        return;
                    }
                    Ok(Err(e)) => {
                        log::warn!("pairing: {e}");
                        let _ = events.send(PairingEvent::Problem(
                            "That didn't work. Here is a new code.".into(),
                        ));
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    Err(_) => {} // expired: show a new code
                }
            }
            Err(e) => {
                log::warn!("pairing: {e}");
                let _ = events.send(PairingEvent::Problem(
                    "Can't reach Spotify. Trying again…".into(),
                ));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_split_for_reading() {
        assert_eq!(spaced("ABCDEF"), "ABC DEF");
        assert_eq!(spaced("ABCDEFGH"), "ABCDEFGH");
    }
}
