## Highlights

### Claude accounts no longer report themselves as maxed out

An account sitting at 1% could show its 5-hour session as **100% used**, and raise an exhausted alert for it.

Anthropic reports usage as either whole percentages or fractions of the limit, so a lone `1` means 1% in one and 100% in the other. Ceiling guessed wrong whenever every window was still at 0 or 1, which is exactly what a lightly used account looks like.

### Notifications stay in the notification center

1.5.17 was meant to fix Windows throwing Ceiling's alerts away. On a clean install it did nothing.

Windows only keeps a notification for an app it can identify, and it reads that identity from a Start Menu shortcut. The installer was not setting it, so a fresh install still lost every alert seconds after it appeared. It only looked fixed on machines that had an older shortcut left behind.

**If you are on 1.5.17, install this one to pick up the corrected shortcut.**

### Updates no longer loop

Running Ceiling from somewhere other than the installed folder, such as a local build or an install from older packaging, put updates in a circle. The installer succeeded elsewhere, the copy you were running stayed on the old version, and the same update came back on the next check with nothing on screen explaining why.

Ceiling now spots that and names both copies instead of quietly repeating itself. Cached installers are cleaned up once superseded, too. They were kept forever, and one machine had 305 MB of them.

### Hide Personal Info hides personal info

The setting only ever reached the Accounts list. The Overview, taskbar flyout, plan cards, activity timeline and provider detail all carried on showing the full address.

They all mask now, including account labels, which default to your address and were a second copy of it. The domain stays visible so you can still tell your own accounts apart.

## Also fixed

- Switching between accounts on the Providers page works. Clicking the second account did nothing, because the chosen account was never sent to the backend.
- Enabled providers sort to the top of the Providers list, with the rest by name. A provider you had configured could otherwise sit far down among ones you do not use.

## Installers

- **Ceiling-1.5.19-Setup.exe** - standard installer
- **Ceiling-1.5.19-portable.exe** - portable
- **Ceiling-1.5.19-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That needs a Start Menu shortcut, and Ceiling will not add one to a machine where you chose a portable build.

1.5.18 was pulled before it shipped. Everything that was in it is here.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.17...v1.5.19
