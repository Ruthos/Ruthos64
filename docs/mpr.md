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
| Window and level | Right-drag, or drag anywhere that is not a line or handle; or type the numbers, or pick a preset |
| Pan | Middle-drag |
| Fit a pane again | Double-click it |
| Go back to the original planes | Reset |

Each pane names its plane (Axial, Coronal, Sagittal, or Oblique and the plane it is nearest), shows its position in millimetres along its normal, and labels its edges with the patient directions. The status line gives the value under the pointer and its patient coordinates.

## Thick slabs

Slab sets a thickness in millimetres, and the slab menu chooses how the planes inside it combine:

- **MIP**, maximum intensity: vessels, bone and contrast.
- **MinIP**, minimum intensity: airways and gas.
- **Mean**: a thicker, smoother slice.

## How it works

The slices are sorted along their normal by Image Position (Patient), and the slice spacing comes from the positions rather than from Slice Thickness. Slices with the same position are counted once. If some slices are further apart than the rest, the status line says so, because distances across the gap are then approximate.

The voxels keep the modality values (Hounsfield units on CT). Each plane is resampled with trilinear interpolation into an image with square pixels of a known size, the finest voxel spacing, so window and level, colour tables and the value readout behave as in the 2D viewer.

Curved MPR and 3D rendering are not part of this; they are later on the roadmap.
