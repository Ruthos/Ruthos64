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

Each segment lists its volume, voxel count and value statistics. **What it is**
sets the code other programs read (bone, tooth, soft tissue, mass, fluid or other
tissue); the name you give a segment is always kept beside it.

## Saving segments

**Save segments to study** writes every segment on the panel as a DICOM
Segmentation object, a new series in the same study that refers to the series it
was drawn on. 3D Slicer, OHIF and the main PACS products read this format. Only
slices holding part of a segment are written, so one small structure in a large
CT makes a small file.

Saving again adds a new version to the same series. Earlier versions remain, and
the newest is what opens. Closing MPR with unsaved changes asks whether to save.

Opening MPR on a series with saved segments offers to load the newest, or loads it
without asking if you choose that in Preferences, under Viewer. A segmentation made
by another program loads the same way, and an unfamiliar code keeps its label and
colour. It is refused, with the reason, if its frames do not line up with this
series: Ruthos never resamples a segment, because one moved without saying so is
worse than none. Binary segmentations only: one holding probabilities, or one
compressed with RLE, is refused for now. An enhanced multi-frame source series
cannot be saved to yet.

- Teaching copies include a study's segments, with new UIDs so they still line up
  and with the creator's name removed. A segment's name is copied, and checked for
  anything that identifies the patient.
- A SEG is sent, routed and retrieved like any other series.
- STL export works on loaded segments as on new ones.
- Reports list the newest saved segments with their volumes and how they were made,
  so a report says whether a volume came from an AI tool.
- Cuts in the 3D pane and volume rendering settings are views, not segments, and are
  not saved. Your name in Preferences, General, is recorded as the creator.

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

### Refine edges

**Refine edges is off by default.** It does well on phantoms but can make things
worse on real anatomy: on an equine mandible, bright teeth set the level so high that
thin cortex beside them was dropped, and the volume fell by 31%. Until that is fixed,
try it and compare with **Show SAM outline**.

The model decides what the object is, and the image decides where its edge lies.
With **Refine edges** on, Ruthos keeps the object the model chose and replaces its
boundary with one read from the CT values, on the slice's own pixels.

For bone, each edge is placed at the half-maximum between the cortex and what lies
beyond that side of it, the established way to find an edge blurred by the scanner.
That is judged side by side, as the wall of a sinus has air on one side and soft
tissue on the other. Soft tissue and Contrast use the midpoint, and stop at anything
much brighter, such as bone.

Refinement stays close to what the model chose. It adds nothing more than the band
width from the model's mask, 3 voxels unless changed in Preferences, under Viewer.
It crosses a faded stretch of cortex only inside the model's mask, and does not climb
into a brighter neighbour outside it, so a bone the model left out stays out. If the
model took in two bones, refinement keeps both: it corrects edges, not the choice of
object.

On a CT phantom blurred like a bone kernel (0.7 mm full width at half maximum, on
0.4 mm voxels), using the real model:

| True thickness | Model alone, whole slice | Refined |
| --- | --- | --- |
| 1 mm | 1.97 mm | 1.07 mm |
| 2 mm | 2.76 mm | 2.02 mm |
| 4 mm | 4.69 mm | 4.00 mm |
| 0.5 mm | 1.72 mm | 0.81 mm |

The wall of a sinus came out within 10%, and a block of bone two voxels from a plate
stayed out. Bone much thinner than the scanner's blur cannot be measured by any
threshold, as its image is mostly the blur: a 0.5 mm plate reads about 60% thick,
much closer than before but not true. These are results on a phantom with voxels a
third the size of a typical equine head CT's. At 1.2 mm voxels, bone a voxel thick
cannot be measured this way. Check a measurement that matters against the slices.

**Show SAM outline** draws the model's own edge, dashed, over the refined segment.
Until a segment is edited or saved, its statistics give both volumes. The list marks
it **AI, refined**, and **edited** once another tool has changed it. Ruthos keeps the
model's own mask only until the segment is saved. After following, slices whose area
differs by more than 40% from the slices either side are listed under **Check
slices**: click a number to go there. Nothing is changed for you.

With Refine edges off, the result is the model's own mask.

Choose what you are finding (Bone, Soft tissue or Contrast), which sets the window
the model sees. Click **Include** or **Exclude**, click a slice, and correct with
further clicks. **Follow through slices** then runs up and down the stack until
the object ends.

- It was trained on natural images and videos, not on medical or veterinary
  images, so check its result on an animal against the slices.
- Masks are made on the series' own slices, whichever pane you click in.
- It follows an object from slice to slice, so motion, as in standing CT, can make
  it slip. Fill slice gaps repairs a single lost slice.
- It fills narrow gaps. Refinement reopened a fracture 3 pixels wide in a thin
  synthetic ring, but a gap deep inside a thick mask is trusted as the model drew it,
  so check fracture lines against the slices.
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
