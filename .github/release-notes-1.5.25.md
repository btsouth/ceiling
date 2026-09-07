## Highlights

### Claude's taskbar tile now shows the capacity that governs your account

Claude's model-specific limits, such as "Fable only," could take over the single taskbar lane when they reached 100%. That hid Session and Weekly capacity even though switching models still let you work. The native taskbar tile and floating bar now reserve that lane for the real account pools; model limits remain visible everywhere that lists all windows.

### Credential and settings updates no longer risk unrelated data

- An unreadable API-key or manual-cookie store is left untouched instead of being treated as empty and overwritten.
- Unknown provider settings and token accounts survive saves and downgrades instead of resetting preferences or disappearing.
- Removing one token account preserves the account you actually selected.

### Custom Codex endpoints are checked by their real host

Plaintext custom Codex URLs are restricted to genuine loopback hosts. URL shapes that merely contain `localhost` while pointing at a remote host are rejected, preventing the Codex access token from being sent there. Legitimate local proxies continue to work.

### Also fixed

- Countdown formatting no longer loses nearly an hour around hour boundaries.
- Relative reset timers hold at one minute instead of displaying "Resets in 0m."

## Installers

- **Ceiling-1.5.25-Setup.exe** - standard installer
- **Ceiling-1.5.25-portable.exe** - portable
- **Ceiling-1.5.25-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

Portable builds show alerts as banners but do not keep them in the notification center. That requires the Start Menu shortcut installed by the standard or Store build.

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.24...v1.5.25
