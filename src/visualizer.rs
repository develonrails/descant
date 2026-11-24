//! The visualizer: what plays, as light.
//!
//! In the spirit of MilkDrop, lighter, with a few presets to pick from in
//! Preferences ([`Preset`]): a tunnel of waveform rings, a nebula with the
//! waveform rising like smoke, a spectrum over a mirroring horizon, and a
//! starfield flying faster as the music gets louder. Beats flare in each.
//! It can also pick another preset with every song. Its colours keep close
//! to the accent colour picked in GNOME's settings, or drift through the
//! rainbow.
//!
//! When a new song starts, when it goes full screen and when it is switched
//! on, the song's cover, title and artists show in the corner for a while.
//!
//! In GNOME's power saver mode it draws half as often.
//!
//! A click on it plays or pauses; a double click goes full screen.
//!
//! It draws with GTK's own render nodes (gradients and rectangles the GPU
//! draws as they are), into a `GtkPicture`, one frame per display frame.
//! It only runs while it can be seen: hidden, it neither listens to the
//! audio nor wakes up, and once the music stops it settles and stops too.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    f32::consts::{PI, TAU},
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

use descant_core::{
    spectrum::{Analyzer, BANDS, Frame, WAVE, WINDOW},
    track::TrackInfo,
};
use descant_playback::scope::{SAMPLE_RATE, Scope};
use gtk::{gdk, gio, glib, graphene, gsk, prelude::*};

use crate::{
    images,
    settings::{self, VISUALIZER_ACCENT, VISUALIZER_PRESET, VISUALIZER_SHUFFLE},
    widgets::{cover_image, pick},
};

/// How often a ring is left behind, and how long it lasts.
const ECHO_EVERY_S: f64 = 0.08;
const ECHO_LIFE_S: f64 = 1.2;
/// How fast echoes zoom out: e^(rate · age).
const ZOOM_RATE: f32 = 1.15;
/// Points on an echo; the ring itself has `2 · WAVE`, mirrored.
const ECHO_POINTS: usize = 96;
/// Silence this long, with nothing playing, and it stops drawing.
const SETTLE_S: f32 = 2.5;
/// No more than about 60 frames a second, however fast the display; about
/// 30 in power saver mode. A little under the frame time, so a 60 Hz
/// display draws every frame, or every other one.
const MIN_FRAME_US: i64 = 15_000;
const MIN_FRAME_SAVING_US: i64 = 31_000;
/// In full screen, the button and the pointer go after this long still.
const HIDE_AFTER: Duration = Duration::from_millis(2500);
/// How long the playing song shows in the corner.
const CAPTION_FOR: Duration = Duration::from_secs(6);
/// The cover in the corner, in logical pixels.
const CAPTION_COVER: i32 = 88;

/// What the visualizer draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    #[default]
    Tunnel,
    Nebula,
    Spectrum,
    Starfield,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Tunnel,
        Preset::Nebula,
        Preset::Spectrum,
        Preset::Starfield,
    ];

    /// How the setting names it.
    pub fn id(self) -> &'static str {
        match self {
            Preset::Tunnel => "tunnel",
            Preset::Nebula => "nebula",
            Preset::Spectrum => "spectrum",
            Preset::Starfield => "starfield",
        }
    }

    pub fn from_id(id: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Preset::Tunnel => "Tunnel",
            Preset::Nebula => "Nebula",
            Preset::Spectrum => "Spectrum",
            Preset::Starfield => "Starfield",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Preset::Tunnel => "The waveform as a ring, leaving echoes that zoom out",
            Preset::Nebula => "Slow clouds of colour, the waveform rising like smoke",
            Preset::Spectrum => "Glowing bars over a horizon that mirrors them",
            Preset::Starfield => "Stars flying past, faster as the music gets louder",
        }
    }
}

pub struct Visualizer {
    pub root: gtk::Overlay,
    picture: gtk::Picture,
    controls: gtk::Revealer,
    fullscreen: gtk::Button,
    scope: Arc<Scope>,
    scene: RefCell<Scene>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    playing: Cell<bool>,
    /// Whether GNOME's power saver mode is on: then half the frames.
    power: gio::PowerProfileMonitor,
    /// Full screen: the button and the pointer hide while nothing moves.
    immersed: Cell<bool>,
    hide_pending: RefCell<Option<glib::SourceId>>,
    /// A click waiting to be sure it is not the first of a double click.
    click_pending: RefCell<Option<glib::SourceId>>,
    caption: Caption,
    /// The preset shown now: the chosen one, or another with every song.
    preset: Cell<Preset>,
    settings: gio::Settings,
    palette: Cell<Palette>,
}

/// Which colours it draws in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Palette {
    /// The hue it keeps close to; none to drift through them all.
    anchor: Option<f32>,
    /// How far apart the hues in one picture are (1: as far as they go).
    spread: f32,
    /// A greyish accent (slate) makes everything greyer too.
    saturation: f32,
}

impl Palette {
    const RAINBOW: Palette = Palette {
        anchor: None,
        spread: 1.0,
        saturation: 1.0,
    };

    /// Colours around `accent`.
    fn around(accent: gdk::RGBA) -> Palette {
        let (hue, saturation, _) = to_hsv(accent);
        Palette {
            anchor: Some(hue),
            spread: 0.3,
            saturation: (saturation / 0.7).clamp(0.2, 1.0),
        }
    }

