# OpenMaker Go frames

How long OpenMaker Go's engine (`mobile/core`) takes to make a frame while
a pan scrolls an open layout, and how much it hands the native painter.
Measured before and after the engine stopped rasterizing frames itself.

## Reproduce

```sh
GO_PROFILE_HOST=... GO_PROFILE_FILE=... GO_PROFILE_ACCOUNT=... \
GO_PROFILE_PASSWORD=... GO_PROFILE_LICENSE=... \
GO_PROFILE_SIZE=1280x700 GO_PROFILE_SCALE=1.75 \
cargo run --release --locked -p openmaker-go --example frame_profile
```

`GO_PROFILE_PATH` opens a local file instead of a hosted one. The example
reads everything it opens from the environment, so no server, file or
account is written in the repository.

## Method

- **The file** was a production-like hosted solution on the lab FileMaker
  Server (FileMaker Server 26), opened read-only: a layout of about 9,000
  vertices (fields, portals, labels).
- **The machine** was a Linux VM with 8 x86-64 cores. A phone or tablet's
  cores are slower; the device section below has what one took.
- **The canvas** was 1280 x 700 points, about a 10.9-inch tablet in
  landscape under its toolbars, at 1.75 pixels a point (that tablet's
  density, 280 dpi).
- **The frames**: after the file settled, the example sent a pan move and
  waited for the frame it made, 60 to 120 times, scrolling down and up.
  It reports the median, 90th percentile and maximum.
- **Before** is OpenMaker Go's first engine: egui drawn by a CPU
  rasterizer (dream-soft-render) at one pixel a point, the frame encoded as
  a PNG for the app, which decoded it to show it. It was measured with an
  example since removed, in the debug build the apps then shipped and in
  a release build, at 1280 x 700 pixels (the rasterizer's limit was
  921,600 pixels a frame).
- **After** is this engine: egui's pass and tessellation at the device's
  density, packed into vertex, index and batch buffers that the app's GPU
  draws.

## Results

Before, at one pixel a point:

| Stage, ms (median) | Debug build | Release build |
|---|---:|---:|
| egui pass (the window's drawing) | 37.5 | 3.8 |
| tessellation | 1.1 | 0.3 |
| rasterization | 7.4 | 5.3 |
| PNG encoding | 298.1 | 2.6 |
| PNG decoding (on the app's side) | 54.2 | 3.0 |
| **Frame** | **about 398** | **about 15** |

After, at 1.75 pixels a point (release build):

| Stage, ms | Median | 90th percentile | Max |
|---|---:|---:|---:|
| egui pass | 3.30 | 4.18 | 4.77 |
| tessellation | 0.23 | 0.27 | 0.35 |
| packing | 0.07 | 0.08 | 0.10 |
| pan move to frame | 3.95 | 5.03 | 5.84 |

A frame is about 250 KiB of vertices and indices; a frame of pixels at
the device's density would be 11 MiB.

## On a tablet

The same hosted file on a Samsung Galaxy Tab S10 FE 5G (Android 17,
2304 x 1440 pixels at 90 Hz, 1.75 pixels a point), release engine, debug
app, over the tablet's Wi-Fi. The app logs each frame's timings with
`adb shell setprop log.tag.OpenMakerGo DEBUG` (`canvas/FramePainter.kt`);
the frames were driven by injected touches: three pans of 500 to 800
points over 1.2 s each, up, down and sideways, each followed by its glide.
The layout drew about 8,500 vertices in about 290 batches.

| Stage, ms (710 frames) | Median | 90th percentile | 99th percentile |
|---|---:|---:|---:|
| egui pass | 7.2 | 8.4 | 13.0 |
| tessellation | 0.54 | 0.67 | 0.91 |
| packing | 0.19 | 0.23 | 0.33 |
| engine, all three | 7.9 | 9.3 | 14.3 |
| app's paint | 7.2 | 8.2 | 9.2 |
| time between frames | 11.0 | 13.0 | 19.0 |

- **The display's rate**: frames came every 11 ms, the 90 Hz display's
  refresh; 4 of 707 gaps were longer than two refreshes (22 ms). The
  app's paint (taking the frame, texture changes, buffers and draw calls,
  on the GL thread) overlaps the engine's next pass.
- **Flings** went the same until each glide's last 0.7 to 0.9 s, where the
  gaps between frames grew from 25 to 90 ms while the engine took 5 to
  7 ms a frame. A pass that changes nothing draws no frame, so these are
  steps where the slowing content did not move (inferred, not measured).
- **New data** is the slow case: the first frames of another record, or
  of related records arriving, took a 40 to 60 ms pass each, a few frames
  per record.
- **Still**: no frame was made in 5 s without input, with or without a
  dialog of the file's open.
- The tablet's pass took about twice the Linux VM's (3.3 ms).

## Reading

- The apps had shipped the debug engine: most of the time went to the
  debug build's PNG encoder, and the rest to unoptimized egui. Building
  the engine optimized (`mobile/build-core.sh` now does unless asked
  otherwise) makes a frame about 26 times faster on its own.
- Drawing on the GPU removes rasterization and both PNG steps, and draws
  at the device's density (text was blurry at one pixel a point) for less
  than the old frame cost at one.
- What is left is the window's own pass, the same work the desktop client
  does each frame.
- Frames are made only when input or the window asks: a still window costs
  nothing. Before, the apps asked for a frame every 180 ms.
