## Fixed

### Multiple Claude accounts stay separate

Ceiling now reads each configured Claude account through the OAuth credentials in that account's own `CLAUDE_CONFIG_DIR`. Separate profiles such as `.claude` and `.claude-work` no longer collapse onto whichever Claude Desktop, browser, CLI, or token session is globally active.

Single-account automatic detection continues to work as before.

## Installers

- **Ceiling-1.5.26-Setup.exe** - standard installer
- **Ceiling-1.5.26-portable.exe** - portable
- **Ceiling-1.5.26-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That requires the Start Menu shortcut installed by the standard or Store build.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.25...v1.5.26
