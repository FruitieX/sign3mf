# sign3d

`sign3d` converts prepared PNG, JPEG, WebP, BMP, or TIFF artwork into a flat,
multipart 3MF sign. The library and command line program use millimetres (mm)
for all physical dimensions. Transparent pixels are outside the sign; a requested
background colour is flood-filled from the image border, so enclosed artwork of
the same colour remains intact.

Build and run from this directory:

```text
cargo build --release
sign3d --input warning.png --output warning.3mf --width 150
sign3d --input warning.jpg --output warning.3mf --height 120 --mode backed --face-depth 1.0 --remove-background '#FFFFFF' --background-tolerance 20 --trim --upscale 4 --filter cubic --svg-out warning.svg
```

Exactly one of `--width` and `--height` is required; both are measured in mm.
The other dimension is derived from the processed artwork bounds. Defaults are
2 colors, 3 mm total thickness, backed mode with a 1 mm face, upside-down
orientation, and 4× upscaling. Use `--orientation right-side-up` to keep the
normal orientation.
`--mode full-depth` gives every
colour the full requested thickness. `--mode backed` creates a separately
named backing from the visible artwork and puts each colour in the remaining
face depth in mm. `--parts color` groups disconnected shells of one colour into one
mesh; `--parts region` creates one part per disconnected region.

The 3MF contains one parent component object and a build item referencing it.
Each colour has a stable name such as `color_01_FFCC00` and a standard display
colour. Display colours are metadata; assign filaments to the imported parts
manually in Bambu Studio or OrcaSlicer after importing the assembled model.

The default tracer is a shared-boundary pixel tracer so neighbouring regions
reuse identical coordinates. The optional `vtracer-backend` feature pins
VTracer `1.0.0-alpha.4` and exposes a cutout SVG adapter, while
`lib3mf-backend` keeps the pure Rust lib3mf parser available for round-trip
validation in embedding applications. The normal writer is deliberately
self-contained so the CLI has stable output across lib3mf API revisions.

Photograph detection, perspective correction, raised lettering, mounting-hole
inference, GUI controls, and automatic slicer filament setup are outside this
first version.
