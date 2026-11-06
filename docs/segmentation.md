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
The **Tool** menu works here too. WL/WW, Move and Zoom do the same as on a slice,
and any other tool, such as Crosshair or a measuring tool, turns the view, so it
turns without changing tool. The right button follows its own tool in the same way,
and the middle button always moves. **Reset** takes every pane back, 3D included,
and leaves cuts alone.

The **3D** menu holds how the picture looks: the presets, Shaded, MIP and MinIP,
looking from the front, back, left, right, top or bottom, and turning by 90 degrees.
Left and right are the patient's, and up is towards the head, as the scanner
recorded them, so check them against a series positioned another way. **Close 3D**
is at the foot.

The view turns around the middle of the volume, or of the box while a box is
cutting it. To turn around something else, choose **Click to set** under Rotation
centre and click it. A cross marks the point, and **Use the middle of the volume**
goes back.

## Cutting

**Cuts** holds the standard cuts: a half, quarter or eighth of the patient, named
for the part that stays. Each is a box with a dot on every face: drag a dot and the
view follows as you drag. Nothing is lost, and **No cut** shows everything again.
**Remove this part instead** hides what the box holds, which opens up the inside.

**Cut by hand**, in the same menu, gives the left button a job until you pick a
tool or press Esc:

- **Cut box:** as above, starting from the whole volume. **Cut inside** or **Cut
  outside** make the cut permanent, until you undo it. The box stays for the next
  cut.
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

Each segment lists its volume, voxel count and value statistics. Click its colour
square to change the colour, by RGB, HSL or a hex code. **What it is**
sets the code stored with it (bone, tooth, soft tissue, mass, fluid or other
tissue); the name you give a segment is always kept beside it.

## Saving segments

**Save segments to study** writes every segment on the panel as a DICOM
Segmentation object, a new series in the same study that refers to the series it
was drawn on. Opening these in other viewers has not been confirmed. Only
slices holding part of a segment are written, so one small structure in a large
CT makes a small file.

Saving again adds a new version to the same series. Earlier versions remain, and
the newest is what opens. Closing MPR with unsaved changes asks whether to save.

Opening MPR on a series with saved segments offers to load the newest, or loads it
without asking if you choose that in Preferences, under Viewer. An unfamiliar code
keeps its label and colour. A segmentation is refused, with the reason, if its frames do not line up with this
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

### Choosing a model

**Model** in the AI section lists what is available, and is remembered:

| Model | How it works | Licence |
| --- | --- | --- |
| SAM 2.1 Tiny | Slice by slice, then follows the object | Apache 2.0 |
| VISTA3D | The whole object in 3D from one click, at 1.5 mm | NVIDIA Open Model License |
| nnInteractive | The whole object in 3D from one click, on the series' own voxels | CC BY-NC-SA 4.0, non-commercial |

A 3D model works like the others: click a structure and see it on the slice, correct
it with **Include** and **Exclude**, then press **Follow through slices**. The whole
click looks at a window around it, and Follow moves that window along for as long as
the object carries on, so a long structure takes several steps. A progress bar shows
how far it has got, with a **Stop** button beside it: what it has found stays, and Follow
carries on from there. Statistics and the 3D pane wait until Follow. A further click after that goes back to the slice. The cut box is
not used, and Refine edges does not apply. **Finding** still matters: a 3D model finds an object,
not a material, and on thin bone a click a few voxels off can come back as bone and
muscle together. So with **Bone** chosen, a click takes the nearest bone within about
4 voxels, and the result keeps only what is joined to your clicks, along with anything
it encloses that the model took, such as marrow.

Below **Finding** is a line from dark to bright, as on the image, with two handles.
Only what lies between them is kept, for Bone, Soft tissue and Contrast alike. The bars
show the values in what the model found, so you can see where the bone or tissue is and
drag a handle to it. Bring the bottom handle down to take in the grey edge of thin bone,
or up to keep to dense bone. It changes at once, as the model need not run again, and
values up to 2 voxels past the model's own edge can be taken too. To set a handle from
the image, press **Pick bottom** or **Pick top** and click a point on a slice: the handle
goes to that point's value. Hover over the line for the values in HU. Turn off **Keep to bone** to see everything the model found.

The window is about 19 cm for VISTA3D and 192 voxels for nnInteractive. With a range
set, Follow goes where voxels in that range leave the window, so with Bone it stays with
the bone. It goes through at most 24 windows.

VISTA3D is trained on human CT, and NVIDIA's model card says it is for research and
not for clinical use. It works at 1.5 mm and treats everything above about 1050 HU
alike, so teeth and dense bone can run together.

VISTA3D and nnInteractive appear only in Research mode: VISTA3D because NVIDIA's
model card and the NVIDIA Open Model License restrict it to research and non-clinical
use, nnInteractive because its licence is non-commercial. Each shows its terms before
first use. Both are still being tested, and are installed with **From a folder…**, as
neither can be downloaded from Ruthos yet.

### Research mode

**Research mode**, in Preferences under Viewer, offers models still being tested, and
those whose licences allow only research and other non-commercial use. VISTA3D is one
of those, by NVIDIA's model card. nnInteractive is another: its weights are
DKFZ's, under CC BY-NC-SA 4.0. It shows its terms before first use. Paid clinical
work, including work in a practice that charges its clients, is not covered unless
DKFZ has given you permission. Whether your use is allowed is your responsibility.

nnInteractive needs about 5 GB of GPU memory. Without that it runs on the CPU, at
about 9 s a click, and gives the same result. If a GPU fails or runs out of memory
during a click, Ruthos repeats the click on the CPU rather than show a bad result.

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
