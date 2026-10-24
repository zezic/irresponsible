# Room correction

IRresponsible measures a stereo speaker system with a separate mono microphone,
creates a minimum-phase correction, and lets the listener compare and save it.
The ordinary flow is Setup, Measure, Correction. Verification is optional.

## Setup and acquisition

The microphone endpoint is independent of the normal virtual capture route.
Select its mono input channel and the stereo playback device. Frequency
calibration uses the microphone's response file and orientation; uncalibrated
measurement requires acknowledgement. Responses use relative dB.

Optional left/right checks identify speakers and recording headroom. The app
controls probe level; hardware gain stays fixed. Direct monitoring, AGC and voice
processing must be disabled. The microphone is recorded and never monitored.

Use one listening position or a guided five-position pattern around it. The first
position is the reference. Subsequent positions sample nearby left, right, forward
and rear locations, with height variation. The guide is a placement aid, not a
geometric acoustic model. A manual Ready action or movement countdown advances
the run. Existing saved layouts remain usable.

Each position captures background noise and sequential left/right ESS sweeps.
Defaults are five seconds of noise, eight seconds per full-range sweep and two
seconds of acoustic decay. Optional extra bass acquisition uses its own longer
sweep and exact inverse. Acquisition settings are separate from correction range.
A repeated reference pair measures repeatability.

Original ADC samples remain immutable. Clock markers identify the independent
input/output time mapping; a diagnostic reference checks consistency. Clipping,
missing references, transport gaps, device failures and unusable captures stop or
retry the take. Noise variation and crest statistics are advisory. Band-specific
uncertainty constrains correction. Noise is not subtracted as a waveform or dB
curve. Detailed acquisition and calibration research is in
[room-correction-research.md](room-correction-research.md).

Stop, window close, sleep and relevant device/gain/routing changes cancel probe
ownership. Output remains muted until the user resumes listening. Completed
channels and spatial checks have checkpoints; resuming is explicit.

## Correction

Spatial aggregation averages power, retaining each measured curve and its spread.
There is no complex average impulse response. The correction uses a common target
shape and target offset, with independently designed stereo FIRs.

Targets include Gentle, Natural, Warm, Flat, custom bass/tilt/mid bell and imported
relative acoustic curves. The parametric mid bell is a Gaussian in log frequency,
with a configurable 20 Hz-20 kHz center frequency, defaulting to 1 kHz. Gain ranges from -12 to +12 dB and defaults to 0 dB (neutral).
Its full width at half gain ranges from 0.1 to 4 octaves and defaults to 1 octave.
Bell gain adds to the tilt and bass shelf before correction fitting. A common L/R target level is fitted automatically by default.
Imported targets must cover the requested correction range. All measured bins
participate in correction fitting within that range.

For baseline `B`, target `T`, and the resolved effort profile `e`:

```text
requested goal = B + e * (T - B)
smoothed baseline = Gaussian(B, octave FWHM profile)
design = constrain(e * (T - smoothed baseline))
processed goal = B + design
```

The normalized Gaussian operates on all bins of the equal-log-frequency
measurement grid. Zero width is identity. Default widths increase from 1/12 octave in the
bass to 1/3 octave in the treble. The smoothed baseline is reused across candidate
target levels. The target curve is not averaged away. Smoothing measured response
before fitting follows [REW's documented workflow](https://www.roomeqwizard.com/help/help_en-GB/html/eqwindow.html).

Boost and cut limits are independently optional and disabled by default. Enabled
limits allow 0 to +24 dB boost and 0 to 36 dB attenuation. Numeric limits in saved
recipes retain their enabled state. Disabled limits bypass both design caps and
realized-gain projection for that direction; playback headroom still follows the
actual filter gain.

Enabled gain limits apply a uniform dB ceiling or floor across the correction
range. Noise and spatial spread are reported as measurement evidence.

Global effort is applied once, after response smoothing. Optional regional effort
and smoothing overrides resolve into one profile. Independent boundaries use
automatic feathering. Removing an override restores the underlying profile.
Regional controls are collapsed by default. Raw measurements are never rewritten.

Optional level alignment attenuates the louder reference speaker; time alignment
delays the earlier arrival. These are independent of tonal effort and remain in
the aligned bypass comparison.

Generation tries the shorter supported FIR and then the longer FIR when needed.
Real-cepstrum synthesis creates minimum-phase coefficients. Dense realized-response
checks enforce enabled stereo gain limits, finite coefficients, tail settling and design
accuracy. A limited common gain/design projection is recorded. Invalid filters
are not published. FIR support and convolution buffering are distinct quantities.

## Listening and verification

Measurement creates a correction once. Listen loads that candidate; Bypass compares
with the original. Target or regional edits prepare a replacement from the saved
source and never silently change the playing filter.

Measure result checks the exact candidate at the reference position or all source
positions. It does not change coefficients, smooth the accumulated filter, or
refit the target. The microphone, output, calibration and gain must match the
source. Noise and repeatability inform the result; only complete evidence can
verify the requested scope. Spatial checks retain partial evidence for resuming.
A failed check does not erase an earlier usable filter.

The graph occupies the full correction workspace. Setup and Measure contain
routing controls. Correction contains target controls, the graph, overall effort,
and collapsible regional adjustments and measurement details. Diagnostic scores
are not presented as a universal sound-quality rating.

## Persistence and exports

The content-addressed store retains original sources, exact coefficients, frozen
objectives and measurement evidence. The UI exposes named measurements and saved
presets rather than the optimizer's pass history. Internal ancestry preserves
reopening, exact-pass exports and existing saved sessions.

Measurements can be reopened and retuned without a microphone. New coefficients
have their own identity and do not inherit verification from another filter.
A prepared correction remains usable when a later check is incomplete or rejected.

Exports include stereo float32 correction WAV and its generation companion,
original microphone WAVs, clock-mapped/windowed transfer IRs, calibrated relative
response text, noise evidence and complete sessions. Listening comparison gains
are excluded from correction WAVs.

## Validation

Unit and simulated integration tests cover acquisition transport and clock
contracts, calibration, spatial averaging, optional gain limits, FIR
realization, explicit verification, interruption recovery, saved identities and
native UI hit geometry. Native preview rendering checks layouts at supported
window sizes. Simulations use temporary stores and emulated endpoints.

Physical microphone/speaker measurements and Intel long-filter timing require
hardware validation. Numerical agreement with a processed goal does not certify
listening preference or whole-area behavior from a reference-only check.
