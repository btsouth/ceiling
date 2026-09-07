## Highlights

### Ceiling tells you when you're going to run out

Ceiling already predicted whether a quota would outlast its window. It just never said so anywhere you'd see it: the expected-vs-actual breakdown is too tall for the tray, so it was hidden there, which is exactly where people look.

The tray now carries a verdict. It names the outcome (on track, ahead of pace, plenty left, or running out early), says what that means for you, and draws one slim bar with a tick showing where your usage *should* be by this point in the window. If you're burning through it, it tells you roughly how long you have left. The detailed breakdown is still there in the main window.

### Predictive pace warnings are back, if you want them

There is a warning that fires when a window is on course to be exhausted before it resets. It had been switched off on every launch with no way to turn it on, and even then it only ever applied to Claude and Codex.

It is available again under **Settings > Notifications**, off by default. Any provider that reports a reset can raise one, and the warning now names the window by its real cadence instead of calling a monthly quota a "session".

### Cursor on-demand spend is visible in dollars

If you hit 100% of your Cursor plan, on-demand is where the real money goes, and it was easy to miss. It could vanish entirely on some account shapes, and anyone running on-demand without a spend cap saw nothing at all, because a meter needs a cap to draw a percentage against.

On-demand now shows its dollars beside its bar, uncapped spend is reported as a plain figure rather than dropped, and it takes the cost slot ahead of plan usage. Only on-demand bills real money on Cursor, so it should be the number you see.

### A Cursor meter that could only ever say 0% is gone

The Cursor card carried a "Promotional" bar that was pinned at 0% by its own arithmetic, and a badge claiming the bonus expired at the end of the billing cycle when Cursor credits expire on their own schedule. Both numbers were wrong, and neither can be worked out from what the Cursor API actually reports, so the lane has been removed rather than guessed at.

## Installers

- **Ceiling-1.5.23-Setup.exe** - standard installer
- **Ceiling-1.5.23-portable.exe** - portable
- **Ceiling-1.5.23-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That requires the Start Menu shortcut installed by the standard or Store build.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.22...v1.5.23
