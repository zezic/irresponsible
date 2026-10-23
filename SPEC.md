# IRresponsible technical reference

Current behavior and contracts of the implemented app. See [README.md](README.md) for installation, packaging, and first use.

## Audio routing and lifecycle

- One stereo route connects a capture device, normally BlackHole 2ch, to a selected playback device. Endpoints use stable Core Audio UIDs and validated channel pairs. Overlapping capture/playback pairs on the same device are rejected; unused output channels are silent.
- Processing supports 44.1 and 48 kHz without changing hardware rates. Adaptive sinc resampling bridges device rates and independent clocks.
- The engine starts automatically with the saved or detected route. Mac system output switches to the virtual capture device after the streams start successfully.
- Stop and Quit restore the previous available output only while the app still owns that routing change. A later user output selection takes precedence. An already-routed capture device does not create a restoration lease.
- Closing the window keeps audio running. Bypass switches processing branches without stopping streams.
- The audio owner serializes stream changes and recovery. Transient Core Audio xruns are counted; invalidated streams and device failures enter bounded recovery, followed by an actionable failure and Retry if necessary. Underruns emit silence and playback ramps back in.
- Launch at login is opt-in through General preferences. macOS ServiceManagement owns its registration and approval state; it is read from macOS rather than copied into application settings.
- The displayed **app delay** estimates software buffering and processing delay. Hardware and driver latency are additional. The CLI audio probe leaves system-output selection unchanged.

## Corrections and presets

- The AutoEq catalogue preserves exact headphone variants, measurement providers, recommendations, asset availability, and rig references. Filters can require AutoEq, measurements, both, or any entry. An empty catalogue refreshes automatically.
- Published AutoEq impulses retain their coefficients, gain, and phase. Generated reference targets use compatible measurement/rig data, including the applicable target and bass shelf. Literal Flat means a horizontal response in that measurement reference, rather than perceptual neutrality or bypass.
- Generated corrections use a minimum-phase, real-cepstrum FIR. Their complete validated recipe and source curves are stored with the correction. This is IRresponsible's synthesis policy; it does not claim coefficient parity with published AutoEq impulses.
- Generation defaults are 8192 taps, 500 Hz-2 kHz mean alignment, raw gain, 1/3-octave smoothing, 1-octave treble smoothing above 6 kHz, +6/-18 dB limits, and a 12 dB/octave slope limit. Correction fades in from 10 to 20 Hz and out from 10 to 16 kHz.
- AutoEq treble extends high-frequency correction and attenuates the Nyquist bin instead of using the upper fade to unity. Restore defaults preserves this choice.
- Mono/stereo PCM and floating-point WAV import accepts 8-384 kHz sources, up to 16 MiB and 262144 frames per channel. Mono is duplicated to stereo. Validation rejects malformed or non-finite data. Managed originals preserve gain, channel order, leading silence, and tails; rate conversion preserves transfer gain and declares its added delay.
- Single-click prepares a comparison; double-click loads it. Repeated selection reuses prepared data. Newer requests supersede older preparation, and errors leave the playing correction intact. Playback changes are confirmed by the engine.
- Presets are explicitly saved, named references to managed corrections. Rename and deletion do not change playback. Their source identity survives rate conversion. Restoring a generated preset also restores its recipe and supported target modes; edits prepare a new comparison before replacement.

## DSP and gain

`capture -> clock adaptation -> FIR / delay-aligned bypass -> comparison and headroom gains -> crossfade -> listening volume -> playback`

- Device callbacks exchange stereo frames through bounded rings, handle silence/ramping, and update meters. They do not allocate, block on locks, perform I/O, or destroy filter state. The owned DSP worker performs resampling and convolution; preparation builds replacement engines separately.
- Convolution uses 128-frame FFT partitions. Replacement filters warm up before a 30 ms crossfade. Bypass compensates algorithmic latency and imported leading delay; intentional delay remains part of an imported correction.
- Level matching uses fixed pink-noise weighting over 20 Hz-20 kHz, limited below Nyquist. It attenuates the estimated louder branch. Manual comparison trim adjusts corrected audio relative to bypass; comparison gains do not track the music.
- Shared headroom accounts for correction boost and comparison gains, with a 1 dB margin. It remains fixed when toggling bypass. This is a transfer-response estimate, not a strict transient bound.
- Listening volume is downstream, smoothed, and persisted. BlackHole handles macOS volume keys; the app does not apply their gain a second time.
- There is no hidden clipper, compressor, or music-dependent AGC. Output peaks and overload frames are reported. The IR graph excludes headroom, comparison gain, and listening volume.

## Interface and spectrum

