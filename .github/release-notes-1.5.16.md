## Highlights

### Custom date range actually works

1.5.15 added Custom on Estimated API value, but the card could show **No data** even when local logs had spend for those days.

1.5.16 fixes that:

- Custom totals fall back to the scanned daily dollar series when the window key is empty
- The ring stays up while loading (no blank empty-state flash)
- Cleaner date bar with a compact range pill

Pick From / To (inclusive local days, up to 366 days) and you should see dollars and the provider legend when logs exist for that range.

## Installers

- **Ceiling-1.5.16-Setup.exe** - standard installer
- **Ceiling-1.5.16-portable.exe** - portable
- **Ceiling-1.5.16-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

---

Hotfix on **1.5.15**.

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.15...v1.5.16
