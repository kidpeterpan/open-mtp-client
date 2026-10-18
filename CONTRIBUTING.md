# Contributing to open-mtp-client

Thanks for helping. This is a Mac app for moving files to and from Android phones over MTP, so the most
valuable contributions are the ones that make connecting and copying more reliable on real phones.

## Ways to help

- **Report a phone.** "Works on a Pixel 8 / Galaxy S24 / Xiaomi 14" and "doesn't connect on X" both help.
  Open **Help › About**, read the diagnostics report (it contains no file names, no photos and no account
  details), and paste it into an [issue](https://github.com/kidpeterpan/open-mtp-client/issues/new).
- **Fix a bug or build a planned feature** from the "planned" rows of the README's feature table.
  For anything bigger than a small fix, open an issue first so we agree on the shape before you write it.
- **Improve the docs**, especially the troubleshooting steps.

## Set up

You need a Mac (the app uses AppKit), Apple's command line tools and Rust 1.90 or newer.

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add rustfmt clippy

git clone https://github.com/kidpeterpan/open-mtp-client.git
cd open-mtp-client
make run-virtual
```

`make run-virtual` starts the app against a fake phone backed by the `.virtual-phone/` folder, so you
need no hardware. `make run-virtual-two` plugs in two fake phones. Real-phone testing is welcome, but
**never run it on a phone with data you can't afford to lose**: this is early software.

## The checks a change must pass

CI runs these on every pull request; run them before you push.

```sh
make lint           # cargo fmt --check, then clippy with warnings as errors
make test           # the whole suite
make coverage       # line coverage must stay at or above 75% (needs: cargo install cargo-llvm-cov)
make test-vendor    # only if you touched vendor/mtp-rs
```

Every behaviour gets a test at the layer it lives in. The suite runs against the virtual phone, so a test
never needs a device, never plays a real sound, never opens a real window of System Settings and never
touches the real Trash: those go through small seams (`Opener`, `Trash`, `Processes`, `Usb`…) with a
recorder or fake in tests. Follow the same pattern for anything new that reaches outside the process.

## How the code is laid out

```
cmd/open-mtp-client/       the app: iced UI, one handler per screen area, wiring
cmd/open-mtp-client-cli/   the command-line tool, built on the same services
internal/dto/              plain structs and errors; no serde, no UI types
internal/service/<domain>/ business logic (service.rs) and client→dto mapping (mapper.rs)
vendor/mtp-rs/             a patched copy of the mtp-rs crate (see vendor/README.md)
```

The rules that keep it tidy:

- Handlers never import `mtp_rs`; services never import `iced`. Only `dto` crosses the boundary.
- A handler talks to its service through a trait; the concrete implementation is in `wire.rs`.
- Every error a user can act on is a `dto::Error` with a title and a hint, not a raw string.

[CLAUDE.md](CLAUDE.md) goes much deeper: the architecture, performance rules and the reasoning behind
many details. Read the part that matches what you are changing.

## Things that are easy to get wrong

- **Every phone request is a real USB round trip** (about 5 ms). Don't put a whole-folder listing on an
  edit or refresh path, and check new features against a folder of 5,000+ files on a release build
  (`make run`, not a debug build).
- **Cancelling an in-flight read can wedge an Android phone.** Wrap long operations in a timeout and
  prefer windowed reads.
- **A phone is untrusted input.** Treat every size, name and date it sends as hostile.
- **No network code.** The app has no analytics and a test keeps it that way. Opening a web page goes
  through the system browser, from a fixed list of links.
- **File names are never rewritten.** Names that exist are copied exactly as they are.

## Pull requests

1. Branch from `main`, keep the change focused, and keep the diff free of unrelated formatting.
2. Make `make lint test coverage` pass.
3. Say in the description what you tried it on (the virtual phone is fine; name the real phone if you
   used one).
4. Include a screenshot for visible changes.

By contributing you agree that your contribution is licensed under the [MIT License](LICENSE).

## Conduct

Be kind and assume good intent. Harassment isn't tolerated; maintainers may remove comments or contributors
who don't follow this.
