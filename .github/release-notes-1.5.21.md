## Highlights

### One notification per scheduled reset

Ceiling could send the same Copilot monthly-reset notification two or three times. One quota window confirmed its reset while another was still awaiting confirmation, so the confirmed window's old baseline survived long enough to replay the same reset.

Each confirmed window now advances independently, and a second guard suppresses replay of the same scheduled reset cycle.

### Safer local state

Ceiling now replaces settings, credentials, history, geometry, and cache files atomically. If the app or Windows interrupts a save, the previous complete file remains available instead of being replaced by a partial one.

### Account history stays with the right account

Chart history, quota-run efficiency, and caches now consistently use the stable account ID, falling back to email or organization when needed. This prevents seats that share an email from blending data and restores history for organization-only providers.

### Claude refresh failures stay contained

Claude now reports HTTP client setup failures instead of panicking a background refresh task, and OAuth credential refreshes use reliable atomic replacement on Windows.

## Installers

- **Ceiling-1.5.21-Setup.exe** - standard installer
- **Ceiling-1.5.21-portable.exe** - portable
- **Ceiling-1.5.21-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That requires the Start Menu shortcut installed by the standard or Store build.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.19...v1.5.21
