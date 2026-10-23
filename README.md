# IRresponsible

Headphone correction for macOS: find your headphones in AutoEq, load a correction, and compare it with bypass. Includes custom WAV impulses, saved presets, and a live spectrum.

`Mac audio -> BlackHole -> IRresponsible -> headphones / audio interface`

![IRresponsible headphone correction interface](assets/screenshot.png)

## Install

Requires **macOS 13 or later**, audio devices at **44.1 or 48 kHz**, and **[BlackHole 2ch](https://existential.audio/blackhole/)**. Use BlackHole's official installer and restart if prompted, or run `brew install --cask blackhole-2ch`.

Quit IRresponsible before updating, then run:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/zezic/irresponsible/master/scripts/install.sh | bash
```

The installer downloads the latest release for your Mac, verifies its checksum and app signature, installs to `~/Applications`, and removes quarantine. Builds are ad-hoc signed and are not notarized yet. To choose a different folder, append `-s -- --directory /Applications` after `bash`.

You can also download the **arm64** (Apple Silicon) or **x86_64** (Intel) archive from [Releases](https://github.com/zezic/irresponsible/releases).

## First listen

1. Stop the engine if running. Select **BlackHole 2ch** as Capture and your headphones or audio interface as Playback. Allow audio capture if macOS asks.
2. Press **Start**. IRresponsible routes Mac audio through the app automatically.
3. Search for your exact headphone model. **Double-click to load**; a single click previews a comparison. Or import your own WAV impulse.
4. Use **Bypass** to compare. Save a preset with **Cmd+S**. Preferences contains IR settings, spectrum controls, themes, and **Launch at login**.

Closing the window keeps audio running. **Stop** or **Quit** restores the previous output when the app owns the route. If audio goes silent after a crash, choose your headphones or interface in macOS Sound settings.

## Build from source

Install [Rust](https://rust-lang.org/tools/install/) with `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`, then restart your terminal. Install Apple's Command Line Tools with `xcode-select --install` if needed.

```sh
git clone https://github.com/zezic/irresponsible.git
cd irresponsible
bash scripts/package.sh
open dist/IRresponsible.app
```

The packaged app includes its icon, resources, and local signature. Rust is needed to build, not to run it.

[Technical and release reference](SPEC.md) · [AutoEq catalogue and measurements](https://github.com/jaakkopasanen/AutoEq)
