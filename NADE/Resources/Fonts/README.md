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
| `CormorantGaramond-Regular.ttf` | `CormorantGaramond-Regular` | 400 |
| `CormorantGaramond-SemiBold.ttf` | `CormorantGaramond-SemiBold` | 600 |

Sources (google/fonts, `main`):
- `ofl/lora/Lora[wght].ttf`
- `ofl/cormorantgaramond/CormorantGaramond[wght].ttf`

Regenerate with the script kept beside this build's notes; the design system
retired bold, so 600 is the ceiling — do not add a 700 cut.