    /// From the settings and the accent colour GNOME has now.
    fn current(settings: &gio::Settings) -> Palette {
        if settings.boolean(VISUALIZER_ACCENT) {
            Palette::around(adw::StyleManager::default().accent_color_rgba())
        } else {
            Palette::RAINBOW
        }
    }
}

/// The playing song, in the bottom-left corner.
struct Caption {
    revealer: gtk::Revealer,
    cover: gtk::Image,
    title: gtk::Label,
    artists: gtk::Label,
    track: RefCell<Option<TrackInfo>>,
    hide_pending: RefCell<Option<glib::SourceId>>,
}

impl Caption {
    fn new() -> Caption {
        let cover = cover_image(CAPTION_COVER, "cover-large");
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(36)
            .css_classes(["title-1"])
            .build();
        let artists = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .css_classes(["title-4", "artists"])
            .build();
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(4)
            .build();
        lines.append(&title);
        lines.append(&artists);
        let card = gtk::Box::builder()
            .spacing(18)
            .css_classes(["visualizer-caption"])
            .build();
        card.append(&cover);
        card.append(&lines);
        let revealer = gtk::Revealer::builder()
            .child(&card)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(700)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .margin_start(28)
            .margin_bottom(28)
            .can_target(false)
            .build();
        Caption {
            revealer,
            cover,
            title,
            artists,
            track: RefCell::new(None),
            hide_pending: RefCell::new(None),
        }
    }
}

impl Visualizer {
    pub fn new(scope: Arc<Scope>, settings: &gio::Settings) -> Rc<Visualizer> {
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Fill)
            .hexpand(true)
            .vexpand(true)
            .alternative_text("Visualizer")
            .build();
        let fullscreen = gtk::Button::builder()
            .icon_name("view-fullscreen-symbolic")
            .tooltip_text("Full Screen")
            .action_name("win.visualizer-fullscreen")
            .css_classes(["osd", "circular"])
            .build();
        let controls = gtk::Revealer::builder()
            .child(&fullscreen)
            .reveal_child(true)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .margin_end(18)
            .margin_bottom(18)
            .build();
        let caption = Caption::new();
        let root = gtk::Overlay::builder().child(&picture).build();
        root.add_overlay(&caption.revealer);
        root.add_overlay(&controls);

