# Bundled fonts

Cormorant Garamond and Lora, the two faces the Classical design system calls for
(`docs/DESIGN.md` §Type). Both are licensed under the SIL Open Font License 1.1
(`OFL-*.txt` here) and are therefore redistributable inside the app binary.

These are **static instances cut from the upstream variable fonts** with
`fontTools.varLib.instancer` at weights 400 and 600, so `Font.custom(...)`
resolves them by PostScript name without any variation-axis gymnastics:

| File | PostScript name | Weight |
|---|---|---|
| `Lora-Regular.ttf` | `Lora-Regular` | 400 |
| `Lora-SemiBold.ttf` | `Lora-SemiBold` | 600 |
| `Lora-Italic.ttf` | `Lora-Italic` | 400 italic |
| `CormorantGaramond-Regular.ttf` | `CormorantGaramond-Regular` | 400 |
| `CormorantGaramond-SemiBold.ttf` | `CormorantGaramond-SemiBold` | 600 |

Sources (google/fonts, `main`), all at upstream **Version 3.008**:
- `ofl/lora/Lora[wght].ttf`
- `ofl/lora/Lora-Italic[wght].ttf`
- `ofl/cormorantgaramond/CormorantGaramond[wght].ttf`

Regenerate with the script kept beside this build's notes; the design system
retired bold, so 600 is the ceiling — do not add a 700 cut.

`Lora-Italic` joined at P2. It is pinned with
`instantiateVariableFont(f, {"wght": 400}, updateFontNames=False)` — **not**
`updateFontNames=True`, which rewrites the PostScript name to
`LoraItalic-Italic` and breaks the `Font.custom("Lora-Italic", …)` lookup. 400
is the italic VF's own default instance, so no rename is needed or wanted. It
is a drawn cut (`italicAngle` −3, its own outlines), which is what
`RenderedFaceTests.testItalicIsTheItalicFaceAndNotTheRoman` measures: a
synthesised oblique or a roman fallback would typeset identically to
`Lora-Regular`, and every italic caption in the app would look plausible and be
wrong.
