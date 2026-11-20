# alea IDE

A studio for [alea](https://github.com/sudo-bngz/alea), the language for
generative art. Write a work on the left, see its edition in the middle,
and reroll the seed until one is worth keeping.

![The alea IDE showing Rosace](docs/screenshot.png)

> Status: first scaffold. It shows `render image` works. Sound, MIDI, video
> and sculpture viewports, the draws panel and drawing on the canvas are
> planned but not there yet.

## What it does

- **A tab is a work at a seed.** `+` opens the same work in a second tab at
  another seed; both tabs share the text.
- **Live preview.** The edition redraws as you type: a quick rough pass, then
  one at the size of the viewport. A mistake keeps the last good image,
  dimmed, and shows alea's own diagnostic under the code.
- **Reroll.** `Space` (when the code is not focused) or `Ctrl/Cmd+R` draws a
  new edition. The strip at the bottom remembers where the tab has been;
  click a thumbnail to go back, or type a seed to return to a known edition.
  Thumbnails drawn from older code are faded.
- **Keep.** Kept editions are never dropped from the history.
- **Export PNG.** Writes the edition on screen at the size the work asks
  for, named `<work>-<seed>.png` like the command line does.

The same seed draws the same edition here and with `alea work.alea --seed N`.

## Run

Requires a Rust toolchain (1.95+). On Linux you also need the usual
windowing libraries (`libxkbcommon`, `libwayland`, `libgl`).

```sh
cargo run --release                  # opens a starter work
cargo run --release -- rosace.alea   # or a file
```

## Keys

| Key | Action |
| --- | --- |
| `Space` / `Ctrl/Cmd+R` | Reroll |
| `Ctrl/Cmd+[` / `]` | Previous / next edition |
| `Ctrl/Cmd+T` | This work in a new tab, at another seed |
| `Ctrl/Cmd+W` | Close tab |
| `Ctrl/Cmd+O` / `S` | Open / save |
| `Ctrl/Cmd+E` | Export PNG |

## How it is built

- [`alea-lang`](https://github.com/sudo-bngz/alea) is the engine: the IDE
  parses the text on screen, resolves it at the tab's seed and paints the
  result (`src/engine.rs`, on a worker thread that always draws the latest
  request).
- [`qell-core`](https://github.com/sudo-bngz/qell) provides the alea syntax
  highlighting.
- [egui](https://github.com/emilk/egui) draws the window (`src/app.rs`).

## Licence

MIT
