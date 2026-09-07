## Highlights

### The first 1% of usage no longer reads as 100%

A lightly used OpenCode Go account could report its rolling window as 100% used while the dashboard showed 1%. The usage page reports each window as either whole percentages or fractions of the limit, and a lone `1` means 1% in one and 100% in the other. The per-window scaling rule turned the first 1% of use into a maxed-out rolling window.

The scale is now resolved once per response from real evidence in the payload, and only read as fractions when a window actually holds a fractional value. The same bug was present in the OpenCode, Qoder, Chutes, and Sakana providers, and it is fixed for all of them.

### OpenCode Go's monthly window has a name

The OpenCode Go card now labels its monthly bar "Monthly" instead of the generic "Extra", so the third usage window on that card is no longer anonymous.

### The Store build submits again

The 1.5.21 Microsoft Store submission was rejected because Partner Center caps installer parameters at 40 characters and the inherited value was longer. The parameters are now normalized to that limit, with startup-prompt suppression and the restart-required exit code preserved, and a deterministic Store-package preparation test guards it in CI.

## Installers

- **Ceiling-1.5.22-Setup.exe** - standard installer
- **Ceiling-1.5.22-portable.exe** - portable
- **Ceiling-1.5.22-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That requires the Start Menu shortcut installed by the standard or Store build.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.21...v1.5.22
