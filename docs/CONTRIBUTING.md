# Building and testing

How to build Descant, and the Flatpak, is in the [README](../README.md#building).

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

On an immutable host such as Fedora Silverblue there is no Rust and there are
no headers; build in a container:

```sh
toolbox create -d fedora -r 44 descant-build
toolbox run -c descant-build sudo dnf install -y rust cargo rustfmt clippy gcc \
    pkgconf-pkg-config gtk4-devel libadwaita-devel pulseaudio-libs-devel openssl-devel \
    libsecret-devel \
    dbus-daemon
toolbox run -c descant-build cargo test --workspace
```

`dbus-daemon` is for the MPRIS test, which starts a private session bus and
talks to Descant over it the way the shell does.

## Where the tests are

The decisions live in `descant-core` and `descant-spotify`, and that is where
the tests are: the supervisor takes the clock as an argument, so every retry,
wait and reconnect is tested to the millisecond, and the data layer is tested
on the shapes Spotify actually sends. The GTK app in `src/` stays thin, so
there is little in it to get wrong.

## Trying it against a real account

The examples in `crates/*/examples` run the real code without a UI. They use
the credentials stored by the playback spike, so log in once:

```sh
cd spikes/s1-playback
cargo run --release -- login    # enter the code at spotify.com/pair
cd ../..
cargo run -p descant-playback --example play
cargo run -p descant-spotify --example search -- radiohead
```

**Mind the audio-key budget.** Spotify hands out about 24 track keys in a row
and then roughly one every 30–35 seconds, per account. Skip through a playlist
too fast (`s1-playback storm` does exactly that) and every client on that
account, the official ones included, may be unable to play for a few minutes.

`s1-playback connect` takes over the account's active device, so stop whatever
is playing elsewhere first.

## CI

Two jobs. `test` runs formatting, lints and the tests in a Fedora container,
because the runner's Ubuntu does not ship the GTK and libadwaita Descant links.
`flatpak` builds the app on pushes to `main`, keeping both the OSTree repository
it exports and the single-file bundle made from it.

The release job waits on CI rather than running alongside it: a build published
from a commit whose tests then failed is worse than no build at all.

## How a build reaches people

A green run on `main` promotes that build into the Flatpak repository served
from GitHub Pages, and replaces the bundle on the rolling `continuous` release.
The repository is the route people install from; the bundle is a fallback. A
bundle is a file, not a remote, so `flatpak update` has nothing to pull from,
and GNOME Software refuses it outright.

The `gh-pages` branch is force-pushed as a fresh orphan commit each time. The
repository keeps its own history inside OSTree, so a git history of it as well
would grow every clone without bound and serve nobody.

### The signing key

Builds are signed, and the public half travels inside `descant.flatpakrepo`, so
adding the remote is also what establishes trust. The private half lives in the
`FLATPAK_GPG_PRIVATE_KEY` repository secret, and nothing publishes without it:
the release job stops if it is unset.

To create one, or to roll it:

```sh
export GNUPGHOME=$(mktemp -d) && chmod 700 "$GNUPGHOME"

gpg --batch --gen-key <<KEY
%no-protection
Key-Type: eddsa
Key-Curve: Ed25519
Key-Usage: sign
Name-Real: descant repository signing
Name-Email: descant@develonrails.github.io
Expire-Date: 0
%commit
KEY

gpg --export-secret-keys --armor | base64 -w0 |
    gh secret set FLATPAK_GPG_PRIVATE_KEY --repo develonrails/descant
```

Rolling it re-signs everything on the next run, but a client that already
trusts the old key will refuse the new signatures: `flatpak remote-delete
descant` and add it again. Worth avoiding for that reason alone.

## Dependencies

librespot is pinned to a revision on its `dev` branch rather than the 0.8.0
release, which lacks fixes Descant needs. When Spotify breaks librespot, the fix
lands on `dev` first: move the `rev` in `Cargo.toml` for all librespot crates at
once.
