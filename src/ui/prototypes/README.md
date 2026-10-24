# Room correction UI study

Reviewed room-correction UI reference. The final graph-centered arrangement with a dedicated right-side pass-history column (variant C) was accepted on 23 October 2026 as the structural design for native implementation. Detailed spacing and alignment will follow the existing app metrics during implementation. The HTML remains throwaway code and is not included in the Rust app.

The agreed decision is recorded in the [implementation specification](../../../docs/room-correction-design.md#agreed-layout-and-implementation-boundary). The reviewed variants are archived locally on `codex/room-correction-ui-prototype`. Variant C is the implementation reference; A and B remain comparison artifacts.

From the repository root:

```sh
python3 -m http.server 8765 --bind 127.0.0.1
```

Open [the prototype](http://127.0.0.1:8765/src/ui/prototypes/room-correction.prototype.html). It can also be opened directly as a local HTML file. Fonts come from the repository's existing assets; there are no remote dependencies.

| Variant | Layout | Initial view |
| --- | --- | --- |
| [A](http://127.0.0.1:8765/src/ui/prototypes/room-correction.prototype.html?variant=A) | Existing app shell, saved sessions on the left, controls below the graph | Correction |
| [B](http://127.0.0.1:8765/src/ui/prototypes/room-correction.prototype.html?variant=B) | Guided workflow with a persistent step rail | Setup |
| [C](http://127.0.0.1:8765/src/ui/prototypes/room-correction.prototype.html?variant=C) | Graph and correction controls, with a dedicated 282 px pass-history column on the right | Correction |

Use the floating switcher or left/right arrow keys to compare layouts. Arrow shortcuts leave focused form controls alone. The URL records the layout. Working state stays in memory; explicitly saved source measurements are kept in this browser and survive reload. Aura and Tomorrow toggle dark/light appearance. State shows the relevant live prototype state. Pass history now occupies the right column in every layout; narrow windows hide the left source/guide rail to keep the graph usable.

Try:

- Change the target, overall effort, a region's effort/smoothing, or maximum boost. The illustrative curves update and verification becomes stale.
- Drag across the graph to create a partial-spectrum region; click a region to select it and drag its vertical boundary lines to resize. All regions stay outlined; only the active region is highlighted. Remove selected region restores underlying settings there. Lower/upper fade widths control the blend at each edge. Compare Room response with Correction FIR and isolate L/R.
- Open Setup, add/remove microphone points, drag their top-view positions or edit coordinates, height and weight.
- Measure a point to walk through background, L/R and optional slow LF acquisition. Manual progression waits at each point. Optional hands-free progression presents the next position, waits the configured movement interval, and starts automatically. Pause countdown, Ready now and Stop remain available.
- Open Refine in the top toolbar. Dial attempt/time limits, tolerance, minimum improvement and measurement strategy; expand Advanced refinement limits for damping, step size, SNR and retries. Run demo starts a stationary sequence from existing data. Measure -> Refine starts noise/baseline capture and continues automatically; Refine after measurement is on by default. Multi-point operation prompts or counts down the required mic moves, including return to reference.
- Select a pass in the right column to see its frozen filter/response, changed frequency bands and comparison with its parent. Audition, Prefer by ear and Save pass work independently of the lowest-error marker; later runs append history. Following new passes scrolls only the column, while the graph independently highlights the currently refined frequency band.
- Open Target settings to adjust bounded boost/cut and automatic target-level fitting (enabled by default). The common L/R offset updates without changing target shape.
- Open Measurements in the titlebar, save the current source with a name, reload, and reopen it. Edit effort/smoothing/target without a new measurement. The browser stores the synthetic responses, point layout, recipe and history; the native application uses the managed object store described in the implementation specification.

All measurements, curves and timings are synthetic illustrations. Curve and target-fitting approximations are not the proposed measurement DSP or FIR synthesis. No sound is generated, no audio device is opened, no microphone permission is requested, and no real app preset is written. Explicit Save measurement writes only a named prototype record to this browser's local storage. Emulated endpoint labels here illustrate selection; native simulated-device testing is available through the application's isolated `--simulate` and `--simulate-smoke` launches.

## Style references

- [IRresponsible metrics](../chrome.rs): 24 px controls, 30 px rows, 16 px insets, 8 px gaps, 4 px corners, 282 px sidebar; Inter Display labels and Iosevka values.
- [Current app geometry](../layout.rs) and [theme palette](../../../assets/themes.json).
- [Cordex preferences specification](/Users/ukolov.sergey8/Work/Cordex/cordex/docs/PREFERENCES_UI_SPEC.md): sections, value bars without thumbs, controls, typography and selection states.
- Cordex's preferences spec references `ab-out/prefs-proto/index.html`; that historical prototype is absent from the inspected checkout. The checkout does contain SVG studies and HTML prototypes for other surfaces.

Use the archived prototype as the reviewed interaction reference. Implement production Rust UI through the existing app mechanisms, with native spacing and geometry rather than a literal port of the HTML. Keep prototype variants out of production.
