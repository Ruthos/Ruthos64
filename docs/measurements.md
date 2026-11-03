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

## LV M-mode

The LV M-mode tool (K) takes four clicks down one time column of an M-mode
strip: septum top, septum bottom, free wall top, free wall bottom. It gives
IVS, LVID and LVFW for diastole or systole; the phase of a new line follows
the series (a systole after an unpaired diastole). Each point must lie below
the last; Backspace removes the last point and Escape abandons the line.
Fractional shortening (LVIDd − LVIDs) / LVIDd is drawn beside the systolic
line and appears in the report's LV M-mode block with the paired values.
Lines are stored as `roi` rows of kind `lvmmode-d` or `lvmmode-s`; their
three values go into `roi_value` under `lv.ivsd`, `lv.lvidd`, `lv.lvfwd`
or the systolic keys.

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
