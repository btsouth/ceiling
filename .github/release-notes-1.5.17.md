## Highlights

### Windows notifications actually stick around

If a reset alert flashed past while you were looking elsewhere, it was gone for good. Ceiling's toasts appeared as a banner for a few seconds and never reached the Windows 11 notification center.

They were published under an app identity Windows did not recognise, so it showed the banner and then discarded it. 1.5.17 publishes under the installed app's real identity, so alerts collect in the notification center like every other app, and repairs the setting on launch if Windows already marked Ceiling as banner-only.

Toasts also carry the Ceiling name and logo now, instead of arriving as unattributed text.

### Resets you were actually waiting for

Two changes so the alerts that matter get through:

- A confirmed reset is no longer dropped. Providers refresh at the same time, and only one alert was allowed per refresh, so an unrelated warning could silently swallow the only weekly reset notification you would get all week.
- Scheduled 5-hour session resets no longer notify. They come round several times a day and are exactly what you already expect. Weekly and monthly resets always notify, and unexpected ones (early, partial, banked) still notify at any cadence.

Portable builds are a deliberate exception. Windows only keeps notifications for an app claimed by a Start Menu shortcut, and Ceiling will not add one to a machine where you chose portable. Portable alerts appear as banners without notification-center history.

### Simplified Chinese

中文 is now a switchable interface language under Settings > General. Anything not yet translated falls back to English.

### Sign in from inside Ceiling

Claude and Codex now have in-app login flows next to the existing Copilot device flow, with progress shown as it happens. Providers without an in-app flow tell you what credentials they need instead of quietly opening a dashboard.

## Also fixed

- Estimated API value no longer under-reports when you have more than one Codex or Claude seat. Secondary accounts added under Accounts were never scanned.
- The one-number strip could sit on a maxed-out Cursor API lane while Auto still had room. It now shows the hottest lane that still has capacity.

## Installers

- **Ceiling-1.5.17-Setup.exe** - standard installer
- **Ceiling-1.5.17-portable.exe** - portable
- **Ceiling-1.5.17-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.16...v1.5.17
