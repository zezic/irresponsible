# Automatic room correction: research evidence and DSP recommendations

Research date: 2026-10-23. This note supports the [feature specification](room-correction-design.md); it does not implement the feature. Statements labelled **Proposal** or **Inference** are engineering recommendations for IRresponsible, not published standards or descriptions of proprietary algorithms. The specification resolves the product defaults and integration decisions; this note supplies their evidence.

**Requested product contract:** Correction covers 20 Hz-20 kHz, subject to actual measurement validity and speaker capability. The user controls correction effort and regional post-processing smoothing. Dedicated noise acquisition, slower LF measurement, emulated-device testing, and a user-defined multipoint count/layout are required parts of the design. The historical LF-focused literature below informs protection and smoothing within that full-range contract; it does not restrict the requested operating range.

## 1. What a target curve can and cannot specify

### Direct sound and a room measurement are different targets

Toole's 2015 paper explains why a good loudspeaker with flat direct sound can produce a downward-sloping steady-state response in a reflective room. Loudspeaker directivity and the amount of reflected sound change that curve. Equalizing an arbitrary listening-position measurement to a universal line can damage the direct response. The transition from modal to higher-frequency behavior matters; above it, a microphone's room curve does not uniquely describe the loudspeaker. The paper also reports differences between trained and untrained listeners' preferred bass/treble settings and argues against one target satisfying everyone. Its discussion of cinema calibration does not establish a cinema target for small stereo rooms. [Toole, The Measurement and Calibration of Sound Reproducing Systems, JAES 2015, particularly sections 2, 4.1, 7](https://www.linkwitzlab.com/Toole-Room%20calibration.pdf).

Olive's own report of controlled room-correction comparisons found preferred treatments had smooth downward in-room responses and good bass extension. Some tested corrections did no better than, or were worse than, the uncorrected system. The experiment involved eight trained listeners, one loudspeaker/subwoofer setup, and a particular room: it supports careful target selection, not a universal numerical curve. [Olive, 2009 research report and reference to AES paper 7960](https://seanolive.blogspot.com/2009/11/subjective-and-objective-evaluation-of.html).

### Studio/reference requirements

EBU Tech 3276 defines an operational room response using third-octave pink noise, gives a **tolerance mask**, and stresses stereo matching. Section 2.4 advises careful equalization limited to frequencies below 300 Hz, preferring placement and acoustic treatment first. Its Figure 2 permits high-frequency rolloff; it is not an instruction to force every frequency to an exact flat line. EBU's publication page also warns that its original listening-level instructions predate loudness normalization and refers to Tech 3343 for updated level instructions. [EBU Tech 3276, section 2.4](https://tech.ebu.ch/docs/tech/tech3276.pdf), [official publication notice](https://tech.ebu.ch/publications/tech3276).