        let visualizer = Rc::new(Visualizer {
            root,
            picture,
            controls,
            fullscreen,
            scope,
            scene: RefCell::new(Scene::new()),
            tick: RefCell::new(None),
            playing: Cell::new(false),
            power: gio::PowerProfileMonitor::get_default(),
            immersed: Cell::new(false),
            hide_pending: RefCell::new(None),
            click_pending: RefCell::new(None),
            caption,
            preset: Cell::new(settings::visualizer_preset(settings)),
            settings: settings.clone(),
            palette: Cell::new(Palette::current(settings)),
        });
        visualizer.connect();
        visualizer
    }

    fn connect(self: &Rc<Self>) {
        // A preset picked in Preferences shows at once.
        let weak = Rc::downgrade(self);
        self.settings
            .connect_changed(Some(VISUALIZER_PRESET), move |settings, _| {
                if let Some(v) = weak.upgrade() {
                    v.preset.set(settings::visualizer_preset(settings));
                }
            });
        // The accent colour, or the setting to follow it, changed.
        let weak = Rc::downgrade(self);
        self.settings
            .connect_changed(Some(VISUALIZER_ACCENT), move |settings, _| {
                if let Some(v) = weak.upgrade() {
                    v.palette.set(Palette::current(settings));
                }
            });
        let weak = Rc::downgrade(self);
        adw::StyleManager::default().connect_accent_color_rgba_notify(move |_| {
            if let Some(v) = weak.upgrade() {
                v.palette.set(Palette::current(&v.settings));
            }
        });
        // Switching the change with every song off goes back to the chosen one.
        let weak = Rc::downgrade(self);
        self.settings
            .connect_changed(Some(VISUALIZER_SHUFFLE), move |settings, key| {
                if let Some(v) = weak.upgrade()
                    && !settings.boolean(key)
                {
                    v.preset.set(settings::visualizer_preset(settings));
                }
            });
        // Shown: listen and draw. Gone (switched off, or the window hidden):
        // stop listening and let the frames go.
        let weak = Rc::downgrade(self);
        self.picture.connect_map(move |_| {
            if let Some(v) = weak.upgrade() {
                v.scope.listen(true);
                v.run();
                v.show_caption();
            }
        });
        let weak = Rc::downgrade(self);
        self.picture.connect_unmap(move |picture| {
            if let Some(v) = weak.upgrade() {
                if let Some(tick) = v.tick.borrow_mut().take() {
                    tick.remove();
                }
                v.scope.listen(false);
                v.scene.replace(Scene::new());
                picture.set_paintable(None::<&gdk::Paintable>);
                v.hide_caption();
                v.forget_click();
            }
        });

        // A click plays or pauses, a double click goes full screen. A click
        // waits out the double-click time first, so that a double click
        // does not pause and resume on its way to full screen. Clicks on
        // the full-screen button stay the button's.
        let clicks = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        clicks.connect_pressed(move |gesture, presses, _, _| {
            let (Some(v), Some(widget)) = (weak.upgrade(), gesture.widget()) else {
                return;
            };
            v.forget_click();
            match on_press(presses) {
                Click::Wait => {
                    let weak = Rc::downgrade(&v);
                    let id = glib::timeout_add_local_once(double_click_time(), move || {
                        if let Some(v) = weak.upgrade() {
                            // it has fired: only forget it
                            v.click_pending.borrow_mut().take();
                        }
                        let _ = widget.activate_action("win.play-pause", None);
                    });
                    *v.click_pending.borrow_mut() = Some(id);
                }
                Click::FullScreen => {
                    let _ = widget.activate_action("win.visualizer-fullscreen", None);
                }
                Click::Nothing => {}
            }
        });
        self.root.add_controller(clicks);

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, _, _| {
            if let Some(v) = weak.upgrade() {
                v.stirred();
            }
        });
        self.root.add_controller(motion);
    }

    /// Drops a click still waiting to play or pause.
    fn forget_click(&self) {
        if let Some(pending) = self.click_pending.borrow_mut().take() {
            pending.remove();
        }
    }

    /// Whether music plays; it starts drawing again when it does.
    pub fn set_playing(self: &Rc<Self>, playing: bool) {
        self.playing.set(playing);
        if playing && self.picture.is_mapped() {
            self.run();
        }
    }

    /// Full screen or not: the button says which way it goes, and in full
    /// screen it hides, with the pointer, while nothing moves.
    pub fn set_immersed(self: &Rc<Self>, on: bool) {
        let entering = on && !self.immersed.get();
        self.immersed.set(on);
        if entering {
            self.show_caption();
        }
        let (icon, tip) = if on {
            ("view-restore-symbolic", "Leave Full Screen")
        } else {
            ("view-fullscreen-symbolic", "Full Screen")
        };
        self.fullscreen.set_icon_name(icon);
        self.fullscreen.set_tooltip_text(Some(tip));
        self.stirred();
    }

    /// A song started (or its details came in): shown in the corner if the
    /// visualizer is on, and kept for when it comes on.
    pub fn track(self: &Rc<Self>, info: &TrackInfo) {
        let new = self.caption.track.borrow().as_ref().map(|t| &t.uri) != Some(&info.uri);
        *self.caption.track.borrow_mut() = Some(info.clone());
        if new {
            if self.settings.boolean(VISUALIZER_SHUFFLE) {
                let next = self.scene.borrow_mut().other_than(self.preset.get());
                self.preset.set(next);
            }
            self.show_caption();
        }
    }

    /// Shows the playing song in the corner for a while, if there is one and
    /// the visualizer can be seen. The cover is only fetched now.
    fn show_caption(self: &Rc<Self>) {
        if !self.picture.is_mapped() {
            return;
        }
        let caption = &self.caption;
        let Some(info) = caption.track.borrow().clone() else {
            return;
        };
        caption.title.set_label(&info.title);
        caption.artists.set_label(&info.artist_line());
        images::show(
            &caption.cover,
            pick(&info.covers, CAPTION_COVER, &caption.cover),
        );
        caption.cover.set_visible(!info.covers.is_empty());
        caption.revealer.set_reveal_child(true);
        if let Some(pending) = caption.hide_pending.borrow_mut().take() {
            pending.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(CAPTION_FOR, move || {
            if let Some(v) = weak.upgrade() {
                v.caption.hide_pending.borrow_mut().take();
                v.caption.revealer.set_reveal_child(false);
            }
        });
        *caption.hide_pending.borrow_mut() = Some(id);
    }

    /// Takes the song out of the corner at once, and lets go of its cover.
    fn hide_caption(&self) {
        let caption = &self.caption;
        if let Some(pending) = caption.hide_pending.borrow_mut().take() {
            pending.remove();
        }
        caption.revealer.set_reveal_child(false);
        images::show(&caption.cover, None);
    }

    /// The pointer moved: show the button (and the pointer), and in full
    /// screen hide them again a little later.
    fn stirred(self: &Rc<Self>) {
        self.controls.set_reveal_child(true);
        self.root.set_cursor(None);
        if let Some(pending) = self.hide_pending.borrow_mut().take() {
            pending.remove();
        }
        if !self.immersed.get() {
            return;
        }
        let weak: Weak<Visualizer> = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(HIDE_AFTER, move || {
            if let Some(v) = weak.upgrade() {
                v.hide_pending.borrow_mut().take();
                v.controls.set_reveal_child(false);
                v.root.set_cursor_from_name(Some("none"));
            }
        });
        *self.hide_pending.borrow_mut() = Some(id);
    }

    /// Draws on every frame until it settles.
    fn run(self: &Rc<Self>) {
        if self.tick.borrow().is_some() {
            return;
        }
        self.scene.borrow_mut().quiet = 0.0;
        let weak = Rc::downgrade(self);
        let id = self.picture.add_tick_callback(move |picture, clock| {
            let Some(v) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let more = v.frame(picture, clock.frame_time());
            if !more {
                // Returning Break removes it; only forget the id.
                v.tick.borrow_mut().take();
            }
            more.into()
        });
        *self.tick.borrow_mut() = Some(id);
    }

    /// One frame at `now_us`; false once it has settled.
    fn frame(&self, picture: &gtk::Picture, now_us: i64) -> bool {
        let mut scene = self.scene.borrow_mut();
        let gap = frame_gap_us(self.power.is_power_saver_enabled());
        let dt = match scene.last_us {
            Some(last) if now_us - last < gap => return true,
            Some(last) => (now_us - last) as f32 / 1e6,
            None => 1.0 / 60.0,
        };
        scene.last_us = Some(now_us);
        scene.palette = self.palette.get();
        let mut samples = std::mem::take(&mut scene.samples);
        let heard = self.scope.heard(Instant::now(), &mut samples);
        scene.analyzer.update(&samples, dt);
        scene.samples = samples;
        scene.quiet = if heard && scene.analyzer.frame().energy() > 0.01 {
            0.0
        } else {
            scene.quiet + dt
        };
        scene.advance(dt);
        let (w, h) = (picture.width() as f32, picture.height() as f32);
        if w > 0.0 && h > 0.0 {
            picture.set_paintable(scene.draw(self.preset.get(), w, h).as_ref());
        }
        self.playing.get() || scene.quiet < SETTLE_S
    }
}

