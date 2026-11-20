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
- Generated corrections port AutoEq's correction and minimum-phase synthesis at [7ae0f56](https://github.com/jaakkopasanen/AutoEq/blob/7ae0f56d53074872b028649617a22bbb4232feb7/autoeq/frequency_response.py). Measurement and target are interpolated onto its 20 Hz-20 kHz geometric grid (1.01 frequency ratio). Quadratic Savitzky-Golay smoothing blends normal and treble curves with a sigmoid. Prominent peaks/dips determine protected cut regions and the reverse traversal start. Independent directional rising-slope limiters are combined by their minimum, followed by treble strength, boost limiting, and final 1/5-octave smoothing. AutoEq's direct return for curves without prominent extrema is preserved.
- Generation defaults are 8192 taps, 1 kHz alignment, -0.2 dB peak normalization, 1/12-octave smoothing, 2-octave treble smoothing with a 6-8 kHz transition, +6 dB maximum boost, and an 18 dB/octave rising-slope limit. Slope decay and concha-interference handling are configurable. A cut cap, bass taper (10-20 Hz), and treble taper (10-16 kHz) are optional overrides, disabled by default. Restore defaults preserves the treble-extension choice.
- FIR synthesis follows AutoEq's doubled-dB magnitude, zero Nyquist design point, Hamming-windowed `firwin2`, and half-length homomorphic minimum-phase conversion. The tap selector determines frequency resolution. Their complete validated recipe, source curves, and versioned synthesis policy are stored with the correction. Old generated audio remains stored; changing its rate requires explicit regeneration under the current policy. Published AutoEq impulses can use different recipes, tap counts, or SciPy versions, so coefficient identity with those assets is not assumed.
- Mono/stereo PCM and floating-point WAV import accepts 8-384 kHz sources, up to 16 MiB and 262144 frames per channel. Mono is duplicated to stereo. Validation rejects malformed or non-finite data. Managed originals preserve gain, channel order, leading silence, and tails; rate conversion preserves transfer gain and declares its added delay.
- Single-click prepares a comparison; double-click loads it. Repeated selection reuses prepared data. Newer requests supersede older preparation, and errors leave the playing correction intact. Playback changes are confirmed by the engine.
- Presets are explicitly saved, named references to managed corrections. Rename and deletion do not change playback. Their source identity survives rate conversion. Restoring a generated preset also restores its recipe and supported target modes; edits prepare a new comparison before replacement.

## Automatic room correction

