# MPR: multiplanar reconstruction

The MPR button in the viewer's toolbar opens the current series in its own window as three linked planes. A series qualifies when it is a stack of at least three single-frame grayscale slices: a CT, an MR or a cone-beam CT series.

## The window

The three panes start square to the acquisition. For an axial CT they are axial, coronal and sagittal. Each pane has a coloured border, and the same colour marks its plane as a line in the other two panes. The planes meet at the crosshair.

| To | Do this |
|---|---|
| Move one plane | Drag its line in another pane |
| Move the crosshair, and so two planes | Drag where the lines cross |
| Tilt the planes (oblique MPR) | Drag a round handle on a line. The planes turn about the pane's own axis; that pane keeps still while the other two turn |
| Step through a pane's own plane | Mouse wheel |
| Zoom | Ctrl and the wheel, or pinch |
| Window and level | Right-drag; with the Crosshair tool, drag anywhere that is not a line or handle; or type the numbers, or pick a preset |
| Pan | Middle-drag |
| Fit a pane again | Double-click it |
| Go back to the original planes | Reset |

Each pane names its plane (Axial, Coronal, Sagittal, or Oblique and the plane it is nearest), shows its position in millimetres along its normal, and labels its edges with the patient directions. The status line gives the value under the pointer and its patient coordinates.

## Tools, layouts and the lock

The second toolbar picks what the left button does, as in the 2D viewer; the letter in brackets is the shortcut.

- **Crosshair** (C) moves and tilts the planes, as described above. Elsewhere in the pane it sets window and level.
- **WL/WW** (W), **Move** (M) and **Zoom** (Z) adjust the view.
- **Caliper** (L), **Angle** (A), **Rectangle** (B), **Ellipse** (E), **Arrow** (N), **Point** (P) and **Text** (X) measure and annotate. Drag a caliper, rectangle, ellipse or arrow; drag the first arm of an angle and click its end; click to place a point or a text note.

Measurements are in millimetres from the plane's square pixels. A measurement belongs to the plane it was drawn on: it shows whenever that plane is on screen and hides when the plane moves. Click one to select it, drag its handles or its outline to change it, and press Delete to remove it.

**Lock planes** stops the planes moving or tilting, by drag or by the wheel, so measuring cannot shift them by accident.

**Layout** chooses three panes in a row, a 2×2 grid, or one pane on its own. In the 2×2 grid the fourth pane shows the original slice through the crosshair, square to the acquisition, which keeps the scan's own orientation in view while the other planes are tilted.

## Saving

**Save to study** writes the pane last worked in as a new series in the same study:

- **This plane**, as one image with its measurements; or
- **a stack** of parallel planes across the volume, a chosen distance apart, each as thick as the slab if one is set. Measurements on any of those planes go with them.

The images are of the source's kind (a CT reformat is a CT image), marked as derived and reformatted, with their own position, orientation and pixel size. They open, measure and report like any other image, and the database window shows the new series straight away.

**Save image** writes the pane as a PNG or JPEG picture, windowed as shown.

## Thick slabs

Slab sets a thickness in millimetres, and the slab menu chooses how the planes inside it combine:

- **MIP**, maximum intensity: vessels, bone and contrast.
- **MinIP**, minimum intensity: airways and gas.
- **Mean**: a thicker, smoother slice.

## How it works

The slices are sorted along their normal by Image Position (Patient), and the slice spacing comes from the positions rather than from Slice Thickness. Slices with the same position are counted once. If some slices are further apart than the rest, the status line says so, because distances across the gap are then approximate.

The voxels keep the modality values (Hounsfield units on CT). Each plane is resampled with trilinear interpolation into an image with square pixels of a known size, the finest voxel spacing, so window and level, colour tables and the value readout behave as in the 2D viewer.

Curved MPR and 3D rendering are not part of this; they are later on the roadmap.
