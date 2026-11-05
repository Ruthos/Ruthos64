# 3D and segmentation

Ruthos builds 3D views in the MPR window. A segment is a region of the volume
you have defined, with a name, a colour and a record of the tool that made it.

**The slices stay in charge.** A 3D image is a derived picture that can look
more certain than the slices it came from. Smoothing and AI models can bridge a
real gap, and that gap is sometimes the fracture line or the defect being looked
for. Every tool here is an assistant: nothing is segmented unless you ask, and
every result should be checked against the slices. Ruthos is not a medical
device and is not for diagnostic use.

## The 3D pane

Click **3D** in the MPR toolbar; it takes the fourth pane of the 2×2 layout.
**3D: Rotate** chooses what the left button does: Rotate, Window, Move or Zoom.
The middle button always moves and the right always zooms. The presets menu
beside it holds the presets and the Shaded, MIP and MinIP modes.

## Cutting

Cutting builds a shape first and removes nothing until you press a button.

- **Cut box:** drag the handles to size a box, turning the view to check it, then
  press **Cut inside** or **Cut outside**. The box stays for the next cut.
- **Cut path:** click points or drag to draw, drag a point to move it, then cut
  straight through the depth, from the view the path was started in.
- **Remove piece:** click something, such as the table, and everything connected
  to that point goes.

**Undo cut** and **Restore everything cut** are in the same menu. Cuts belong to
the open 3D pane: they do not change the images or the segments, and they are
not saved.

## Segments

Click **Segments** in the toolbar.

| Tool | What it does |
| --- | --- |
| Threshold | Everything in the value range |
| Region grow | Click a slice: connected voxels join across steps up to the gradient limit, so low-valued thin bone is followed and a strong edge stops it |
| Keep largest | Drops table, collar and noise |
| Split pieces | Each fragment becomes its own segment |
| Close, Open, Grow, Shrink | Morphology, with a radius in millimetres |
| Fill holes | Fills enclosed spaces |
| Fill slice gaps | Fills voxels missing from one slice but present either side |

Each segment lists its volume, voxel count and value statistics. Segments are
not saved with the study yet.

## AI segmentation

The AI tool uses SAM 2.1 Tiny. Click a structure on a slice and the model finds
the object meant, then follows it through the stack. It runs only when you ask,
entirely on your machine, and no image data leaves it. Its result is an ordinary
segment, marked **AI**, that every tool above can edit.

### Detail, and the cut box

The model stretches whatever it is given to 1024 by 1024 pixels and makes its
mask in 256 by 256 cells, whatever the size of the image. On a whole 512 pixel
slice each cell covers two pixels: thin bone comes out thicker than it is, and a
fine fracture line can be filled in. In a test on a synthetic ring of 3 pixel
cortex on a 600 pixel slice, the mask was 75% too large and the gap was filled.
With a box drawn close around the ring, the overlap with the true shape rose from
0.73 to 0.92.

So draw a **Cut box** around the object in the 3D pane before you click. **Keep
to the cut box** gives the model only the box, and follows only through its
slices; nothing is cut from the volume. The panel shows how many pixels each mask
cell covers, and warns when:

- much of the box is empty, or the object spans little of it: a tighter box gives
  more detail;
- the box is long and thin, as the model stretches it square;
- the object reaches a side of the box: it may be cut off there, so make the box
  larger and click again.

The model is not in the download. On first use Ruthos shows its licence (Apache
2.0), source and training data, then fetches about 400 MB, leaving 216 MB on
disk. That is the model and Microsoft's ONNX Runtime and DirectML, each checked
against a fixed SHA-256 before use. The model comes from an ONNX export of Meta's
weights made by a third party. A machine without internet access can install the
model from a folder.

Choose what you are finding (Bone, Soft tissue or Contrast), which sets the window
the model sees. Click **Include** or **Exclude**, click a slice, and correct with
further clicks. **Follow through slices** then runs up and down the stack until
the object ends.

- It was trained on natural images and videos, not on medical or veterinary
  images, so check its result on an animal against the slices.
- Masks are made on the series' own slices, whichever pane you click in.
- It follows an object from slice to slice, so motion, as in standing CT, can make
  it slip. Fill slice gaps repairs a single lost slice.
- It tends to take in edge voxels: treat its volumes as approximate.
- A GPU took about 0.2 s a slice in testing and a CPU about 2 s, so a long series
  on a CPU takes many minutes.

## Export

**Export STL…** writes the selected segment and **Export all shown…** writes each
shown segment as its own file, such as the fragments of a fracture. Meshes are in
millimetres in patient coordinates, and the file header names the series, the
segment and the tool. The status line reports the printed size and whether the
mesh is closed. There is no decimation yet, so files can be large. Exports show
the segment, not what has been cut in 3D.

Exported meshes are for planning and education and do not replace reading the
images.