struct Echo {
    born: f64,
    hue: f32,
    turn: f32,
    energy: f32,
    wave: [f32; ECHO_POINTS],
}

struct Scene {
    analyzer: Analyzer,
    samples: Vec<f32>,
    last_us: Option<i64>,
    /// Seconds drawn so far.
    t: f64,
    hue: f32,
    /// How far the rings have turned, in radians.
    turn: f32,
    echoes: VecDeque<Echo>,
    last_echo: f64,
    /// How long nothing has been heard.
    quiet: f32,
    /// Each band's recent highest, falling slowly (Spectrum's caps).
    peaks: [f32; BANDS],
    stars: Vec<Star>,
    /// How fast the stars fly, in depth per second.
    speed: f32,
    random: Random,
    palette: Palette,
    /// Where the hue is in its wandering around the accent, 0–1.
    wander: f32,
}

/// A star: across and up from the middle (−1–1), and how far away (0–1].
struct Star {
    x: f32,
    y: f32,
    z: f32,
}

const STARS: usize = 420;

impl Scene {
    fn new() -> Scene {
        Scene {
            analyzer: Analyzer::new(SAMPLE_RATE),
            samples: vec![0.0; WINDOW],
            last_us: None,
            t: 0.0,
            hue: 0.62,
            turn: 0.0,
            echoes: VecDeque::new(),
            last_echo: 0.0,
            quiet: 0.0,
            peaks: [0.0; BANDS],
            stars: Vec::new(),
            speed: 0.0,
            random: Random::seeded(),
            palette: Palette::RAINBOW,
            wander: 0.0,
        }
    }

    /// The hue now, `offset` further on (less far around an accent).
    fn colour(&self, offset: f32, saturation: f32, value: f32) -> gdk::RGBA {
        self.colour_from(self.hue, offset, saturation, value)
    }

    fn colour_from(&self, hue: f32, offset: f32, saturation: f32, value: f32) -> gdk::RGBA {
        hsv(
            hue + self.palette.spread * offset,
            saturation * self.palette.saturation,
            value,
        )
    }

    /// A preset other than `current`, at random.
    fn other_than(&mut self, current: Preset) -> Preset {
        let others: Vec<Preset> = Preset::ALL.into_iter().filter(|p| *p != current).collect();
        others[self.random.below(others.len())]
    }

    fn advance(&mut self, dt: f32) {
        let f = self.analyzer.frame();
        self.t += f64::from(dt);
        let kick = if f.onset { 0.025 } else { 0.0 };
        self.hue = match self.palette.anchor {
            // around the accent: a slow sway, nudged by the beats
            Some(anchor) => {
                self.wander = (self.wander + dt * 0.03 + 4.0 * kick).fract();
                (anchor + 0.05 * (TAU * self.wander).sin()).rem_euclid(1.0)
            }
            None => (self.hue + dt * 0.012 + kick).fract(),
        };
        self.turn = (self.turn + dt * (0.12 + 0.5 * f.mid)) % TAU;
        while self
            .echoes
            .front()
            .is_some_and(|e| self.t - e.born > ECHO_LIFE_S)
        {
            self.echoes.pop_front();
        }
        if self.t - self.last_echo >= ECHO_EVERY_S {
            self.last_echo = self.t;
            let mut wave = [0.0; ECHO_POINTS];
            let mirrored = mirrored_wave(f);
            for (j, w) in wave.iter_mut().enumerate() {
                *w = mirrored[j * mirrored.len() / ECHO_POINTS];
            }
            self.echoes.push_back(Echo {
                born: self.t,
                hue: self.hue,
                turn: self.turn,
                energy: f.energy(),
                wave,
            });
        }
        for (peak, &level) in self.peaks.iter_mut().zip(&f.bands) {
            *peak = level.max(*peak - dt * 0.35);
        }
        self.speed = 0.12 + 0.9 * f.energy() + 1.4 * f.beat;
        if self.stars.is_empty() {
            for _ in 0..STARS {
                let z = 0.05 + 0.95 * self.random.unit();
                let star = self.random.star(z);
                self.stars.push(star);
            }
        }
        for i in 0..self.stars.len() {
            self.stars[i].z -= self.speed * dt;
            if self.stars[i].z <= 0.03 {
                self.stars[i] = self.random.star(1.0);
            }
        }
    }

    fn draw(&self, preset: Preset, w: f32, h: f32) -> Option<gdk::Paintable> {
        let snap = gtk::Snapshot::new();
        let full = graphene::Rect::new(0.0, 0.0, w, h);
        snap.append_color(&gdk::RGBA::new(0.03, 0.025, 0.06, 1.0), &full);
        match preset {
            Preset::Tunnel => self.tunnel(&snap, w, h),
            Preset::Nebula => self.nebula(&snap, w, h),
            Preset::Spectrum => self.spectrum(&snap, w, h),
            Preset::Starfield => self.starfield(&snap, w, h),
        }
        snap.to_paintable(Some(&graphene::Size::new(w, h)))
    }

