# The dial

Ruthos uses a rotary dial as a precise one-axis control on Windows. It works with the Microsoft Surface Dial, the ASUS Dial on the StudioBook and the ASUS DialPad on the ProArt touchpad. Use it to step through frames and slices and to fine-tune window width, window level, a measurement point or the loupe. Zoom stays on pinch and coarse windowing on the right-drag.

The dial is optional. It is set up in Preferences > Viewer > Dial and is on by default. Without a dial nothing changes, and every action also has a keyboard shortcut.

## Setting up an ASUS dial

Ruthos talks to the dial through Windows' wheel interface (RadialController). In the ASUS Dial software, set the dial to **Microsoft Wheel Device**. The dial should then appear in Windows Settings under Bluetooth and devices > Wheel, and Ruthos's modes appear in its on-screen menu.

ASUS does not say whether every ProArt DialPad supports that mode. If yours does not, use the keyboard route below.

## Modes

A press of the dial moves to the next mode, skipping any that cannot act on the image. Clockwise means forward, deeper, wider, brighter or further along the measurement; Preferences can reverse it.

| Mode | One detent | Available when |
|---|---|---|
| Navigate | One frame of a loop, otherwise one image of the series | Always |
| Window width | 1 per cent of the image's value range, at least 1 | Greyscale images |
| Window level | 0.5 per cent of the image's value range, at least 1 | Greyscale images |
| Measurement point | The selected measurement's active point, 0.1 pixel along the measurement (vertically for an LV M-mode point) | A measurement is selected |
| Loupe magnification | The next magnification | The loupe is on |

- The dial acts on the image under the pointer, or on the active one.
- Locked and synchronised cells follow it as they follow the mouse wheel.
- Navigate stops at the first and last frame or image, and pauses a playing loop. Turning fast moves five frames or images a detent; the other modes never accelerate.
- A run of measurement-point detents is one step for Undo.
- The mode and its value show briefly at the top of the image.

The value range is what the image can store, held to the range it actually spans when it stores far more bits than it uses. One detent is 3 on an 8-bit ultrasound frame and about 40 HU on a 12-bit CT.

## Keyboard shortcuts

Each shortcut names its mode, so it does the same thing whatever is selected. The plain arrow keys keep their usual jobs.

| Action | Anticlockwise | Clockwise |
|---|---|---|
| Navigate | Ctrl+Shift+Left | Ctrl+Shift+Right |
| Window width | Ctrl+Shift+Down | Ctrl+Shift+Up |
| Window level | Ctrl+Shift+Page Down | Ctrl+Shift+Page Up |
| Measurement point | Alt+Left | Alt+Right |
| Loupe magnification | Ctrl+Shift+Minus | Ctrl+Shift+Plus |
| Next mode (a press) | | Ctrl+Shift+M |

## The ProArt Creator Hub route

When the DialPad cannot act as a Microsoft Wheel Device, map it to the shortcuts instead:

1. In ProArt Creator Hub, open Application Edit and add `ruthos.exe`.
2. Create one function for each mode, giving it the anticlockwise and clockwise keystrokes from the table.
3. The ASUS menu then switches between them on a press.

Ruthos still shows the mode and value, because each keystroke names its mode. Two things differ from the native route: each detent arrives as one keystroke, so Navigate does not accelerate, and the ASUS menu decides which modes are offered.

## Preferences

| Setting | Values | Default |
|---|---|---|
| Use dial | On, off | On |
| Modes in the press cycle | Any of the five | All five |
| Mode at start | Any enabled mode | Navigate |
| Degrees per step | 5, 10, 15, 20 | 10 |
| Accelerate fast turns in Navigate | On, off | On |
| Reverse direction | On, off | Off |
| Show mode indicator | On, off | On |

Dial settings apply at once, so you can turn the dial, adjust and turn again; Save keeps them and Cancel puts them back. The Dial section appears only on Windows.
