# GUI header: render the operator's wordmark IMAGE (not a default-font label)

Operator (2026-11-07, third report): the header shows the triangle mark plus
"Cryptographic Triangles" rendered in the default egui font - his styled
wordmark artwork was never wired into the header. fn header drew
brand/mark64 (44px) + a plain ui.label(APP_TITLE).

Fix: the header now draws brand/header (the operator-supplied lockup PNG,
triangle + styled lettering, 600x126 asset) at 48px height (aspect-preserved,
228px wide) in place of mark64 + the text label. Window icon / menus
unchanged. One-line behavioral change, no data paths touched.
