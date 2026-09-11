# sign3mf

`sign3mf` converts prepared PNG, JPEG, WebP, BMP, or TIFF artwork into a flat,
multipart 3MF sign. The library and command line program use millimetres (mm)
for all physical dimensions. Transparent pixels are outside the sign; a requested
background colour is flood-filled from the image border, so enclosed artwork of
the same colour remains intact.

Build and run from this directory:

```text
cargo build --release
sign3mf --input warning.png --output warning.3mf --width 150
sign3mf --input warning.jpg --output warning.3mf --height 120 --mode backed --face-depth 1.0 --remove-background '#FFFFFF' --background-tolerance 20 --trim --upscale 4 --filter cubic --svg-out warning.svg
sign3mf --input plate.png --output plate.3mf --width 180 --hole corners --hole-diameter 5
sign3mf --input plate.png --output rounded.3mf --width 180 --corner-radius 4 --chamfer 0.25
```

Exactly one of `--width` and `--height` is required; both are measured in mm.
The other dimension is derived from the processed artwork bounds. Defaults are
2 colors, 3 mm total thickness, backed mode with a 1 mm face, face-down
orientation (a 180° rotation around Y), and 4× upscaling. Use `--orientation
right-side-up` to keep the face on top of the build plate.
`--mode full-depth` gives every
colour the full requested thickness. `--mode backed` creates a separately
named backing from the visible artwork and puts each colour in the remaining
face depth in mm. `--parts color` groups disconnected shells of one colour into one
mesh; `--parts region` creates one part per disconnected region.

Use `--corner-radius` to fillet the outside sign outline in the XY plane; it is
disabled at `0` and the value is a radius in mm. Use `--chamfer` to bevel the
outside perimeter edges and outline corners in 3D; it is also disabled at `0`
and the value is the bevel distance in mm. Internal colour boundaries remain
shared so chamfering does not create gaps between separately assignable parts.

Use repeatable `--hole` values (`corners`, `center-top`, `center-left`,
`center-right`, or `center-bottom`) to add round through-holes. Values may also
be comma-separated. Hole centres default to 5 mm from the nearest edge and the
default hole diameter is 5 mm; customize them with `--hole-inset` and
`--hole-diameter`. Countersunk holes are not enabled yet.

The 3MF contains one parent component object and a build item referencing it.
Each colour has a stable name such as `color_01_FFCC00` and a standard display
colour. It also includes `Metadata/model_settings.config`, which maps each
distinct colour to a sequential Bambu/Orca extruder slot and keeps same-colour
parts together. The actual filament loaded in each slot remains slicer
configuration and should be checked after import.

The default tracer is a shared-boundary pixel tracer so neighbouring regions
reuse identical coordinates. The optional `vtracer-backend` feature pins
VTracer `1.0.0-alpha.4` and exposes a cutout SVG adapter, while
`lib3mf-backend` keeps the pure Rust lib3mf parser available for round-trip
validation in embedding applications. The normal writer is deliberately
self-contained so the CLI has stable output across lib3mf API revisions.

Photograph detection, perspective correction, raised lettering, mounting-hole
inference, GUI controls, and automatic slicer filament setup are outside this
first version.
