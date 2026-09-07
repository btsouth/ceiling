## Since 1.5.6

### Taskbar multi-account and strip polish

- Pin which Codex or Claude seat drives each strip tile (Settings → Taskbar). The strip no longer always picks the hottest account when you care about a specific seat.
- The taskbar flyout marks that account **On strip**, lists it first within the provider, and keeps the chip quieter than banked-resets so hierarchy stays clear.
- Native strip tiles show the window label only (Weekly / 5h) plus optional reset. Long account names no longer collide with the next provider; seat identity lives in the flyout via **On strip** and the account line.
- The **On strip** flyout row keeps the same left inset as every other provider (tint only — no margin/padding shift).

### Charts trust and efficiency

- Reset-window and calendar period cards show **N% of tokens priced** when unpriced models shrink the dollar total. Fully priced windows stay quiet.
- Grok Charts pick up multi-project and partial session usage that previously dropped off project rollups.
- New **Quota run efficiency** card: tokens per 1% used, cache-read share for that run, projected tokens at 100% (once peak is high enough), and run-over-run change vs the previous complete run. Local observation only, not a published allowance. The card fills as resets finalize with local token samples (run-over-run needs two complete runs on the same window).

## Installers

- **Ceiling-1.5.9-Setup.exe** - standard installer
- **Ceiling-1.5.9-portable.exe** - portable
- **Ceiling-1.5.9-Store-Setup.exe** - Microsoft Store package (WebView2 bundled)

---

Covers everything since the last published release (**1.5.6**), including draft tags that were never published (1.5.7–1.5.8).

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.6...v1.5.9
