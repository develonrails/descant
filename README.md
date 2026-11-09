<p align="center">
  <img src="data/icons/io.github.develonrails.Descant.svg" width="128" alt="">
</p>

<h1 align="center">Descant</h1>

<p align="center">A small, fast, native GNOME client for Spotify.</p>

Descant plays your music and gets out of the way. It is written in Rust with
GTK 4 and libadwaita, plays through [librespot](https://github.com/librespot-org/librespot),
and is built to be light on your computer and to keep playing when other
clients go quiet.

<p align="center">
  <img src="docs/screenshots/home.png" alt="Descant's Home in the dark style: quick tiles, Made for you, and the now-playing bar">
</p>

## What Descant stands for

**Frugal.** Every feature has to earn its memory. At rest, with Home open,
Descant uses about 32 MB of memory of its own and 17 threads, and no CPU at
all: it wakes up when you do something. Track lists only build the rows you
can see, episodes come twenty at a time, covers are kept at exactly the
pixels they fill on your screen, and every cache has a cap. Podcasts cost
nothing until you open one.

**Sound that keeps playing.** Spotify rations the keys that unlock each
track. Clients that ignore this start skipping tracks or playing silence.
Descant paces itself, waits out a throttle instead of failing, reconnects on
its own after sleep or a network change, and picks up where you left off the
next time you open it.

**Native.** Descant is a GNOME app, not a web page in a window. It follows the
GNOME Human Interface Guidelines, adapts to narrow windows, follows your light
or dark style and accent colour, keeps its settings in GSettings, works with
media keys and the shell's media controls (MPRIS), and ships as a Flatpak.

**Focused.** Playlists, podcasts, search, discovery, your library and
playback, done well. No skins, no visualizers, no friend feed, no news, no
audiobooks. A smaller app has fewer places for bugs to hide.

**Private.** Descant has no servers, no telemetry and no developer key of its
own. It talks to Spotify with your own session and nothing else. Your sign-in
lives in the system keyring, and audio is streamed, never stored.

## Features

- **Sign in with your phone:** scan a QR code, confirm in the Spotify app.
- **Home:** quick tiles, Continue listening and New episodes, Made for You,
  Recently Played, your top artists and songs.
- **Search** for songs, artists, albums, playlists, podcasts and episodes, or
  paste a Spotify link.
- **Podcasts:** follow shows, save episodes to Your Episodes, and go on where
  you left off; jump back 15 seconds or ahead 30.
- **Your library:** Liked Songs (newest first), saved albums, followed
  artists and your playlists. Like tracks, create playlists, add and remove
  tracks.
- **Playback:** queue, shuffle, repeat and volume. Descant remembers the track
  and the moment you stopped at.
- **Spotify Connect:** Descant shows up as a device you can play to from your
  phone, songs and episodes alike.
- **Media controls:** play, skip, seek and set the volume from the media keys,
  GNOME's media controls, GSConnect or `playerctl` (MPRIS).
- **Ten-band equalizer** with presets, for playback on this computer.
- **Keep playing in the background** when the window is closed, if you want.
- **Keyboard shortcuts:** <kbd>Ctrl</kbd>+<kbd>?</kbd> lists them.

You need a **Spotify Premium** account; librespot cannot play on a free one.

## Install

Descant comes as a Flatpak from its own signed repository on GitHub Pages.
Add it once:

```sh
flatpak remote-add --user --if-not-exists --from \
    descant https://develonrails.github.io/descant/descant.flatpakrepo
flatpak install --user descant io.github.develonrails.Descant
```

After that, `flatpak update` and GNOME Software pick up new builds like any
other Flatpak. A single-file bundle is attached to the
[`continuous` release](https://github.com/develonrails/descant/releases/tag/continuous)
for installing without a remote, but it cannot update itself.

## Building

On Fedora, or in a Fedora toolbox:

```sh
sudo dnf install rust cargo gcc pkgconf-pkg-config \
    gtk4-devel libadwaita-devel pulseaudio-libs-devel openssl-devel \
    libsecret-devel
cargo run
```

The Flatpak builds offline from vendored crates:

```sh
cargo vendor --locked vendor > build-aux/flatpak/cargo-config.toml
flatpak run org.flatpak.Builder --user --install --force-clean \
    build-dir build-aux/flatpak/io.github.develonrails.Descant.yml
```

Run the tests with `cargo test --workspace`; the MPRIS test starts a private
session bus, so it needs `dbus-daemon`. CI also runs `cargo fmt` and
`cargo clippy -- -D warnings`. [CONTRIBUTING](docs/CONTRIBUTING.md) has the
rest: building on Silverblue, trying it against a real account, and how a
build reaches people.

## How it is put together

| | |
|---|---|
| `crates/descant-core` | The rules: when to play, wait, retry or reconnect, as a state machine with the clock passed in, so every case is tested exactly. Also the equalizer, and the podcast rules: where you left off, what is new, what is played. No I/O. |
| `crates/descant-playback` | librespot, driven through Spotify Connect (Spirc). Keeps a failing audio output from taking the app down and hears the key refusals librespot only logs. |
| `crates/descant-spotify` | Your library, Home, search and podcasts, through the same session librespot uses (Spotify's Web API refuses clients like this one). Pairing and the keyring. |
| `src/` | The GTK app, and MPRIS. |

## Thanks

Descant stands on [librespot](https://github.com/librespot-org/librespot), which
does the hard part.

The heart is [Lucide](https://lucide.dev)'s (ISC); its licence is beside it in
`data/icons/actions`.

If Descant is useful to you, you can [buy me a coffee](https://ko-fi.com/apenzoon).

## License

[MIT](LICENSE).

Descant is an independent project. It is not affiliated with, endorsed by or
sponsored by Spotify. Spotify is a trademark of Spotify AB.