ITU-R BS.1116-3 distinguishes free-field loudspeaker performance from operational room response. Section 8.3.4 defines the latter over 50 Hz-16 kHz using third-octave pink noise and gives a tolerance mask; it prefers inter-speaker room-response differences no greater than 2 dB. Section 8.3.3 separately specifies early-reflection and room behavior. Applying a filter cannot certify a room to all these conditions. [ITU-R BS.1116-3, sections 7.2 and 8.3](https://www.itu.int/dms_pubrec/itu-r/rec/bs/R-REC-BS.1116-3-201502-I%21%21PDF-E.pdf), [ITU status page](https://www.itu.int/rec/R-REC-BS.1116-3-201502-I).

**Proposal:** Within full-range operation, a studio profile preserves speaker voicing through its target, correction effort, and stronger smoothing of unsupported local detail at high frequencies. It can still reduce reproducible bass resonances and match channels. Do not label a preset "ITU compliant" or "EBU compliant" on the strength of its target curve.

### Historical B&K response

The original manufacturer-hosted paper is available: Henning Moller, B&K application note 17-197, presented in 1974. Figure 5 and the accompanying text on printed page 4 suggest modest bass lift and treble rolloff for commercial recordings. The author explicitly describes the shape as tendencies, partly informed by listening and partly by concert-hall considerations. The study used five speakers, three rooms, and five critical listeners. There is no machine-readable canonical anchor table in the paper. [B&K, Relevant loudspeaker tests, 1974, printed pages 1-4 and Figure 5](https://www.bksv.com/doc/17-197.pdf).

**Proposal:** A future "B&K-inspired" preset may identify an explicitly approximate digitization of Figure 5. For the first release, app-owned tilt/shelf presets are clearer and avoid presenting arbitrary online implementations as the historical original.

### Current manufacturers reinforce configurability

Genelec's Sound Character Profiler offers shared spectral adjustments and custom shelf controls after calibration. Neumann advertises a room-adaptive target. These establish product precedents for separate calibration and tonal preference, but reveal no general algorithm that IRresponsible should claim to reproduce. [Genelec GLM operating manual, section 8.3](https://assets.ctfassets.net/4zjnzn055a4v/7jLw9cbFMRvbkbxmQKqNVN/f340eda426e2b5c2c3354fd5424fcd08/GLM4_System_Operating_Manual_2021-07-14_-_FINAL.pdf), [Neumann software overview](https://www.neumann.com/en-au/products/software).

**Proposal:** Provide Gentle monitoring, Natural listening, Warm listening, Literal flat, and Custom. Store their parameters explicitly: tilt in dB/octave, pivot, shelf frequency/gain, supported bandwidth, normalization reference, and target version. Describe each as an IRresponsible starting point. Never use a headphone Harman curve as a room-loudspeaker target. Cinema X-curves and automotive targets are also outside this stereo-room feature's scope.

## 2. Measuring the transfer response

### Logarithmic sweep and deconvolution

Farina's exponential-sweep method separates the linear impulse response from harmonic-distortion responses, which appear earlier in the deconvolved result. The inverse sweep needs frequency-dependent amplitude compensation; reversing samples alone is insufficient. Linear convolution and adequate silence avoid circular time aliasing. [Farina, AES 5093, 2000, sections 2-3](https://angelofarina.it/Public/Papers/134-AES00.PDF).

Novak, Lotton, and Simon derive a synchronized exponential-sweep construction and analytical deconvolution, with implementation examples. Their synchronization concerns correct higher-harmonic analysis; it does not substitute for correcting independent ADC/DAC clock rates. [Novak et al., JAES 2015](https://aes.org/publications/elibrary/elibrary-page/?id=18042), [author-hosted paper](https://imdea.ant-novak.com/_static/TM/biblio/Novak%20-%202015%20-%20JAES%20-%20Synchronized%20Swept-Sine%20-%20Theory%2C%20Application%2C%20and%20Implementation.pdf).

REW documents that increasing sweep duration improves signal-to-noise ratio, but independent recording/playback clocks can smear the impulse and phase. Very long sweeps also increase sustained speaker loading. Its typical 256k sweep is approximately 5.46 seconds at 48 kHz. [REW, Making Measurements](https://www.roomeqwizard.com/help/help/html/makingmeasurements.html).

**Proposal:** Use the selected supported playback rate (44.1/48 kHz), an 8 second full-range sweep per speaker, one repetition, and at least 2 seconds of post-sweep capture. Use a low frequency selected for the actual speaker, initially 20 Hz for full-range systems; do not excite lower frequencies just because the app can generate them. Taper the beginning/end smoothly and keep inverse calibration consistent with those tapers. Preserve the raw capture and exact generated stimulus or its versioned reproducible definition. Analyze in f64 off the audio callback; do not perform FFTs, allocation, or disk writes in the real-time loop.

These durations are initial tuning values. Long-decay rooms may require a longer capture tail. Measurement bandwidth, correction bandwidth, and resulting FIR length are separate settings.

### Slower LF measurement without breaking ESS analysis

The standard ESS harmonic separation depends on the exponential time/frequency law: an nth harmonic corresponds to a constant advance `L * ln(n)`. A different sweep law does not inherit that property. Correct higher-harmonic phase also requires the synchronized construction described by Novak et al. [Novak et al., sections 1.1-1.3](https://imdea.ant-novak.com/_static/TM/biblio/Novak%20-%202015%20-%20JAES%20-%20Synchronized%20Swept-Sine%20-%20Theory%2C%20Application%2C%20and%20Implementation.pdf).

**Proposal:** Keep a normal full-range ascending ESS and add a separately acquired slower LF ESS over an overlapping band, initially 16 seconds over 20-300 Hz with validated guard coverage around its trusted interval. Give each sweep its own duration, exact samples, inverse, noise estimate, clock fit, and valid band. This puts much longer dwell per LF octave than the 8-second full-range sweep. Each additional LF pass needs its own ringdown capture. These are tuning proposals, not optimal values established by the papers.

**Inference:** Longer LF dwell adds excitation energy and can improve identification of low-amplitude narrow resonances at fixed acoustic level. It does not alone produce arbitrary frequency resolution: the recovered IR's usable duration, noise, timing stability, and synthesis support still limit resolution. A 0.5 Hz-wide feature requires substantially more usable temporal support than a 5 Hz-wide one. Do not call FFT zero padding additional physical resolution.

**Proposal:** Combine LF and full-range magnitude estimates using uncertainty-weighted overlap and smooth band transitions, retaining both originals. A disagreement beyond repeatability triggers diagnosis instead of splicing inconsistent levels. Preserve the full-range timed IR as the default source of wideband arrival/phase diagnostics; partial-band minimum-phase extrapolation and combining unrelated IR phases are unnecessary for the initial magnitude-correction design.

An arbitrary slowed-bass/time-warped sweep is possible for linear identification with a matching numerically derived, regularized inverse. It requires its own proof of amplitude normalization, noise behavior, and nonlinear contamination; do not reuse the standard exponential inverse or constant harmonic-arrival separation. Separate LF ESS is the simpler initial contract.

### Timing and independent clocks

REW explicitly measures clock-rate mismatch using an additional acoustic reference at the sweep end and the time between start/end references. It can also use an electrical loopback. [REW, Analysis Preferences, clock adjustment](https://www.roomeqwizard.com/help/help/html/analysis.html).

**Proposal:** Capture before playback. Emit a known timing marker from a fixed reference speaker, allow its tail to settle, emit the active L or R sweep, capture its complete decay, then emit another marker from the same reference speaker. At a fixed mic position use the same reference output for both channel runs. Estimate sub-sample marker arrivals by correlation, reject ambiguous peaks, and resample capture to playback-clock time before deconvolution.

If marker spacing is N output frames and n input frames, an input position associated with output frame k is `input_start + k * n / N`. Account for nominal input/output rates before interpreting the residual as drift. Persist the fitted mapping, ppm, marker confidence, and residual error. Overall latency and clock-rate mismatch are different quantities. A one-time IR peak shift corrects neither rate mismatch nor arbitrary callback scheduling changes.

**Proposal:** Prefer a shared-clock interface or loopback when available; support an independent USB mic through the marker path. Flag stream discontinuities and missing markers as invalid measurements. Use a third diagnostic marker or repeatability check to detect changes that a two-point affine clock mapping cannot represent. Do not claim absolute acoustic distance from an acoustic reference; it establishes relative timing to the reference path.

### Microphone calibration, orientation, and gain

miniDSP supplies individual response calibration and separate 0-degree/90-degree files. Its documentation pairs these with specific microphone orientations. The selected calibration and physical orientation must agree. [miniDSP microphone calibration/orientation guidance](https://docs.minidsp.com/product-manuals/shd/autoeq/first-steps.html), [UMIK-1 manufacturer description](https://www.minidsp.com/products/acoustic-measurement/umik-1?lang=en).

REW typically sets measurement level near 75 dB SPL, separately adjusts analog input gain, and warns against increasing loudspeaker output just to obtain a larger recorded level. Its analog input guidance is approximately -30 to -12 dBFS RMS, while USB microphones can legitimately read below -50 dBFS. [REW, Check Levels](https://www.roomeqwizard.com/help/help/html/measurementlevel.html).

A frequency-response calibration is not automatically absolute SPL calibration. REW accepts sensitivity data for suitable USB mics; analog chains require the same interface gain/input volume as used during SPL calibration. Changing those settings invalidates the calibration relationship. [REW, Calibrating the SPL Reading](https://www.roomeqwizard.com/betahelp/help/html/inputcal.html).

**Proposal:** Setup identifies device/channel, mic model/serial, calibration file, orientation, and whether sensitivity is known. For a generic analog mic, support calibrator/external-meter SPL alignment separately from response calibration. If sensitivity is absent, show relative dB and input dBFS, never invented SPL. Reject malformed, nonfinite, unordered, or implausible calibration data at import. Save calibration contents/hash with the measurement, not just an external path.

**Proposal:** Run short, low-level checks on L then R; the user adjusts acoustic volume and analog preamp independently. Aim around 70-75 dB SPL when calibrated, with at least 12 dB input peak headroom initially. Permit lower levels when the useful-band SNR is adequate. Keep stimulus and hardware gain fixed across baseline/verification. Disable AGC, noise suppression, echo cancellation, input monitoring, and hardware direct-monitor routes. The mic must never be mixed into speaker playback.

### Dedicated background-noise acquisition and uncertainty

REW records a no-signal noise floor before the measurement and distinguishes that spectrum from noise/non-harmonic distortion measured with excitation. Its documentation illustrates misleading normalized distortion peaks when the fundamental is weak relative to noise. [REW, Distortion Graph](https://www.roomeqwizard.com/help/help/html/graph_distortion.html).

Canfield-Dafilou and Abel explicitly analyze additive noise convolved with an inverse sweep and the artifacts of truncated measurements. This supports treating deconvolved noise as a processed quantity rather than copying a raw ambient spectrum onto an IR plot. [On Restoring Prematurely Truncated Sine Sweep Room Impulse Response Measurements, DAFx 2017](https://www.dafx17.eca.ed.ac.uk/papers/DAFx17_paper_23.pdf).

Power subtraction can estimate a signal spectrum under stationary, uncorrelated-noise assumptions, but estimation error can yield nonphysical negative power. It does not recover the phase of a missing response. [Aalto University, spectral subtraction derivation and limitations](https://speechprocessingbook.aalto.fi/enhancement/noise-attenuation/).

**Proposal:** After level setup, acquire a dedicated silent stage, initially 5-10 seconds, with the exact mic gain/rate/calibration used for sweeps and output held silent. Store raw samples, calibrated noise PSD by band, variance across time windows, tonal components, and transient indicators. Repeat this stage after gain/routing changes, at a moved mic where background differs, and when between-attempt checks detect a changed floor. Keep quick silent guards in individual takes for change detection; they do not replace the initial noise record.

**Proposal:** Use the measured floor to select duration, identify unusable bands, set confidence/regularization, choose the useful IR tail, and determine whether an apparent iteration improvement exceeds uncertainty. A linear inverse maps additive noise through its own transfer function; propagate the recorded-noise statistics through the same clock correction, inverse, window and spectral normalization, analytically where valid or by reproducible noise realizations. Comparing raw PSD directly with unnormalized FFT energy is dimensionally incorrect.

**Proposal:** Do not subtract the silent waveform, subtract dB values, or denoise the only saved raw take. If bias-correcting averaged power, document the estimator and show uncertainty; nonpositive/uncertain estimates mean insufficient evidence, not zero room response. Correlated interference and noise that changes when the speaker is active require repeatability checks. No background-only measurement proves signal-present behavior is clean.

## 3. Response analysis and correction limits

### Windows and frequency resolution

REW documents the time/frequency tradeoff and frequency-dependent windows. A 15-cycle window is 150 ms at 100 Hz and 15 ms at 1 kHz. Removing late sound through a window is different from smoothing a full-room spectrum. [REW, Impulse Responses](https://www.roomeqwizard.com/help/help/html/impulseresponse.html).

**Proposal:** Keep at least two views derived from one saved IR: a long-window room response for bass fitting/decay and a direct/early or frequency-dependent window for diagnosis. Start with the captured tail, typically 2 seconds, adapting the analysis window to usable SNR and decay. Show the window's lower validity limit; a 10 ms gate cannot establish 30 Hz behavior. Preserve the raw IR so later window changes need no remeasurement.

**Proposal:** Design correction over the requested 20 Hz-20 kHz domain. Use regional effort, smoothing, uncertainty, and total-gain limits to prevent fitting fragile details, especially at high frequencies. Unsupported speaker extension, calibration, sweep-edge bins and low-SNR regions retain explicit limitations; full-range does not mean unlimited inversion. A room transition estimate is diagnostic, not an automatic assertion that all response below a number is invertible. Do not infer an accurate Schroeder frequency from an invalid small-room RT60 fit.

### Nulls, minimum phase, and spatial robustness

Room responses are mixed phase, even at low frequencies. REW demonstrates that deep cancellation nulls cannot be repaired with arbitrary boost and describes excess-group-delay analysis to locate unsuitable regions. At higher frequencies narrower equalization becomes increasingly position-sensitive. [REW, Minimum Phase](https://www.roomeqwizard.com/help/help/html/minimumphase.html).

Frequency-dependent regularization controls inversion outside the transducer's usable band and suppresses excessive inverse-filter energy. Farina's first-party implementation also discusses smoothing and the relation to shorter filters and dynamic range. This is a useful general technique, not justification for exact full-room inversion. [Farina, Kirkeby inverse filter documentation](https://angelofarina.it/Aurora_XP/Kirkeby%27s%20Inverse%20Filter%20v4_0.htm).

**Proposal:** Fit a smoothed desired gain, apply a confidence mask and bounded gain, then synthesize a minimum-phase correction FIR. The current feature proposal allows +3 dB boost and 12 dB cut by default, with configurable limits and a cut-only choice. Automatic target-level fitting is enabled: fit one common L/R offset against the feasible correction rather than treat the target's zero reference as fixed. Freeze that offset during verification/refinement. These are product design choices, not a literature-derived optimum. Never allow successive attempts to escape the configured total limits or boost below the usable LF cutoff. Deep narrow dips remain visible as limitations; suggest placement/acoustic changes instead of chasing them.

**Proposal:** Phase correction in the first release should mean correction filter's minimum-phase behavior plus explicitly validated inter-channel delay alignment. Do not invert the entire measured complex response or promise reflection cancellation. Mixed-phase correction can be a later mode with a separate latency, pre-ringing, causality, and spatial-validation specification. A long minimum-phase FIR can represent bass response without its delay equalling half its length; centered linear-phase synthesis has a different latency contract.

### Multiple positions

Dirac's first-party guidance uses the central position for gains/delays and spatially separated additional positions for the listening region. Its recommended spacing is product-specific and should not be copied as a physical law. [Dirac, measurement order and spacing](https://helpdesk.dirac.com/en/dirac-room-correction/In-what-order-should-I-measure-the-positions-cbe).

REW distinguishes incoherent RMS/power averaging, mean dB, and vector averaging. Vector averaging can introduce cancellation between positions; dB averaging weights peaks/dips differently from power averaging. It suggests spatial-level alignment where source-distance changes dominate. [REW, All SPL Graph](https://www.roomeqwizard.com/help/help/html/graph_allspl.html).

Makivirta and Lund measured 18 positions in 12 listening rooms. They distinguish estimating one listener's local response from sampling an entire listening area. In their professional-room sample, very small displacements often added little to frequency smoothing, while pronounced modes made local spatial sampling more useful. Wider averaging could compromise the main position. Their result does not establish that all rooms need a fixed point count or spacing. [Stability of the Frequency Response Estimate in Listening Rooms, 2016, sections 1.3 and 4-5](https://assets.ctfassets.net/4zjnzn055a4v/7lPJ4uF9Wjgt5Pme73JdYF/e7ceca69d16646d4454d2ec1744adbab/Stability_of_the_Frequency_Response_Estimate_in_Listening_Rooms_-_2016.pdf).

Genelec GLM allows an arbitrary measurement count, anchors timing/level to the primary position, and generally suggests additional locations within 0.5-1 m. These are its operating recommendations rather than a universal sampling theorem. [Genelec GLM manual, section 7.15](https://assets.ctfassets.net/4zjnzn055a4v/7jLw9cbFMRvbkbxmQKqNVN/f340eda426e2b5c2c3354fd5424fcd08/GLM4_System_Operating_Manual_2021-07-14_-_FINAL.pdf).

**Proposal:** Let the user choose count and positions, with single-position and seven-position templates as conveniences. Every point has a label, optional relative coordinates, and an explicit preference/coverage weight. Distinguish **local listening volume** (head motion around one seat) from **multiple seats** (a compromise across listeners). An arbitrary diagram alone must not imply a physical interpolation model or that unsampled positions were measured.

**Proposal:** Store every L/R pair separately. Use weighted incoherent power average `sqrt(sum(w_i * |H_i|^2) / sum(w_i))` as a declared aggregate, then constrain candidate behavior using individual positions, spread, and worst/upper-percentile error. The aggregate is not the measured response at a fictional average seat, nor a guarantee each seat matches the target. Power averaging emphasizes energetic peaks; dB averaging and robust positionwise objectives have different meanings and must not be silently exchanged.

**Proposal:** Keep center-position levels for L/R gain matching. For multi-seat operation, preserve absolute seat levels by default: acoustic quietness at a distant seat is part of actual coverage. An optional shape-only distance normalization should state that it removes level differences, apply one scalar jointly to that position's L/R pair, and retain original values. Dense sampling around a preferred seat must not silently overweight it: weight seats/groups explicitly before distributing weight among their points. A user-requested weighted preference may intentionally favor that seat.

**Proposal:** More positions improve coverage of the chosen objective, not the number of independent correctable speaker degrees of freedom. One diagonal stereo FIR cannot generally equalize unrelated seat nulls. Moving the mic remains manual; autonomous retries run at its current position, and a spatially verified result requires guided return to every required verification position. Keep individual and aggregate verification statuses separate.

Single-position operation is valid but cannot establish spatial robustness. Predicted curves at training positions also cannot prove performance at an unmeasured location.

## 4. Automated measure / correct / verify / refine

### Correction effort and regional post-processing smoothing

**Proposal:** Expose correction effort as the fraction of measured target error the design attempts to remove, with a global value and optional smooth frequency-region overrides. Effort is distinct from allowed gain, confidence, graph smoothing, and the damping used between iterations. A user selecting 50% effort must not obtain 100% correction after enough retries.

For one channel or the declared spatial aggregate, let `M0(f)` be the immutable baseline in dB, `T(f)` the chosen full target, and `e(f)` the fixed effort in `[0,1]`. Define the effort-dependent requested response:

```text
T_eff(f) = M0(f) + e(f) * (T(f) - M0(f))
```

For a spatial objective this defines one common effort-dependent aggregate goal; individual positions still enter stability/coverage constraints. This is an app-owned definition, not a perceptual equivalence law.

**Proposal:** Regional post-processing smoothing operates on the total correction gain in log frequency before minimum-phase synthesis. Define its domain (dB magnitude), regional kernel widths, crossover blends, edge treatment, and order relative to effort and gain projection. Suggested order: estimated correction request -> fixed effort -> regional post-smoothing -> cumulative gain/unsupported-band projection -> FIR synthesis -> exact realized-response checks. Prefer a joint smooth bounded fit when final hard projection would create sharp corners. Display-only smoothing never changes the filter.

**Proposal:** Freeze the processed acoustic goal `Q = M0 + constrain(S(e * (T - M0)))` for the run, where `S` is the declared regional post-smoothing map. The feedback controller evaluates agreement with `Q`, while separately reporting residual to `T_eff` and `T`. Keep one unsmoothed design representation and regenerate each total candidate through the declared rendering map. Do not smooth an already smoothed correction on every iteration, and do not repeatedly multiply residual by effort while retaining the old filter: the latter would asymptotically erase full target error even when the user requested partial correction. If smoothing or gain limits prevent attaining `T_eff`, report that remaining error as an intentional/design constraint, not a reason to remove those settings automatically. Agreement with `Q` is not automatically a full target match.

**Proposal:** Freeze effort, smoothing, target/reference level, spatial weights and measurement policy for a comparable run. A user edit creates a new design objective/candidate series and invalidates the old convergence score. Zero effort means no target-shape correction there; explicitly reapply zero-effort and correction-range constraints after cross-region smoothing so adjacent bands cannot leak corrections into that region. Optional channel alignment and overall level trim retain their separately declared meaning.

REW provides a "Measure with these filters" operation: it plays a filtered sweep and shows any filter-set headroom requirement. That is direct precedent for physical verification of a calculated correction. It separately constrains individual and combined boost, avoids boosting past natural rolloffs, and constrains narrow boost filters. These controls do not establish a universally convergent repeated-measurement algorithm. [REW, EQ Window, Filter Tasks](https://www.roomeqwizard.com/help/help_en-GB/html/eqwindow.html).

**Inference:** For a fixed linear time-invariant plant `P`, stimulus `X`, candidate `C`, and known measurement attenuation `a`, the verified response is `Y / X = a * P * C`. An accurate baseline already predicts this. Iteration is useful for finding imperfect measurement, implementation, filter truncation, drift, or level-dependent behavior; it is not a reason to repeatedly invert noise. Exact cancellation notches remain notches on every iteration.

**Proposal:** Use immutable baseline captures and versioned **total candidate filters**. Record the actual applied filter hash, common attenuation, channel trims/delays, stimulus amplitude, sample rate, and routing for each attempt. Verification must pass through exactly the candidate that playback would use. A sweep sent to a generator path which bypasses correction cannot verify it.

**Proposal:** Each attempt follows this bounded contract:

1. Acquire baseline L/R and, where requested, spatial positions. Repeat a short validation capture to estimate measurement repeatability before trusting small improvements.
2. Generate a total candidate from baseline and the fixed selected target, correction effort, and regional smoothing. Compute its realizable response, peak-frequency gain, impulse behavior, and required common headroom.
3. Apply candidate plus its documented common attenuation; wait for filter swap and prior tails to settle. Capture sequential L/R again using the same mic position/gain/reference.
4. Reject invalid capture or changed environment before fitting residual error. Estimate useful-band SNR, clipping/dropouts, timing confidence, predicted-vs-measured discrepancy, and response variation between runs.
5. Compare shape error against the frozen processed acoustic goal `Q` using one frozen reference-level convention; separately report distance to the effort-adjusted request, full target, and absolute level loss. Weight by log frequency and confidence, with per-region and channel-balance checks. Improvement must exceed repeatability uncertainty.
6. If useful, make a damped, bandwidth-limited residual update and rebuild one total FIR. Bound both each update and the total correction. Refit from baseline rather than multiplying an unlimited chain of inverses. Where estimating `P` from corrected runs, divide by the **known effective** candidate only with regularization; tiny candidate gain can make the estimate noisy.
7. Stop after a small configured attempt budget (initial proposal: three corrected measurement attempts), a repeatability-based plateau, an unacceptable regression, or a completed target criterion. Retain the best physically verified candidate, not automatically the last.
8. Perform a final full-range L/R measurement through the chosen candidate, even if refinement used limited bands. If no candidate improves on baseline within confidence, keep correction disabled and explain the limiting evidence.

**Proposal:** Narrowband runs are an optional speed optimization. Measure the suspect interval plus guard frequencies, retain enough time for LF ringing, and keep fixed timing markers. They may validate a local magnitude change, but cannot validate global headroom, full-band phase/minimum phase, or the rest of the correction. Do not shorten LF capture tails merely because excitation bandwidth is smaller. Disjoint tiny sweeps can cost more marker/settling overhead than a full sweep; choose based on expected elapsed time.

**Proposal:** A multi-position session must not silently claim closed-loop verification of all positions after the mic returns to center. Either guide the user through each required verification position, or report center-only physical verification plus predicted results at other saved positions. Retain per-attempt data so target edits and later algorithm changes reuse existing evidence.

## 5. DSP validation before acoustic testing

### Emulated devices and acoustic plant

Pyroomacoustics supplies source-to-mic room IR simulation and signal convolution using image-source methods, with optional later ray-traced reflections, material/air absorption, and noise. It documents an added global fractional-delay-filter delay, optional signal normalization, and limitations of its hybrid simulation. These details matter when treating simulated output as timing/gain ground truth. [Pyroomacoustics, Room Simulation](https://pyroomacoustics.readthedocs.io/en/pypi-release/pyroomacoustics.room.html?highlight=compute_rir).

MathWorks demonstrates swept-sine identification against a simulated nonlinear loudspeaker, comparing low-level linear behavior with higher-level distortion and separating linear/harmonic responses. This is precedent for testing measurement DSP with known nonlinear plants before relying on a physical room. [MathWorks, Measuring Loudspeaker Nonlinearities Using Swept Sine Excitation](https://www.mathworks.com/help/audio/ug/measuring-loudspeaker-nonlinearities-using-sweeptone.html).

**Proposal:** Implement an emulated input/output backend at the same stream boundary used by measurement, so the full ownership/routing, probe, capture, clock mapping, analysis, refinement, and persistence path runs autonomously. It must consume the **actual generated output samples** and return simulated mic samples; directly supplying an already correct IR to the analyzer would skip essential contracts. No operating-system virtual-device driver is required for the first deterministic test backend. A separate host-device smoke test still checks real CPAL/OS integration.

**Proposal:** The emulated plant comprises independent stereo speaker responses, direct/fractional propagation delays, position-dependent reflection/room responses, modal resonances and nulls, reverberant tails, mic response and analog gain, optional noise/nonlinearity, and ADC/DAC buffering and clocks. Change virtual mic position only on explicit test actions. Support mismatched nominal rates, positive/negative ppm drift, callback block-size differences, jitter, dropped/duplicated blocks, clipping, disconnects, and changing noise. A seeded room fixture or externally generated RIR ensemble can supply position-dependent acoustics; simple analytic resonators provide exact LF ground truth independently of geometrical simulation assumptions.

**Proposal:** Disable any simulator automatic per-source/per-run normalization. Store simulation seed, actual source/mic transfer gain, implementation delays and known input/output timeline mapping. Keep simulated-room truth outside the correction algorithm: tests may inspect it, but the app must infer everything through its normal mic capture. Include both accelerated deterministic runs and real-time scheduler runs. Simulation proves behavior against declared models; it does not replace physical speaker/mic validation or guarantee a geometrical model reproduces a specific real room.

These are proposed acceptance tests, not claims from the target-curve literature:

- Synthetic known FIR/room responses recover correct magnitude, timing, gain, and phase within specified numerical tolerances; verify frequency-dependent inverse-sweep normalization with a digital identity path.
- Add noise, harmonic nonlinearity, truncation, fractional delay, and clock mismatch; the pipeline must retain the linear IR, report uncertainty, and avoid false improvement. Include stream loss and ambiguous timing-reference cases.
- Independent-clock simulations across both signs of drift prove that resampling fixes IR smearing, and an inserted discontinuity is rejected rather than disguised as a large clock rate.
- Nulls, bandwidth-limited speakers, and spatially different IRs prove boost and correction-domain limits survive **every** refinement attempt. Noise-only changes cannot produce endlessly accumulating EQ.
- Gain bookkeeping covers candidate normalization/headroom and L/R trims. A known scalar attenuation must not be interpreted as a newly discovered broadband room deficit.
- Dedicated noise acquisition detects stationary floor, tonal interference, a transient, and a changed floor; low-SNR power estimates become uncertain rather than exact notches. Confidence is validated after the inverse/window processing, not only on raw input.
- Separate slow-LF sweeps recover known narrow resonances within declared time-resolution/SNR limits, agree in the overlap with full sweeps, and reject invalid harmonic assumptions for altered sweep laws.
- Effort zero produces no target-shape correction; fractional effort remains fractional after repeated refinement; effort one requests the full target within constraints. Regional smoothing is applied once to each total design, blended continuously, and cannot be undone by retries or change the only displayed graph while leaving playback unchanged.
- User-defined multipoint layouts preserve position/seat weights and center alignment reference; conflicting seat nulls remain protected, and center-only retries cannot acquire a false all-position verification status.
- Physical loopback verifies generation/capture/routing before speaker measurements. Physical room tests compare baseline, candidate prediction, corrected measurement, neighboring positions, and level-matched listening.

## 6. Questions to resolve with the feature specification

1. Is independent USB-mic hardware required on the first release, or is shared-clock I/O the initial supported path? The measurement data model should preserve enough timing evidence either way.
2. Does first-release phase treatment include L/R arrival alignment? This requires a shared timing reference; independent IR peak normalization destroys the needed information.
3. Which existing user DSP stages remain during baseline, and where does the measurement generator enter so that candidate verification measures the same playback path? This choice must be visible in session metadata.
4. User-defined count/layout and manual movement are required for spatial measurement. Decide the default template and whether a given automated refinement verifies only the current position or guides the user through a complete spatial cycle; never conflate their evidence.
5. Which correction FIR duration and latency fit the current engine's CPU budget? Quality should be judged by the realized frequency response and artifact checks, not a tap count alone.
6. What calibrated microphone file formats will be accepted initially? Implement their actual sign/units/sensitivity conventions from first-party examples before promising broad format compatibility.
7. Which regional effort/smoothing controls are ordinary UI and which are advanced? The saved recipe must define their units, ordering, interpolation and feedback objective regardless of presentation.
8. What trustworthy boundary measurement is possible at 20 Hz and 20 kHz for each supported route? Plan safe sweep guard coverage and calibrated uncertainty; show unsupported edge regions rather than pretending the requested domain proves physical response there.

No cited source establishes a universal Harman room target, a universal optimal sweep duration, or guaranteed convergence of N automated correction passes. Those remain explicit app design choices that require synthetic and physical validation.
