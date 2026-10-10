# rs-cad

MIT-licensed, read-only CAD inspection and SVG rendering.

## Status

Implemented: bounded DWFX package reading, bounded ASCII DXF rendering and DWG version inspection.

DWFX renders the observed XPS `FixedPage` subset: `Canvas`, `Path` and `Glyphs`. W2X metadata is not rendered yet. DXF supports `LINE` and `CIRCLE` from the `ENTITIES` section. DWG supports only version identification in this release, no geometry decoding or rendering.

## Usage

```sh
cargo run -- inspect drawing.dwfx
cargo run -- render drawing.dwfx drawing.svg
cargo run -- inspect-dxf drawing.dxf
cargo run -- render-dxf drawing.dxf drawing.svg
cargo run -- inspect-dwg drawing.dwg
```

## Security limits

The DWFX reader caps archive entries, expansion size, entry size, render elements and SVG output. It rejects archive paths containing `..`. It does not fetch external resources or resolve XML external entities.

## Licence and provenance

`rs-cad` is MIT-licensed. MIT has no express patent grant. This project does not include ODA, Autodesk SDKs, proprietary converters, LibreDWG, libdxfrw or Autodesk DWF Toolkit code. Format facts and fixture provenance belong in `docs/formats.md` before each parser expands.
