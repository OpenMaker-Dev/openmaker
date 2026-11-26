# A web-viewer build through XWayland, at a desktop scale above 1

Screenshots taken 2026-11-26 with the mock provider (`OPENMAKER_PROVIDER=mock`).
Each image compares one `web-viewer` debug build used two ways: through
XWayland, as it runs on a Wayland session, and as a native Wayland window
(`DISPLAY` unset). See
[layout-model.md 9.6](../../re/layout-model.md#96-web-viewer).

- **`sway-scale-2-and-1.5.png`** (sway 1.12, headless, its GLES renderer):
  - Left half at scale 2, right half at 1.5. In each, the window's top left
    corner through XWayland is on top and the native one below, magnified
    three times without smoothing.
  - The XWayland windows are drawn at 1 and stretched by the compositor,
    so they are blurred.
- **`hyprland-scale-2.png`** (Hyprland 0.56, `xwayland:force_zero_scaling`,
  a 1920 × 1080 monitor set to scale 2 for the measurement):
  - The top left corner through XWayland is on top, the native one below,
    unmagnified.
  - The XWayland window is not stretched: it is sharp but drawn at 1, half
    the native window's size.