    /// Colour drifting behind: one patch per range (and more for a fuller
    /// sky), each swelling with its range, `strength` times as bright.
    fn drift(&self, snap: &gtk::Snapshot, w: f32, h: f32, patches: usize, strength: f32) {
        let f = self.analyzer.frame();
        let t = self.t as f32;
        let full = graphene::Rect::new(0.0, 0.0, w, h);
        let levels = [f.bass, f.mid, f.treble];
        for i in 0..patches {
            let level = levels[i % 3];
            let i = i as f32;
            let x = w * (0.5 + 0.34 * (t * 0.05 * (i + 1.3) + i * 2.1).sin());
            let y = h * (0.5 + 0.3 * (t * 0.037 * (i + 1.7) + i * 1.3).cos());
            let r = w.max(h) * (0.36 + 0.2 * level);
            let colour = self.colour(i * 0.12, 0.7, 1.0);
            glow(
                snap,
                &full,
                x,
                y,
                r,
                colour,
                strength * (0.10 + 0.32 * level),
            );
        }
    }

    fn tunnel(&self, snap: &gtk::Snapshot, w: f32, h: f32) {
        let f = self.analyzer.frame();
        self.drift(snap, w, h, 3, 1.0);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let radius = 0.2 * w.min(h);

        // The heart: a flare on every beat.
        let core = radius * 1.7 * (1.0 + 0.25 * f.beat);
        glow(
            snap,
            &around(cx, cy, core),
            cx,
            cy,
            core,
            self.colour(0.5, 0.75, 0.9),
            0.04 + 0.22 * f.beat + 0.08 * f.bass,
        );

        // Echoes, oldest (largest, faintest) first.
        for echo in &self.echoes {
            let age = (self.t - echo.born) as f32;
            let left = 1.0 - age / ECHO_LIFE_S as f32;
            let zoom = (age * ZOOM_RATE).exp();
            let alpha = left.powf(1.4) * (0.35 + 0.65 * echo.energy.min(1.0));
            let size = (1.6 + 1.3 * zoom.sqrt()).min(3.6);
            for (j, value) in echo.wave.iter().enumerate() {
                let angle = TAU * j as f32 / ECHO_POINTS as f32 + echo.turn;
                let r = radius * zoom * (1.0 + 0.35 * value);
                let colour =
                    self.colour_from(echo.hue, 0.2 * j as f32 / ECHO_POINTS as f32, 0.75, 1.0);
                dot(
                    snap,
                    cx + r * angle.cos(),
                    cy + r * angle.sin(),
                    size,
                    colour,
                    alpha,
                );
            }
        }

        // The spectrum: points of light around the ring, bass at the bottom,
        // the left a mirror of the right.
        for (b, &level) in f.bands.iter().enumerate() {
            if level < 0.02 {
                continue;
            }
            let along = PI * (b as f32 + 0.5) / BANDS as f32;
            let r = radius * (1.3 + 0.55 * level);
            let size = radius * (0.04 + 0.14 * level);
            let colour = self.colour(0.35 * b as f32 / BANDS as f32, 0.6, 1.0);
            for angle in [PI / 2.0 + along, PI / 2.0 - along] {
                let (x, y) = (cx + r * angle.cos(), cy + r * angle.sin());
                glow(
                    snap,
                    &around(x, y, size),
                    x,
                    y,
                    size,
                    colour,
                    0.2 + 0.75 * level,
                );
            }
        }

        // The ring itself: the waveform, with a soft glow under it.
        let wave = mirrored_wave(f);
        let points = wave.len();
        let ring = |snap: &gtk::Snapshot, size: f32, colour: gdk::RGBA, alpha: f32| {
            for (j, value) in wave.iter().enumerate() {
                let angle = TAU * j as f32 / points as f32 + self.turn;
                let r = radius * (1.0 + 0.35 * value);
                dot(
                    snap,
                    cx + r * angle.cos(),
                    cy + r * angle.sin(),
                    size,
                    colour,
                    alpha,
                );
            }
        };
        snap.push_blur(6.0);
        ring(snap, 5.0, self.colour(0.0, 0.8, 1.0), 0.8);
        snap.pop();
        ring(snap, 2.4, self.colour(0.0, 0.15, 1.0), 0.95);
    }

    fn nebula(&self, snap: &gtk::Snapshot, w: f32, h: f32) {
        let f = self.analyzer.frame();
        self.drift(snap, w, h, 5, 1.5);
        let base = h * 0.62;
        let lift = h * 0.4;
        let amplitude = h * 0.2;
        // Earlier waves rise and thin out like smoke.
        for echo in &self.echoes {
            let k = ((self.t - echo.born) / ECHO_LIFE_S) as f32;
            let alpha = (1.0 - k).powf(1.3) * (0.35 + 0.6 * echo.energy.min(1.0));
            let y0 = base - k * lift;
            let spread = 1.0 + 0.6 * k;
            for (j, value) in echo.wave.iter().enumerate() {
                let x = w * (j as f32 / (ECHO_POINTS - 1) as f32);
                let colour = self.colour_from(echo.hue, 0.15 * k, 0.6, 1.0);
                dot(snap, x, y0 + value * amplitude * spread, 3.0, colour, alpha);
            }
        }
        let wave = mirrored_wave(f);
        let points = wave.len();
        let line = |size: f32, colour: gdk::RGBA, alpha: f32| {
            for (j, value) in wave.iter().enumerate() {
                let x = w * (j as f32 / (points - 1) as f32);
                dot(snap, x, base + value * amplitude, size, colour, alpha);
            }
        };
        snap.push_blur(8.0);
        line(6.0, self.colour(0.1, 0.8, 1.0), 0.7 + 0.3 * f.beat);
        snap.pop();
        line(2.2, self.colour(0.0, 0.2, 1.0), 0.9);
    }

