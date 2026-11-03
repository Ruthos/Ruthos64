# Measurements

How Ruthos measures in the 2D viewer, what it stores, and how definitions,
sets and calculations are organised.

## The scale of every measurement

Every length, angle and area carries a *basis*: where its millimetres came
from. The viewer shows the basis in the corner annotation ("Scale: …"), the
report shows it in a column, and values that are not at the patient's scale
carry a marker on the image (`†` detector plane, `≈` estimated magnification,
`?` calibration type not stated, `px` uncalibrated).

| Priority | Basis | When |
|---|---|---|
| 1 | Hand calibration | The user calibrated from a known length (scoped to an ultrasound region when the image has them) |
| 2 | Ultrasound region | Inside a region of the Sequence of Ultrasound Regions with physical deltas |
| 3 | Calibrated | Pixel Spacing with Pixel Spacing Calibration Type GEOMETRY or FIDUCIAL |
| 4 | Patient plane | Cross-sectional modalities (CT, MR, PT, NM, OPT) with Pixel Spacing |
| 5 | Magnification estimated | Projection radiography with Imager Pixel Spacing and Estimated Radiographic Magnification Factor |
| 6 | Calibrated, type unstated | Projection radiography with Pixel Spacing but no calibration type |
| 7 | Detector plane | Projection radiography with Imager Pixel Spacing only |
| 8 | Uncalibrated | Nothing usable: values in pixels |

Inside an ultrasound strip whose horizontal axis is time (M-mode, spectral
Doppler) only vertical lengths are measured; angles and areas are refused,
and a length that crosses two regions is refused. The Measurements menu's
"Show calibration basis" panel lists the attributes read and the basis
chosen for the current image.

## Editing

Every measurement has handles: its points, or the four corners of a
rectangle or oval. Drag a handle to move only that point; drag the outline
to move the whole measurement. Tab cycles the active handle, the arrow keys
move it by one image pixel, Shift+arrow by a tenth, Escape during a drag puts
the point back, and Ctrl+Z undoes the last edit in that viewer. Edits update
the stored row, so a measurement keeps its id, name and creation time.

## The loupe

While a point is placed or a handle dragged, a magnified view of the image
sits beside the pointer, drawn by the GPU from the same pixels, window and
colour table with no smoothing. Above six screen pixels per image pixel a
grid outlines the pixels. Alt while hovering previews it; Shift slows the
pointer by the magnification. Preferences > Measurements sets the
magnification (2–8×), size (120–200 px) and whether Shift slows movement.

## The Measurements menu

In the viewer toolbar and on right-click: the tools with their shortcuts
(W M Z T L A B E N P K), the measurement set to name from, naming the
selected measurement, undo and delete, calibration, showing or hiding
measurements and labels, copying values as tab-separated text, and the
report.

## LV dimensions (M-mode and 2D)

The LV tool (K) takes four clicks along one line: septum top, septum
bottom, free wall top, free wall bottom. It gives IVS, LVID and LVFW for
diastole or systole.

- **In an M-mode strip** the first click fixes the time column and each
  later point must be deeper than the last.
- **On a 2D image** the line runs from the first point in any direction;
  each point must be further along than the last, and the two inner
  points are placed on the line from the first to the last.

Afterwards the ends move anywhere, which rotates, shortens or lengthens
the line, and the inner points keep their places along it; an inner
point slides along the line. In an M-mode strip each thickness is the
vertical distance between its points, so a slanted line still measures
at one instant. Backspace removes the last point and Escape abandons the
line.

**Several sets.** Measure as many cycles as you like. In an M-mode strip
each systolic line pairs with the nearest unpaired diastolic line to its
left, as cycles run left to right; on a 2D loop, diastole and systole are
different frames, and lines pair in frame order. The phase of a new line
follows that pairing, and the Measurements menu changes it. FS is drawn
beside each systolic line. The report lists every set with its FS and,
with two or more, the mean of each value.

Lines are stored as `roi` rows of kind `lvmmode-d`/`lvmmode-s` (M-mode) or
`lv2d-d`/`lv2d-s` (2D); their three values go into `roi_value` under
`lv.ivsd`, `lv.lvidd`, `lv.lvfwd` or the systolic keys.

## Spectral Doppler and M-mode time

Inside a spectral Doppler strip the scale comes from the ultrasound
region's velocity axis and its baseline (Reference Pixel Y0):

- **A point** gives the velocity in m/s and the simplified Bernoulli
  gradient, 4v² in mmHg. Name it MV E, Ao Vmax, TR Vmax and so on.
- **A line** gives the time between its ends in ms, the velocities at both
  ends and the slope in m/s², for deceleration times and pressure
  half-times.

In an M-mode strip a line gives its vertical distance and, when its ends
are at different times, the time between them.

The built-in Doppler set covers MV E and A, E deceleration time, Ao, PA,
MR, TR and AR Vmax and AR pressure half-time, with calculations for E/A
and the TR, MR, Ao and PA peak gradients. Named repeats of any
measurement are averaged in the report and in calculations.

## Definitions, sets and calculations

A *definition* names something measurable: a key (`lv.lvidd`), a label
("LVIDd"), the tool that produces it, its quantity and unit, decimals, an
optional coded concept and species. A *set* groups definitions for naming.
A *calculation* derives a number with an expression over keys: numbers,
`+ − × ÷`, `^`, parentheses and `sqrt`, `abs`, `min`, `max`. Lengths are
converted to the calculation's input unit first, so Teichholz is written in
centimetres as `7 / (2.4 + lv.lvidd) * lv.lvidd^3`. Whether a calculation is
scale independent (a ratio such as VHS) is worked out automatically, and a
scale-dependent result from a detector-plane input is flagged in the report.

Ruthos ships a built-in library (echo, radiography, orthopaedic). Built-in
entries are read-only; "Edit a copy" stores a copy under the same key that
stands in for it, and deleting the copy restores the built-in entry. The
user's entries live in `settings.json` under `measurements`. Export writes
the whole library as JSON; Import merges a file by key after showing what it
would replace.

```json
{
  "definitions": [
    { "key": "custom.spleen", "label": "Spleen thickness", "tool": "length",
      "quantity": "length", "unit": "mm", "decimals": 1 }
  ],
  "sets": [ { "name": "Abdomen", "keys": ["custom.spleen", "kidney.left"] } ],
  "calculations": [
    { "key": "custom.ratio", "label": "Spleen : kidney",
      "expression": "custom.spleen / kidney.left", "input_unit": "cm",
      "result_unit": "", "decimals": 2 }
  ]
}
```

Measurements named before the library existed are linked to definitions by
label when the application starts, and an old `settings.json` with label
sets is migrated once, with the original kept as `settings.json.bak`.
