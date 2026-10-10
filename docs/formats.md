# Format Provenance

## DWFX

The initial reader follows the XPS-compatible structure observed in local test documents:

- OPC ZIP package
- `FixedDocumentSequence.fdseq`
- `FixedDocument.fdoc`
- `FixedPage.fpage`
- XPS namespace elements `Canvas`, `Path`, `Glyphs`

W2X is treated as metadata until independently implemented. Do not copy parser code or fixtures from GPL, proprietary, or licence-unclear sources.

## DXF

The initial ASCII reader parses `ENTITIES` and renders `LINE` and `CIRCLE`. It rejects malformed tag pairs and non-finite values. Other entities are returned as diagnostics, not silently ignored.

## DWG

The reader identifies only the first six-byte version marker:

- AC1015, R2000
- AC1018, R2004
- AC1021, R2007
- AC1024, R2010
- AC1027, R2013
- AC1032, R2018

It does not decode DWG sections, objects or geometry yet. Every future decoder addition needs a public-reference citation and independently created or permissively licensed fixture.