    fn spectrum(&self, snap: &gtk::Snapshot, w: f32, h: f32) {
        let f = self.analyzer.frame();
        let horizon = h * 0.64;
        let tall = h * 0.46;
        // A sky that warms toward the horizon, brighter on a beat.
        let tint = self.colour(0.0, 0.7, 0.8);
        snap.append_linear_gradient(
            &graphene::Rect::new(0.0, 0.0, w, h),
            &graphene::Point::new(0.0, 0.0),
            &graphene::Point::new(0.0, h),
            &[
                gsk::ColorStop::new(0.0, tint.with_alpha(0.0)),
                gsk::ColorStop::new(0.64, tint.with_alpha(0.22 + 0.2 * f.beat)),
                gsk::ColorStop::new(1.0, tint.with_alpha(0.0)),
            ],
        );
        self.drift(snap, w, h, 2, 0.6);
        // Bass in the middle, the highs out to both sides.
        let slots = 2 * BANDS;
        let slot = w * 0.84 / slots as f32;
        let width = slot * 0.62;
        let left = w * 0.08 + (slot - width) / 2.0;
        let bar = |k: usize| {
            let band = if k < BANDS { BANDS - 1 - k } else { k - BANDS };
            (left + k as f32 * slot, band)
        };
        let colour = |band: usize| self.colour(0.4 * band as f32 / BANDS as f32, 0.7, 1.0);
        snap.push_blur(10.0);
        for k in 0..slots {
            let (x, band) = bar(k);
            let height = 2.0 + f.bands[band] * tall;
            let rect = graphene::Rect::new(x, horizon - height, width, height);
            snap.append_color(&colour(band).with_alpha(0.75), &rect);
        }
        snap.pop();
        for k in 0..slots {
            let (x, band) = bar(k);
            let c = colour(band);
            let height = 2.0 + f.bands[band] * tall;
            let top = horizon - height;
            snap.append_linear_gradient(
                &graphene::Rect::new(x, top, width, height),
                &graphene::Point::new(x, top),
                &graphene::Point::new(x, horizon),
                &[
                    gsk::ColorStop::new(0.0, self.colour(0.0, 0.15, 1.0)),
                    gsk::ColorStop::new(0.35, c),
                    gsk::ColorStop::new(1.0, c.with_alpha(0.85)),
                ],
            );
            // the mirror under the horizon, fading
            let below = height * 0.5;
            snap.append_linear_gradient(
                &graphene::Rect::new(x, horizon + 2.0, width, below),
                &graphene::Point::new(x, horizon + 2.0),
                &graphene::Point::new(x, horizon + 2.0 + below),
                &[
                    gsk::ColorStop::new(0.0, c.with_alpha(0.3)),
                    gsk::ColorStop::new(1.0, c.with_alpha(0.0)),
                ],
            );
            // the highest it reached lately, falling slowly
            let cap = horizon - 2.0 - self.peaks[band] * tall - 5.0;
            snap.append_color(
                &self.colour(0.0, 0.1, 1.0).with_alpha(0.85),
                &graphene::Rect::new(x, cap, width, 2.0),
            );
        }
        snap.append_color(
            &gdk::RGBA::new(1.0, 1.0, 1.0, 0.18),
            &graphene::Rect::new(0.0, horizon + 0.5, w, 1.0),
        );
    }

    fn starfield(&self, snap: &gtk::Snapshot, w: f32, h: f32) {
        let f = self.analyzer.frame();
        self.drift(snap, w, h, 3, 0.5);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let core = 0.12 * w.min(h) * (1.0 + 0.6 * f.beat);
        glow(
            snap,
            &around(cx, cy, core),
            cx,
            cy,
            core,
            self.colour(0.0, 0.45, 1.0),
            0.1 + 0.4 * f.beat,
        );
        let reach = 0.5 * w.max(h);
        // a streak behind each star, longer the faster they fly
        let trail = (self.speed * 0.03).min(0.06);
        for (i, star) in self.stars.iter().enumerate() {
            let near = 1.0 - star.z;
            let colour = self.colour((i % 9) as f32 * 0.04, 0.35, 1.0);
            let size = 1.2 + 3.6 * near * near;
            for (step, fade) in [(3.0, 0.15), (2.0, 0.3), (1.0, 0.55), (0.0, 1.0)] {
                let z = star.z + trail * step;
                let (x, y) = (cx + star.x / z * reach * 0.5, cy + star.y / z * reach * 0.5);
                if x < -8.0 || x > w + 8.0 || y < -8.0 || y > h + 8.0 {
                    continue;
                }
                let alpha = fade * (0.3 + 0.7 * near);
                dot(snap, x, y, size * (1.0 - 0.15 * step), colour, alpha);
            }
        }
    }
}

/// What a press on the visualizer does, by how many presses in a row.
#[derive(Debug, PartialEq, Eq)]
enum Click {
    /// The first: play or pause, unless a second follows in time.
    Wait,
    FullScreen,
    /// A third and on: nothing more.
    Nothing,
}

fn on_press(presses: i32) -> Click {
    match presses {
        1 => Click::Wait,
        2 => Click::FullScreen,
        _ => Click::Nothing,
    }
}

/// How long GNOME waits for the second click of a double click.
fn double_click_time() -> Duration {
    let ms = gtk::Settings::default().map_or(400, |s| s.gtk_double_click_time());
    Duration::from_millis(ms.clamp(100, 1000) as u64)
}

