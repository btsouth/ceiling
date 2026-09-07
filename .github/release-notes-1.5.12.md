## Highlights

### Antigravity IDE and CLI

Ceiling now detects:

- Google Antigravity **IDE** language servers that advertise `--https_server_port 0` without `--extension_server_port` (Antigravity 2.3+)
- Antigravity **CLI** (`agy` / `antigravity-cli`), which hosts the same local quota API without a CSRF token

If you had "language server not running" while the IDE or `agy` was open and signed in, refresh after updating.

### One card per Codex (or Claude) account

Registering the signed-in home no longer leaves a ghost ambient reading beside it. Overview shows each seat once; removing an account still drops its card.

### Taskbar stays capacity-only

Enabling a provider that is not installed or not signed in no longer adds a blank dash pill to the strip. Setup failures stay on Overview and Settings. This also stops Antigravity error placeholders from looking like a broken "Claude" tile (its quota window is named Claude).

### Readable provider marks

Gemini no longer renders as an empty ring on the native taskbar strip (it had no 16x16 glyph and fell through to a hollow circle). Antigravity gets a mark too. Strip SVG icons keep a stronger brand tint so seats stay identifiable at a glance.

## Installers

- **Ceiling-1.5.12-Setup.exe** - standard installer
- **Ceiling-1.5.12-portable.exe** - portable
- **Ceiling-1.5.12-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

---

Patch on **1.5.11**.

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.11...v1.5.12
