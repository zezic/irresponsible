"""Regenerate the AutoEq parity fixture with NumPy and SciPy installed.

Run from the repository root: python tests/fixtures/generate-autoeq.py
The upstream methods execute directly; this script does not reimplement them.
AutoEq's MIT license is in assets/AutoEq-LICENSE.
"""

import ast
import csv
import json
from pathlib import Path
from urllib.request import urlopen

import numpy as np
import scipy
from scipy.interpolate import InterpolatedUnivariateSpline
from scipy.signal import find_peaks, firwin2, minimum_phase, savgol_filter

REVISION = "7ae0f56d53074872b028649617a22bbb4232feb7"
BASE = f"https://raw.githubusercontent.com/jaakkopasanen/AutoEq/{REVISION}/autoeq"


def source(name):
    return urlopen(f"{BASE}/{name}.py").read().decode()


namespace = dict(np=np, savgol_filter=savgol_filter, find_peaks=find_peaks)
exec(compile(source("utils"), "autoeq/utils.py", "exec"), namespace)
namespace.update(
    DEFAULT_SMOOTHING_WINDOW_SIZE=1 / 12,
    DEFAULT_TREBLE_SMOOTHING_WINDOW_SIZE=2.0,
    DEFAULT_TREBLE_SMOOTHING_F_LOWER=6000.0,
    DEFAULT_TREBLE_SMOOTHING_F_UPPER=8000.0,
    DEFAULT_TREBLE_F_LOWER=6000.0,
    DEFAULT_TREBLE_F_UPPER=8000.0,
    DEFAULT_TREBLE_GAIN_K=1.0,
    DEFAULT_MAX_GAIN=6.0,
    DEFAULT_MAX_SLOPE=18.0,
)
methods = {
    "_smoothen", "smoothen", "equalize", "protection_mask", "limited_ltr_slope",
    "limited_rtl_slope", "find_rtl_start",
}
upstream = ast.parse(source("frequency_response"))
cls = next(n for n in upstream.body if isinstance(n, ast.ClassDef) and n.name == "FrequencyResponse")
cls.body = [n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name in methods]
exec(compile(ast.fix_missing_locations(ast.Module(body=[cls], type_ignores=[])), "autoeq/frequency_response.py", "exec"), namespace)
FrequencyResponse = namespace["FrequencyResponse"]


def init(self, name, frequency, raw):
    self.frequency = np.asarray(frequency)
    self.raw = np.asarray(raw)
    self.error = np.array([])
    self.smoothed = np.array([])


FrequencyResponse.__init__ = init
FrequencyResponse.reset = lambda self, **kwargs: None
f = np.asarray(namespace["generate_frequencies"](20.0, 20000.0, 1.01))
x = np.log2(f / 1000)


def curve(frequency, db):
    return dict(frequency=np.asarray(frequency, dtype=np.float32).tolist(), db=np.asarray(db, dtype=np.float32).tolist())


def interpolate(c):
    return InterpolatedUnivariateSpline(np.log10(c["frequency"]), c["db"], k=1)(np.log10(f))


def reference(name, measured, target, parameters=None):
    parameters = parameters or {}
    fr = FrequencyResponse(name=name, frequency=f, raw=interpolate(measured))
    fr.error = fr.raw - interpolate(target)
    result = fr.equalize(**parameters)
    settings = {"normalization": "Raw"}
    keys = dict(window_size="smoothing_octaves", treble_window_size="treble_smoothing_octaves",
                treble_f_lower="treble_start_hz", treble_f_upper="treble_end_hz",
                treble_gain_k="treble_gain", max_slope="slope_limit_db_per_octave",
                max_slope_decay="slope_decay", max_gain="boost_limit_db")
    for key, value in parameters.items():
        if key == "concha_interference":
            settings[key] = "Present" if value else "Absent"
        else:
            settings[keys[key]] = value
    return dict(name=name, measured=measured, target=target, settings=settings,
                expected=np.round(result[0], 7).tolist())


flat = curve(f, np.zeros(len(f)))
cases = []
for name, error, parameters in [
    ("flat", np.zeros(len(f)), {}),
    ("monotonic", x * 4, {}),
    ("narrow-notches", 5 * np.sin(x * 2) - 24 * np.exp(-((x + 1) / .07) ** 2), {}),
    ("protected-cuts", 10 * np.sin(x * 3) + 5 * np.sin(x * 8), {}),
    ("treble-transition", 10 * np.sin(x * 17) * np.exp(-((x - 2.8) / 1.2) ** 2), {}),
    ("plateaus", np.round(5 * np.sin(x * 3)), {"window_size": 1.0 / 3.0}),
    ("minimum-window", 5 * np.sin(x * 3), {"window_size": 1.0 / 24.0}),
    ("concha", -22 * np.exp(-((x - 3.2) / .08) ** 2) + 4 * np.sin(x * 2), {"concha_interference": True}),
    ("decay-and-treble-gain", 12 * np.sin(x * 5), {"max_slope_decay": .5, "treble_gain_k": .25}),
    ("full-decay", 12 * np.sin(x * 5), {"max_slope_decay": 1.0}),
    ("wide-smoothing", 12 * np.sin(x * 5), {"window_size": 4.0, "treble_window_size": 4.0}),
]:
    cases.append(reference(name, curve(f, error), flat, parameters))

sparse_f = [30, 100, 400, 1000, 3000, 6000, 7500, 10000, 15000]
cases.append(reference("uneven-grid", curve(sparse_f, [2, -4, 3, 0, 8, -3, 10, -7, 2]), flat))
root = Path(__file__).resolve().parents[2]
with (root / "tests/fixtures/dt990-pro.csv").open() as handle:
    rows = list(csv.DictReader(handle))
measured = curve([float(row["frequency"]) for row in rows], [float(row["raw"]) for row in rows])
with (root / "tests/fixtures/harman-over-ear-2018.csv").open() as handle:
    rows = list(csv.DictReader(handle))
target = curve([float(row["frequency"]) for row in rows], [float(row["raw"]) for row in rows])
cases.append(reference("dt990-harman", measured, target))

impulses = []
for rate in [44100, 48000]:
    for normalization in ["Raw", "Peak"]:
        c = curve(f, cases[-1]["expected"])
        n = 8192 if normalization == "Peak" else 2048
        hz = np.linspace(0, rate / 2, n)
        sampler = InterpolatedUnivariateSpline(np.log10(c["frequency"]), c["db"], k=1)
        db = sampler(np.log10(np.maximum(hz, max(c["frequency"][0], hz[1]))))
        if normalization == "Peak":
            db -= np.max(db) + .2
        gain = 10 ** (db / 10)
        gain[-1] = 0
        linear = firwin2(2 * n, hz, gain, fs=rate)
        ir = minimum_phase(linear, n_fft=len(linear))
        impulses.append(dict(rate=rate, normalization=normalization, length=f"Taps{n}",
                             curve=c, coefficients=np.round(ir, 9).tolist()))

grid = np.asarray(f, dtype=np.float32).tolist()
for case in cases:
    if case["target"] == flat:
        case["target"] = None
    for key in ["measured", "target"]:
        if case[key] is not None and case[key]["frequency"] == grid:
            del case[key]["frequency"]
for impulse in impulses:
    del impulse["curve"]
output = dict(revision=REVISION, scipy=scipy.__version__, frequency=grid,
              impulse_curve=c, cases=cases, impulses=impulses)
(root / "tests/fixtures/autoeq-parity.json").write_text(json.dumps(output, separators=(",", ":")) + "\n")