/// A small, fast generator: plenty for scattering stars and picking presets.
struct Random(u32);

impl Random {
    fn seeded() -> Random {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos());
        Random(nanos | 1)
    }

    fn next(&mut self) -> u32 {
        // xorshift32
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// 0–1.
    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1 << 24) as f32
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }

    /// A star somewhere ahead, `z` away.
    fn star(&mut self, z: f32) -> Star {
        Star {
            x: 2.0 * self.unit() - 1.0,
            y: 2.0 * self.unit() - 1.0,
            z,
        }
    }
}

/// The least time between two frames drawn, in microseconds.
fn frame_gap_us(power_saver: bool) -> i64 {
    if power_saver {
        MIN_FRAME_SAVING_US
    } else {
        MIN_FRAME_US
    }
}

/// The waveform around a whole ring: once down one side, mirrored up the
/// other, so the ends meet.
fn mirrored_wave(f: &Frame) -> [f32; 2 * WAVE] {
    let mut ring = [0.0; 2 * WAVE];
    // smoothed a little: the hi-hats otherwise fray the line
    let smooth: [f32; WAVE] = std::array::from_fn(|j| {
        let near = j.saturating_sub(2)..(j + 3).min(WAVE);
        let n = near.len() as f32;
        near.map(|k| f.wave[k]).sum::<f32>() / n
    });
    for (j, &value) in smooth.iter().enumerate() {
        ring[j] = value;
        ring[2 * WAVE - 1 - j] = value;
    }
    ring
}

/// A square around (x, y) reaching `r` each way.
fn around(x: f32, y: f32, r: f32) -> graphene::Rect {
    graphene::Rect::new(x - r, y - r, 2.0 * r, 2.0 * r)
}

/// Light fading out from (x, y) to nothing at `r`, within `bounds`.
fn glow(
    snap: &gtk::Snapshot,
    bounds: &graphene::Rect,
    x: f32,
    y: f32,
    r: f32,
    colour: gdk::RGBA,
    alpha: f32,
) {
    if r <= 0.0 {
        return;
    }
    let stops = [
        gsk::ColorStop::new(0.0, colour.with_alpha(alpha.clamp(0.0, 1.0))),
        gsk::ColorStop::new(0.45, colour.with_alpha((alpha * 0.35).clamp(0.0, 1.0))),
        gsk::ColorStop::new(1.0, colour.with_alpha(0.0)),
    ];
    snap.append_radial_gradient(bounds, &graphene::Point::new(x, y), r, r, 0.0, 1.0, &stops);
}

fn dot(snap: &gtk::Snapshot, x: f32, y: f32, size: f32, colour: gdk::RGBA, alpha: f32) {
    let half = size / 2.0;
    snap.append_color(
        &colour.with_alpha(alpha.clamp(0.0, 1.0)),
        &graphene::Rect::new(x - half, y - half, size, size),
    );
}

/// A colour from hue (0–1, wrapping), saturation and value.
fn hsv(hue: f32, saturation: f32, value: f32) -> gdk::RGBA {
    let h = hue.rem_euclid(1.0) * 6.0;
    let c = value * saturation;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    gdk::RGBA::new(r + m, g + m, b + m, 1.0)
}

