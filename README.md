# Rustranscript

Records your calls, transcribes them and tells the speakers apart. Everything
happens on your machine: audio and text never leave your computer.

*Em português: o app também tem interface em pt-BR (Configurações > Idioma).*

Linux only (PulseAudio or PipeWire).

## Install

**AppImage (any distro)**

1. Download `Rustranscript_*.AppImage` from the [Releases](https://github.com/petebarbosa/Rustranscript/releases) page.
2. `chmod +x Rustranscript_*.AppImage`, then run it.
3. Optional, to get the `rstt` command in your terminal:
   `ln -s /path/to/Rustranscript.AppImage ~/.local/bin/rstt`

**Arch Linux**

```
cd packaging/arch && makepkg -si
```

## First run

Transcription needs an engine and speech models (about 1.7 GB). They are not
bundled: open **Queue** (or Settings) and click **Download and install**
(*Baixar e instalar* in pt-BR), or run `rstt setup install`. This happens once.
Recording works without it, and finished recordings wait in the queue.

## Record

- Press **Ctrl+Alt+R** to start and stop (change it in Settings).
- Or from a terminal: `rstt record start` and `rstt record stop`
  (`rstt record toggle` does both). `rstt --help` lists everything.
- Wayland (e.g. Hyprland): global shortcuts are not available to apps. Open
  **Settings**, copy the snippet shown there into your compositor config, and
  it will call `rstt record toggle`.

Your microphone and the other side of the call are recorded separately, which
is how speakers are told apart.

## Where your data lives

`~/.local/share/rustranscript` holds recordings, transcripts and the
transcription engine. Remove the folder to erase everything.

## Build from source

You need Rust, Node.js/npm and the development packages for WebKitGTK 4.1,
GTK 3 and libpulse.

```
npm ci
npx tauri build --no-bundle   # binary at target/release/rstt
npx tauri build               # AppImage in target/release/bundle/appimage/
```

## License

MIT, see [LICENSE](LICENSE).
