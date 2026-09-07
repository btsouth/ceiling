## Taskbar multi-account

Pin which Codex or Claude seat drives each strip tile (Settings → Taskbar). The flyout marks that account **On strip** and lists it first. When a provider has more than one account, the native strip detail line shows a short account tag so you can tell seats apart at a glance.

## Charts trust and efficiency

- Reset-window and calendar period cards show **N% of tokens priced** when unpriced models shrink the dollar total. Fully priced windows stay quiet.
- Grok Charts pick up multi-project and partial session usage that previously dropped off project rollups.
- New **Quota run efficiency** card: tokens per 1% used, cache-read share for that run, projected tokens at 100% (once peak is high enough), and run-over-run change vs the previous complete run. Local observation only, not a published allowance. The card fills as resets finalize with local token samples (run-over-run needs two complete runs on the same window).

## Installers

- **Ceiling-1.5.7-Setup.exe** – standard installer
- **Ceiling-1.5.7-portable.exe** – portable
- **Ceiling-1.5.7-Store-Setup.exe** – Microsoft Store package (WebView2 bundled)

---

**Full Changelog**: https://github.com/btsouth/ceiling/compare/v1.5.6...v1.5.7