- The winit/Vello/Parley interface uses native measured macOS titlebar geometry and Cordex-style controls. Preferences has General, IR settings, Spectrum, and Presets tabs, with concise delayed hover help.
- Full mode shows the catalogue, route, correction controls, graph, and listening controls. Compact mode gives the content area to the graph. Details starts collapsed. Window sizes, layout mode, and separate light/dark or fixed theme choices persist. Appearance and layout changes do not restart audio.
- Loaded and candidate IRs have separate traces and a replacement overlay. Measured, target, and predicted curves appear when their metadata is available. Prediction is derived from source data, not a measurement of the listener's headphones.
- The spectrum taps input before correction/gain or output after processing. FFT analysis runs on the UI thread over bounded telemetry; overflow drops visualization samples without blocking audio. Gaps and rate changes reset incomplete windows; hidden windows pause capture and analysis.
- Spectrum levels are Hann-windowed stereo mean power with a separate dBFS scale. Opposite-polarity channels do not cancel. Shape-preserving cubic interpolation smooths the log-frequency outline without inventing peaks. Fill blends over the grid, additively on dark themes and with multiply on light themes; IR curves remain above it.
- FFT resolution, time averaging, level range, continuous intensity, and refresh target are configurable. Refresh cadence is independent of FFT size after warm-up. The Cordex/VU scheduler preserves deadline phase, skips missed slots, gates redraws, and backs off for hidden or unavailable surfaces. VSync removes the application timer cap; a target is not a guarantee of physical presentation cadence.

## Persistence and packaging

Versioned settings are replaced atomically. SHA-256-addressed objects store original impulses, catalogue assets, and prepared corrections; reads verify integrity. Cached profiles work offline. Corrupt settings are reported without deleting managed IRs, and catalogue refresh never replaces the playing filter implicitly.

[scripts/package.sh](scripts/package.sh) builds the release executable, bundles the icon, resources, and third-party licenses, and signs the app locally. [assets/app-icon/icon.svg](assets/app-icon/icon.svg) is the editable icon source; [scripts/generate-icon.swift](scripts/generate-icon.swift) exports the PNG and macOS icon set used by packaging.

## Code and verification

| Area | Implementation |
| --- | --- |
| Audio ownership and DSP | [audio.rs](src/audio.rs), [dsp.rs](src/dsp.rs) |
| Correction validation and generation | [correction.rs](src/correction.rs), [generation.rs](src/correction/generation.rs) |
| Catalogue and rig targets | [catalog.rs](src/catalog.rs), [targets.rs](src/catalog/targets.rs) |
| Managed storage and typed values | [storage.rs](src/storage.rs), [domain.rs](src/domain.rs) |
| macOS routing and login items | [platform.rs](src/platform.rs), [login.rs](src/platform/login.rs) |
| UI, spectrum, and pacing | [ui.rs](src/ui.rs), [spectrum.rs](src/spectrum.rs), [frame_schedule.rs](src/frame_schedule.rs) |
| DSP, import, target, and persistence contracts | [contracts.rs](tests/contracts.rs) |

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

Unit and integration tests cover DSP/gain contracts, upstream fixtures, preparation/activation, persistence, UI interactions, spectrum analysis, and frame scheduling. Hardware-dependent tests are explicitly marked ignored for ordinary runs. The `alloc-audit` feature checks callback allocations; CLI probes and offscreen renders exercise native audio and UI paths separately.

## CI and releases

[Build and release](.github/workflows/build.yml) checks formatting and Clippy, runs tests with all features on Apple Silicon and Intel, packages both native apps, and exercises the installer without making network requests or opening the app. Hardware-dependent tests remain excluded. Normal pushes to master, pull requests, and manual runs upload build archives as Actions artifacts.

For a release, update `package.version` in Cargo.toml and its Cargo.lock entry, commit and push, then create and push the matching version tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Replace `0.1.0` with the committed Cargo version. A mismatched tag fails before builds start. Prerelease versions such as `0.2.0-rc.1` produce GitHub prereleases. The macOS marketing version uses the numeric version core; the CI run number supplies the bundle build number. The package's full Cargo version is retained in its info string and network user agent.

Only a tag push publishes a release, after both architecture builds succeed. It uploads versioned `.zip` apps and `SHA256SUMS` using the workflow token; no personal token is needed. Releases are assembled as drafts before publication. Rerunning a failed draft upload is supported; already-published releases are left unchanged. The release job alone receives contents-write permission.

[scripts/install.sh](scripts/install.sh) downloads the latest stable release for the current Mac architecture, verifies its SHA-256 checksum, bundle identity/version, and ad-hoc signature, then removes quarantine from that app and installs it to `~/Applications`. `--directory` overrides the destination. A running app must be quit first. Failed verification preserves an existing installation, and interrupted replacement restores its backup. The installer does not start the app. [scripts/test-installer.sh](scripts/test-installer.sh) exercises installation, checksum/version rejection, replacement rollback, and running-app protection against a local, quarantined archive fixture.
