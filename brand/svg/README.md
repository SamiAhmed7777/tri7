# Triangles vector brand assets

These SVGs reconstruct the historical Cryptographic Triangles identity as clean, resolution-independent artwork. Every file has a transparent canvas and uses native SVG geometry; there are no linked or embedded bitmap images.

## Files

- `brand-mark.svg` — the standalone dimensional triangle. Its red-orange face runs from `#F03A18` and `#E32105` into `#A01000`, with a crisp `#5B2013` keyline, a restrained right-hand facet, and a hard lower depth edge.
- `brand-wordmark.svg` — the wide display lockup. It reuses the mark construction at lockup scale and pairs it with a heavy, tightly set “Triangles” wordmark. Copper highlights (`#F0B27A`, `#E08A3C`, `#8C4A1E`) sit over a dark red-brown keyline and offset depth layer.
- `brand-compact.svg` — the simplified stacked lockup for small placements and dark UI surfaces. Both the triangle and the two-line “Cryptographic Triangles” name use flat brand red (`#E32105`).

## Rendering notes

The lettering uses a strong fallback stack: `DejaVu Sans`, `Liberation Sans`, `Arial`, then generic `sans-serif`, with weight 900 and explicit SVG text fitting. DejaVu Sans is the reference rendering face in this repository's current build environment. Applications that require absolutely invariant glyph outlines should convert the text elements to paths during their packaging/export step.

All three files use `viewBox` coordinates and can be scaled without changing their proportions. Do not add a white or black rectangle behind them; choose the surrounding application surface separately.