- Room acquisition is an owned audio mode. It fades listening out, closes the listening streams while retaining the virtual-capture routing lease, and uses the selected stereo output with a separate typed mono microphone endpoint. The microphone is recorded and never monitored. Input and output rates are discovered independently. Opening the workspace alone preserves listening.
- Measurement audio reports a distinct Measuring lifecycle, followed by Stopped while output remains muted. The selected measurement route can be owned without first running the listening capture stream. Explicit Start/resume uses the current selected route; late events from an older measurement cannot reclaim mute state. Playback rejection receipts identify their correction, so stale state/rejection events do not erase a newer pending selection. Device monitoring during measurement watches actual microphone/speaker endpoints and system-output ownership; unrelated device inventory additions do not abort unchanged selected endpoints.
- Deliberate L/R level checks use three seconds of 300 Hz-3 kHz pink noise with quiet guards, initially -30 dBFS peak. A level approval identifies the route, microphone, calibration and fixed gain. Dedicated noise acquisition defaults to five seconds per position. The regular ESS defaults to eight seconds over 20 Hz-20 kHz with two seconds of decay; optional LF detail is a separate 16-second 20-300 Hz ESS with its own inverse.
- Original ADC f32 samples remain unchanged. Lossless bounded capture carries ordinals and driver timestamps. Host timestamp regression/gross gaps, clipping, ring/count/ordinal loss, stalls, stream errors, missing/ambiguous references and acoustic clock inconsistency invalidate a take. Host timestamps tolerate bounded jitter and do not prove single-sample hardware continuity. Two fixed-left coded references fit an affine ADC/DAC clock map. Arrival selection uses matched-filter strength with normalized confidence; repeatable reflections do not veto a take. A third code is predicted through the measured first-marker acoustic transfer and independently checks the clock map using normalized correlation and its position residual. Template projection amplitudes are retained as diagnostics and do not establish hardware gain or veto takes. Level consistency uses full-response repeats, measured uncertainty and observed device/control changes, without normalizing away real response differences. A derived 96-tap sinc resample precedes identification. The listening clock servo is never part of identification.
- Analysis uses a regularized exact discrete inverse of each emitted ESS reference, with zero-padded linear-tail accounting. Candidate probes are rendered by the existing convolver, removing its known 128-frame implementation delay while retaining intentional alignment. Verification divides by the original unfiltered ESS, retaining the candidate's acoustic effect. Exact emitted stereo frames and their hashes are retained.
- A Hann/Welch PSD in dBFS/Hz, temporal variation and crest statistics describe the dedicated ambient recording. Fixed warning thresholds default to 8 dB variation and 24 dB peak-to-average. Warnings retain evidence; per-band uncertainty is retained as measurement evidence. Noise is never subtracted from the waveform or magnitude response.
- Requested correction is 20 Hz-20 kHz. The reported measurement interior excludes sweep edges, uncovered microphone calibration and insufficient resolution. Correction fitting uses every bin within the configured correction range. Default endpoint guards are 5%; a nominal 20 kHz endpoint therefore does not establish a precise 20 kHz measurement. IR windows retain 20 ms before the reference and a tapered captured tail. Resolution is reported as 1/tail duration; LF dwell improves SNR independently. Late decay and pre-arrival energy are retained diagnostics, without whole-take energy-percentage or silent-edge vetoes. Known correction FIR support is added separately before the complete configured acoustic decay interval; effective duration and support are stored, and acoustic resolution uses only the room-decay interval.
- Room targets are independent of headphone rigs: Gentle monitoring, Natural, Warm, Flat in-room, editable tilt/bass, and imported relative acoustic curves. One robust common L/R target offset is fitted by default. The ordinary anchor is 300 Hz-3 kHz; the requested correction band supplies the fallback for limited targets. Targets cannot request correction outside an imported curve's declared coverage.
- Spatial fitting uses weighted incoherent power averaging, retains individual curves and spread, and never derives a fictitious complex average IR. Setup offers a single position or a guided five-position pattern around the listener. Saved source layouts and contributions remain readable. Reference verification covers only that position; spatial verification visits every included source position and preserves partial checkpoints.
- Parametric room targets combine tilt, bass shelf and a mid bell with a 20 Hz-20 kHz center frequency, default 1 kHz. Bell gain is -12 to +12 dB, default 0 dB; Gaussian full width at half gain is 0.1-4 octaves, default 1 octave. Saved targets without bell parameters remain neutral.
- One effort line interpolates target error in dB once: easy effort is a level with range-edge fades, advanced effort is a free knot line drawn and edited on the graph. Gaussian smoothing of measured dB magnitudes uses octave FWHM, 1/12 octave in bass and 1/3 in treble by default, blended across a movable crossover band. Target shape remains unchanged.
- Boost and cut limiters are independently optional and disabled by default. Enabled boost limits range from 0 to +24 dB; enabled cut limits range from 0 to 36 dB attenuation. Enabled gain limits apply uniformly within the correction range. Minimum-phase synthesis uses a neutral real-cepstrum primitive, without headphone peak normalization. Dense DC-to-Nyquist response, finite coefficients, tail energy and design error are checked. At most 5% common tonal-design projection and 0.15 dB common L/R gain projection can make the realized stereo span feasible; both are recorded. No channel is independently normalized. Generation chooses the shorter supported FIR first, then retries with the longer FIR if realization cannot meet the constraints; tap count is not a user setting.
- Optional level alignment attenuates the louder reference speaker by at most 6 dB; optional geometric alignment delays the earlier arrival by at most 5 ms. Both remain separate from tonal effort. Bypass retains these fixed trims/delays. Baseline history audition provides an original-system comparison. Default alignment is off.
- Measurement produces one prepared correction. Optional reference or spatial verification measures the selected exact filter once, without residual updates or target refitting. Frozen source, gain, alignment, confidence and processed goal remain the comparison reference. Complete trustworthy evidence can verify a filter; partial or failed checks retain the previous usable correction. Retry and verification deadline policy is internal.
- Stop, window close, sleep, device/rate/gain/routing changes and transport faults cancel owned starts and probes. Generation leases invalidate old queued starts and old results cannot replace a new run's UI. Driver callbacks observe cancellation directly. Sleep blocks new probe admission until wake, briefly permits stream teardown, and never restarts a probe. Resume listening is explicit. Core Audio endpoint properties and [workspace sleep notifications](https://developer.apple.com/documentation/AppKit/NSWorkspace/willSleepNotification) extend the existing platform listeners.
- Measurement sources, exact filters, objectives, run settings, outcomes and history use the SHA-256 object store and atomic manifests. Settings version 2 reads version 1. Existing headphone corrections migrate their formerly separate target/measurement fields into their source variant; imported and published WAV coefficients retain their contracts. Waveforms are binary objects, not JSON arrays. Saving a preset and naming a source are separate operations.
- Exports include stereo float32 correction WAV plus a self-contained generation companion, original mono microphone WAVs, clock-mapped/windowed raw-transfer IR WAVs, calibrated relative-response text with uncertainty, noise PSD, and a complete object/session export. Measurement IRs retain microphone response; the response text applies microphone magnitude calibration. Exports preserve transfer gain and channel order and exclude listening/headroom/comparison gains from correction WAVs.

