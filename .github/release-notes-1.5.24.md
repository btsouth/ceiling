## Highlights

This release supersedes 1.5.23, which was never published — everything from it is included here.

### Your usage bars now show where you *should* be

Ceiling could already work out whether you were on course to run out before a window reset. It just wasn't on the thing you actually look at.

Every weekly and monthly bar — in the Overview and in a provider's detail view — now carries a marker for where usage should be at this point in the window. One rule, everywhere: **the marker is where the bar's edge should be right now.**

- Edge sitting at the marker → you're on pace.
- Edge past it → you're ahead of budget, and the overspend fills in as a striped band, so you can see *how far*, not just *which side*.
- Bars set to show remaining capacity mirror the marker, so it means the same thing either way.

It's worked out from elapsed time against the window's own length, which means it needs nothing from the provider and shows up on every long window at once, instead of only the single window a pace prediction was calculated for.

Five-hour session bars are deliberately left plain. Nobody spends a session evenly, so a marker there would drift across the bar all afternoon and tell you nothing.

### Predictive pace warnings are back, if you want them

Under **Settings → Notifications**, "Predictive Pace Warnings" alerts you when a window is on course to be exhausted before it resets. It's off by default.

This existed but was unreachable: switched off on every launch with no way to enable it, and limited to Claude and Codex even then. Any provider that reports a reset can raise one now, and warnings name the window by its real cadence instead of calling a monthly quota a "session".

### Cursor on-demand spend is visible, in dollars

If you hit 100% of your Cursor plan, on-demand is where the real money goes, and it was easy to miss entirely. It could vanish on some account shapes, and anyone running on-demand *without* a spend cap saw nothing at all, because a meter needs a cap to draw a percentage against.

The **On-demand** row now appears in a provider's detail view with its spend beside it, uncapped spend is reported as a plain figure rather than dropped, and it takes the cost slot ahead of plan usage. On-demand is the only Cursor lane that bills real money, so it should be the number you see.

### A Cursor meter that could only ever say 0% is gone

The Cursor card carried a "Promotional" bar pinned at 0% by its own arithmetic, and a badge claiming the bonus expired at the end of the billing cycle when Cursor credits expire on their own schedule. Both numbers were wrong and neither can be derived from what the Cursor API reports, so the lane has been removed rather than guessed at.

### Also fixed

- Claude's Opus model pool can now raise usage alerts alongside the session and weekly windows. It could previously sit at 99% in silence.
- A weekly window promoted into the primary slot is no longer reported as a session.
- Devin's usage percentages are resolved from every reported window at once, so a response mixing `0.4` and `32` reads the `0.4` as 0.4% rather than rescaling it to 40%.
- Release binaries are built with link-time optimization and symbol stripping for the first time. The build profile had been sitting where cargo ignored it, so shipped builds were larger and slower than intended.

## Installers

- **Ceiling-1.5.24-Setup.exe** - standard installer
- **Ceiling-1.5.24-portable.exe** - portable
- **Ceiling-1.5.24-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That requires the Start Menu shortcut installed by the standard or Store build.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.22...v1.5.24
