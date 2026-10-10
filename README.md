# rs-cad

MIT-licensed, read-only CAD inspector and renderer.

## Capabilities

- DWFX: bounded OPC package reader, XPS `FixedPage` rendering to SVG, PNG or JPEG.
- DXF: bounded ASCII `ENTITIES` inspection and SVG rendering for `LINE` and `CIRCLE`.
- DWG: version marker inspection only. No DWG geometry decoding or rendering.

DWFX supports the observed XPS `Canvas`, `Path` and `Glyphs` elements. It resolves `SolidColorBrush` and `ImageBrush` resources, embedded fonts and literal canvas clips. W2X layer metadata is reported as unsupported.

## Usage

```sh
cargo run -- inspect drawing.dwfx
cargo run -- inspect drawing.dwfx --page 1
cargo run -- render drawing.dwfx --format svg --output drawing-svg
cargo run -- render drawing.dwfx --page 1 --format png --max-edge 4096 --output drawing-png

cargo run -- inspect drawing.dxf
cargo run -- render drawing.dxf --format svg --output drawing-svg

cargo run -- inspect drawing.dwg
```

SVG retains vector detail when zoomed. PNG and JPEG default to a 2,048 px longest edge. `--max-edge` accepts up to 4,096 px, subject to the pixel allocation limit.

## Limits

DWFX rejects archives with excessive entries, expansion size, entry size, render elements or SVG output. Archive paths containing `..` are rejected. It does not fetch external resources or resolve XML external entities.

## Development

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

## Licence and provenance

`rs-cad` is MIT-licensed. It does not include ODA, Autodesk SDKs, proprietary converters, LibreDWG, libdxfrw or Autodesk DWF Toolkit code. Format facts and fixture provenance are documented in [`docs/formats.md`](docs/formats.md).