The implementation is in `measurement`, `correction::room`, `audio::Owner`, and the native `ui::room` workspace. Hardware measurements and Intel long-filter timing remain separate acceptance checks; the validation record and isolated launch commands are in [the room design notes](docs/room-correction-design.md#validation).

## DSP and gain

`capture -> clock adaptation -> FIR / delay-aligned bypass -> comparison and headroom gains -> crossfade -> listening volume -> playback`

- Device callbacks exchange stereo frames through bounded rings, handle silence/ramping, and update meters. They do not allocate, block on locks, perform I/O, or destroy filter state. The owned DSP worker performs resampling and convolution; preparation builds replacement engines separately.
- Convolution uses 128-frame FFT partitions. Replacement filters warm up before a 30 ms crossfade. Bypass compensates algorithmic latency and imported leading delay; intentional delay remains part of an imported correction.
- Level matching uses fixed pink-noise weighting over 20 Hz-20 kHz, limited below Nyquist. It attenuates the estimated louder branch. Manual comparison trim adjusts corrected audio relative to bypass; comparison gains do not track the music.
- Shared headroom accounts for correction boost and comparison gains, with a 1 dB margin. It remains fixed when toggling bypass. This is a transfer-response estimate, not a strict transient bound.
- Listening volume is downstream, smoothed, and persisted. BlackHole handles macOS volume keys; the app does not apply their gain a second time.
- There is no hidden clipper, compressor, or music-dependent AGC. Output peaks and overload frames are reported. The IR graph excludes headroom, comparison gain, and listening volume.

## Interface and spectrum

- Starting a measurement does not require pink-noise level checks. Optional left/right recordings are retained only for the matching fixed signal path, including single-speaker evidence. Uncalibrated measurement requests one explicit start confirmation for the whole automatic sequence; the acknowledgement is recorded in its source plan and grants no persistent UI permission.
- Cmd+W and native window close cancel active/pending measurement through the same operation. Escape and every active measurement modal retain immediate Stop. Planned acquisition positions and measured contribution edits are separate; measured edits are bound to an immutable source hash and point index.


- The winit/Vello/Parley interface uses native measured macOS titlebar geometry and Cordex-style controls. Preferences has General, IR settings, Spectrum, and Presets tabs, with concise delayed hover help.
- The room workspace uses Setup, Measure and Correction. Setup presents microphone/level controls and guided listening-area selection in aligned columns. Correction has the graph beside a settings column for target, smoothing, limits, effort and range, with collapsible advanced and diagnostic sections. Route selectors stay in Setup/Measure. Native spacing, control sizes and scroll geometry remain shared with the headphone workspace.
- Microphone input channels are typed mono choices from the selected device's exposed channel count. Speaker endpoint edits obey the existing stopped-stream route contract. Physical gain is set on the hardware; probe peak has its own numeric control. Slider drags prepare one descendant on release or focus loss. Opening another source, starting measurement or leaving the workspace cancels pending preparation.
- Fitting opens the live graph. Measurement creates a prepared correction; the optional Measure result dialog checks its exact coefficients. Saved measurements reopen the latest usable prepared or verified correction. Existing history remains stored for reopening and exact exports, without a permanent history sidebar.

- Full mode shows the catalogue, route, correction controls, graph, and listening controls. Compact mode gives the content area to the graph. Details starts collapsed. Window sizes, layout mode, and separate light/dark or fixed theme choices persist. Appearance and layout changes do not restart audio.
- Loaded and candidate IRs have separate traces and a replacement overlay. Measured, target, and predicted curves appear when their metadata is available. Prediction is derived from source data, not a measurement of the listener's headphones.
- The spectrum taps input before correction/gain or output after processing. FFT analysis runs on the UI thread over bounded telemetry; overflow drops visualization samples without blocking audio. Gaps and rate changes reset incomplete windows; hidden windows pause capture and analysis.
- Spectrum levels are Hann-windowed stereo mean power with a separate dBFS scale. Opposite-polarity channels do not cancel. Shape-preserving cubic interpolation smooths the log-frequency outline without inventing peaks. Fill blends over the grid, additively on dark themes and with multiply on light themes; IR curves remain above it.
- FFT resolution, time averaging, level range, continuous intensity, and refresh target are configurable. Refresh cadence is independent of FFT size after warm-up. The Cordex/VU scheduler preserves deadline phase, skips missed slots, gates redraws, and backs off for hidden or unavailable surfaces. VSync removes the application timer cap; a target is not a guarantee of physical presentation cadence.

## Persistence and packaging

Versioned settings are replaced atomically; version 2 reads existing version 1 documents. SHA-256-addressed objects store original impulses, catalogue assets, and prepared corrections; reads verify integrity. Cached profiles work offline. Corrupt settings are reported without deleting managed IRs, and catalogue refresh never replaces the playing filter implicitly.

[scripts/package.sh](scripts/package.sh) builds the release executable, bundles the icon, resources, and third-party licenses, and signs the app locally. [assets/app-icon/icon.svg](assets/app-icon/icon.svg) is the editable icon source; [scripts/generate-icon.swift](scripts/generate-icon.swift) exports the PNG and macOS icon set used by packaging.

## Code and verification

| Area | Implementation |
| --- | --- |
| Audio ownership and DSP | [audio.rs](src/audio.rs), [dsp.rs](src/dsp.rs) |
| Correction validation and generation | [correction.rs](src/correction.rs), [generation.rs](src/correction/generation.rs), [autoeq.rs](src/correction/autoeq.rs) |
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
