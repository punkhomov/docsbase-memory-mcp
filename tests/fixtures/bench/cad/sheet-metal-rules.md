# Sheet Metal Rules

Rules that drive the sheet metal environment in the CAD kernel.

## Common errors

The kernel validates feature parameters before a rebuild. A frequent failure is
reported verbatim as:

```
Body thickness and Sheet Metal component rule thickness are different
```

This happens when a body was created with a thickness that no longer matches
the rule assigned to its component. Align the rule thickness or override the
body thickness, then rebuild.

## Diagnostics

Use the rule inspector to compare the body thickness with the component rule
thickness before every release build.