/// Hue (0–1), saturation and value of a colour.
fn to_hsv(colour: gdk::RGBA) -> (f32, f32, f32) {
    let (r, g, b) = (colour.red(), colour.green(), colour.blue());
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let range = max - min;
    let hue = if range == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / range).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / range + 2.0) / 6.0
    } else {
        ((r - g) / range + 4.0) / 6.0
    };
    let saturation = if max == 0.0 { 0.0 } else { range / max };
    (hue, saturation, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_goes_to_hsv_and_back() {
        for hue in [0.0, 0.1, 0.3, 0.55, 0.8, 0.95] {
            let colour = hsv(hue, 0.6, 0.9);
            let (h, s, v) = to_hsv(colour);
            assert!((h - hue).abs() < 1e-4, "{hue} came back as {h}");
            assert!((s - 0.6).abs() < 1e-4 && (v - 0.9).abs() < 1e-4);
        }
    }

    #[test]
    fn around_an_accent_the_hue_stays_close_to_it() {
        // GNOME's blue accent
        let blue = gdk::RGBA::new(0.208, 0.518, 0.894, 1.0);
        let mut scene = Scene::new();
        scene.palette = Palette::around(blue);
        let (anchor, ..) = to_hsv(blue);
        for _ in 0..600 {
            scene.advance(1.0 / 60.0);
            let off = (scene.hue - anchor).abs();
            assert!(
                off.min(1.0 - off) <= 0.051,
                "{} strayed from {anchor}",
                scene.hue
            );
        }
        // a slate accent greys everything a little
        let slate = Palette::around(gdk::RGBA::new(0.435, 0.514, 0.588, 1.0));
        assert!(slate.saturation < 0.5);
    }

    /// How many of a second's display frames get drawn.
    fn drawn_per_second(refresh_hz: i64, power_saver: bool) -> usize {
        let frame_us = 1_000_000 / refresh_hz;
        let mut last = None;
        (0..refresh_hz)
            .filter(|i| {
                let now = i * frame_us;
                let draw = last.is_none_or(|l| now - l >= frame_gap_us(power_saver));
                if draw {
                    last = Some(now);
                }
                draw
            })
            .count()
    }

    #[test]
    fn about_60_frames_a_second_and_30_saving_power() {
        assert_eq!(drawn_per_second(60, false), 60);
        assert_eq!(drawn_per_second(60, true), 30);
        assert_eq!(drawn_per_second(120, false), 60);
        assert_eq!(drawn_per_second(120, true), 30);
    }

    #[test]
    fn a_click_waits_a_double_click_goes_full_screen() {
        assert_eq!(on_press(1), Click::Wait);
        assert_eq!(on_press(2), Click::FullScreen);
        assert_eq!(on_press(3), Click::Nothing);
    }

    #[test]
    fn hues_go_round() {
        let red = hsv(0.0, 1.0, 1.0);
        assert_eq!((red.red(), red.green(), red.blue()), (1.0, 0.0, 0.0));
        let green = hsv(1.0 / 3.0, 1.0, 1.0);
        assert!(green.green() > 0.99 && green.red() < 0.01);
        assert_eq!(hsv(1.25, 0.5, 0.8), hsv(0.25, 0.5, 0.8));
        let grey = hsv(0.7, 0.0, 0.5);
        assert_eq!((grey.red(), grey.green(), grey.blue()), (0.5, 0.5, 0.5));
    }

    #[test]
    fn the_ring_closes_on_itself() {
        let mut frame = Frame::default();
        for (j, w) in frame.wave.iter_mut().enumerate() {
            *w = j as f32 / WAVE as f32;
        }
        let ring = mirrored_wave(&frame);
        assert_eq!(ring[0], ring[2 * WAVE - 1]);
        assert_eq!(ring[WAVE - 1], ring[WAVE]);
    }

    #[test]
    fn echoes_are_left_behind_and_fade_away() {
        let mut scene = Scene::new();
        for _ in 0..60 {
            scene.advance(1.0 / 60.0);
        }
        let after_one_second = scene.echoes.len();
        assert!((11..=14).contains(&after_one_second), "{after_one_second}");
        for _ in 0..240 {
            scene.advance(1.0 / 60.0);
        }
        // never more than a lifetime's worth
        let most = (ECHO_LIFE_S / ECHO_EVERY_S).ceil() as usize + 1;
        assert!(scene.echoes.len() <= most);
        assert!(scene.echoes.iter().all(|e| scene.t - e.born <= ECHO_LIFE_S));
    }
}

#[cfg(test)]
mod look {
    use super::*;
    use gtk::prelude::GskRendererExt;

    /// Something like music: a kick on every beat, a bass line, a chord and
    /// a hi-hat, `seconds` in.
    fn music(seconds: f32) -> Vec<f32> {
        let rate = SAMPLE_RATE as f32;
        (0..WINDOW)
            .map(|i| {
                let t = seconds + i as f32 / rate;
                let beat = t % 0.5;
                let kick =
                    (-beat * 18.0).exp() * (TAU * (50.0 + 80.0 * (-beat * 30.0).exp()) * t).sin();
                let bass = 0.25 * (TAU * 55.0 * t).sin();
                let chord: f32 = [220.0, 277.0, 330.0, 440.0]
                    .iter()
                    .map(|f| 0.06 * (TAU * f * t).sin())
                    .sum();
                let hat = if (t % 0.25) < 0.03 {
                    0.05 * ((t * 91_237.0).sin() * 43_758.5).fract()
                } else {
                    0.0
                };
                0.6 * kick + bass + chord + hat
            })
            .collect()
    }

    #[test]
    #[ignore = "times a frame of each preset; run with --release"]
    fn times_a_frame() {
        gtk::init().unwrap();
        for preset in Preset::ALL {
            let mut scene = Scene::new();
            let frames = 600;
            let (mut analysis, mut drawing) = (Duration::ZERO, Duration::ZERO);
            for f in 0..frames {
                let samples = music(f as f32 / 60.0);
                let start = Instant::now();
                scene.analyzer.update(&samples, 1.0 / 60.0);
                scene.advance(1.0 / 60.0);
                let analysed = Instant::now();
                let paintable = scene.draw(preset, 1600.0, 900.0);
                drawing += analysed.elapsed();
                analysis += analysed - start;
                drop(paintable);
            }
            println!(
                "{:<10} analysis {:>4} µs  drawing {:>4} µs per frame",
                preset.name(),
                analysis.as_micros() / frames,
                drawing.as_micros() / frames
            );
        }
    }

    #[test]
    #[ignore = "renders each preset to /tmp/descant-visualizer-*.png to look at"]
    fn renders_frames_to_look_at() {
        gtk::init().unwrap();
        let mut scene = Scene::new();
        let renderer = gsk::CairoRenderer::new();
        renderer.realize(None).unwrap();
        for f in 0..180 {
            let t = f as f32 / 60.0;
            scene.analyzer.update(&music(t), 1.0 / 60.0);
            scene.advance(1.0 / 60.0);
            if f == 120 {
                // from here on around GNOME's blue accent
                scene.palette = Palette::around(gdk::RGBA::new(0.208, 0.518, 0.894, 1.0));
            }
            if f == 151 {
                for preset in Preset::ALL {
                    let paintable = scene.draw(preset, 1024.0, 600.0).unwrap();
                    let snap = gtk::Snapshot::new();
                    paintable.snapshot(&snap, 1024.0, 600.0);
                    let node = snap.to_node().unwrap();
                    let texture = renderer.render_texture(&node, None);
                    texture
                        .save_to_png(format!("/tmp/descant-visualizer-{}.png", preset.id()))
                        .unwrap();
                }
            }
        }
        renderer.unrealize();
    }
}
