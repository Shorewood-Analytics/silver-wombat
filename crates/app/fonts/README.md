# Bundled fonts

The browser gives WebAssembly no system fonts, so the app carries these three.
All are complete releases under the SIL Open Font License 1.1 (`OFL.txt`).
The JetBrains Mono and IBM Plex files are byte-identical to the upstream repositories at the tags below (checked 2026-11-15).

| File | Family | Version | Used for |
| --- | --- | --- | --- |
| `Inter-Regular.ttf` | Inter | 4.001, from the `inter-ui` npm package 4.1.1, converted from WOFF2 to TTF (a container change; same tables and glyphs) | the interface, and code unless fixed-width is switched on |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono | 2.304, `JetBrains/JetBrainsMono` tag `v2.304` | terminal, diffs, and code when fixed-width is switched on |
| `IBMPlexSans-Regular.ttf` | IBM Plex Sans | 3.005, `IBM/plex` tag `@ibm/plex-sans@1.1.0` | GPUI's web platform names this family as its default system font and fails without it |

"Plex" is a reserved font name, so the IBM Plex file must stay unmodified: do not subset it.
