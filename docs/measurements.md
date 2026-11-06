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

## Anatomical M-mode

On a 2D ultrasound loop, **Anat. M-mode** in the toolbar reads a time and depth
strip along a line you draw, at any angle, after the study. Click two points for a
straight line and more for a curve through them. Drag a dot and the strip follows,
right-click a dot to remove it, and click or drag in the strip to move the loop to
that frame.

A strip has only the frames of its loop: at 50 frames a second that is 20 ms between
columns, where a true M-mode takes about 1 ms. Time is never smoothed, so each frame
shows as a block. Peak systole can fall between frames, and small fast structures can
be missed, so the strip suits dimensions better than fine timing, and the 2D loop
stays the reference. Below 30 frames a second it says so.

It needs a calibrated 2D region and the loop's frame time, and refuses a line that
leaves the region. **Save to study** makes a new series, named with the rate it was
derived at. That is an M-mode image like any other, so the caliper, LV dimensions and
other M-mode tools measure it there.

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

## Tools and the Measurements menu

The two menus sit side by side in the viewer toolbar.

**Tools** holds the mouse tools: WL/WW, Move, Zoom, Rotate, and one
**Measure** row. Click a row to put it on the left mouse button,
right-click to put it on the right.

**Measurements** chooses what Measure draws:

| Group | Tool | Key |
|---|---|---|
| Measure | Caliper: distance, time, velocity | L |
| | LV dimensions: M-mode or 2D | K |
| | Angle | A |
| Area | Rectangle | B |
| | Ellipse | E |
| | Polygon | G |
| Mark | Point: value or velocity | P |
| | Arrow | N |

Below the picker are the measurement set to name from, naming the
selected measurement, undo and delete, calibration, showing measurements
and labels, copying values, and the report. The right-click menu is the
same menu.

**Areas** are computed from the outline, so they work on colour images
too; on single-channel images the label adds the mean, SD, minimum and
maximum of the pixels inside. A polygon is clicked point by point and
closed with a double-click, a click on the first point, or Enter;
Backspace removes the last point and Escape abandons it. Every vertex has
a handle.

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

## One caliper, many quantities

The Caliper (L, formerly Length) reads everything its position allows:

| Where | A caliper line gives | A point gives |
|---|---|---|
| 2D image | distance | pixel value |
| M-mode strip | depth (vertical) and time (across) | |
| Spectral Doppler strip | time, the velocity at each end, the slope | velocity and 4v² gradient |

The scale comes from the ultrasound regions the scanner writes: units
across and down, the size of a pixel in those units, and the baseline
(Reference Pixel Y0) of a Doppler strip.

**The name decides what is recorded.** Each definition has a quantity,
and naming a caliper after it stores that quantity:

| Quantity | Stored | Example definitions | Label shows |
|---|---|---|---|
| length | mm | VSD, LA, Ao | the distance |
| time | ms | RR interval, E deceleration time | the time; for an RR interval also the heart rate |
| velocity | m/s | MV E, TR Vmax (a point, or the faster end of a line) | velocity and 4v² |
| slope | m/s² | E deceleration slope | the rate of change |
| pht | ms | AR and MV pressure half-time | the half-time and the slope it comes from |

Pressure half-time is the time for the peak velocity to fall to
peak/√2 at the line's slope: draw the caliper along the
deceleration from the peak. Where a quantity is not available the label
says so (no time axis here, no velocity scale here, no distance scale
here) instead of recording something else. Unnamed calipers show all
their readings.

The built-in Doppler set covers MV E and A, E deceleration time and
slope, MV and AR pressure half-time, Ao, PA, MR, TR and AR Vmax and the
RR interval, with calculations for E/A, heart rate, MVA by pressure
half-time (220/PHT) and the TR, MR, Ao and PA peak gradients. Repeats of
a named measurement are averaged in the report and in calculations.

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

## Menus and context (November 2026)

The Tools menu holds the navigation tools (WL/WW, Move, Zoom, Rotate), the drawing and annotation tools (Arrow, Text, Point, Rectangle, Ellipse, Polygon, Pencil, Brush, Repulsor) and one Measure row. What Measure measures is chosen in the Measurements menu: Caliper, LV dimensions, Open polygon, Angle, Cobb angle or the angle between two lines.

The Measurements menu starts with the context, **For: Cardiac**, Abdominal, Musculoskeletal or All sets. It follows the study (an echo or a thorax is cardiac; an abdomen, kidney or bladder abdominal; a limb, joint or tendon musculoskeletal; ultrasound with nothing else to go on is cardiac; other studies offer every set) and can be changed in the menu or in the naming popup. Naming a measurement then offers only that context's sets, each as a short submenu. In Preferences > Measurements each set says which contexts it is offered for; a set with none ticked is offered everywhere. The built-in library adds an Abdomen set and a Tendons (equine) set of cross-sectional areas.
