## Highlights

### Antigravity quotas match Settings

Ceiling was reading each model's leftover fraction from the language server. Antigravity Settings shows shared pools instead: Gemini vs Claude/GPT, with weekly and five-hour limits.

1.5.15 uses that same group summary (`RetrieveUserQuotaSummary`). If Settings showed real weekly/5h usage while Ceiling listed every model at 0%, this is the fix. Leave Antigravity open and signed in, then refresh.

Thanks @pupontech for the side-by-side report (#163).

### Claude: capacity vs Charts

Live capacity (Accounts, tray meters) needs a Claude CLI sign-in. Charts still read local session logs under `~/.claude` without one.

If Claude was Error with "sign-in was not found" while Charts still showed dollars, that split is real. Errors now:

- keep multi-source Auto detail instead of collapsing to OAuth-only
- point at running `claude` once in a terminal
- say Charts can still show local spend without live capacity

Thanks @insanowsky for the screenshots (#165).

### Custom range on Estimated API value

On Charts, Estimated API value adds **Custom** next to Today / Yesterday / 30 days. Pick inclusive local From/To dates (up to 366 days) for every-other-week use or a single month.

Thanks @insanowsky for the request (#164).

## Installers

- **Ceiling-1.5.15-Setup.exe** - standard installer
- **Ceiling-1.5.15-portable.exe** - portable
- **Ceiling-1.5.15-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

---

Patch on **1.5.14**.

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.14...v1.5.15
