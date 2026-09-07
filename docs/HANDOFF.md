# Ceiling handoff

Use this as the implementation starting point. When it disagrees with the app, trust the active Tauri sources in `apps/desktop-tauri` and shared Rust sources in `rust/src`.

## Current state

- Repository: `C:\projects\personal\ceiling`
- Trunk: protected `main`; create short-lived feature branches for new work. PRs squash-merge.
- Public release: [Ceiling 1.5.11](https://github.com/btsouth/ceiling/releases/tag/v1.5.11). Package / next draft: **1.5.12** (Antigravity detection, duplicate-account prune, strip hides not-ready providers).
- Website: [ceiling.win](https://ceiling.win).
- Tracker: [Ceiling in Linear](https://linear.app/southforge-ai/project/ceiling-6615aa2c9e6b).

Ceiling is a local-first Windows companion for AI-subscription capacity. First-class providers: Codex/ChatGPT, Claude, Cursor, **Grok**, Gemini, and GitHub Copilot. The app is built on the mature Win-CodexBar Tauri/Rust foundation while keeping Ceiling's Windows UI, release identity, and product behavior distinct.

## Implemented product contract

- Separate provider-reported windows for session, weekly, monthly, model-specific, and named extra capacity.
- Explicit inactive/not-enforced states instead of fabricated zero-use or unlimited meters.
- Overview, Activity, Accounts, and Charts dashboard surfaces.
- Concurrent multi-account for Codex/Claude (and other multi-seat providers): cache and UI keyed by `(provider, account)`, not switch-active-only.
- Compact taskbar-adjacent floating bar **and** native taskbar strip widget, plus tray/dashboard views.
- Taskbar strip can pin which account drives a multi-account provider tile; flyout marks that seat **On strip**.
- Grok: SuperGrok weekly pool meter, plan detection from `~/.grok/auth.json` / cookies, local session analytics on Charts (tokens, cache mix, reasoning/effort, projects, API-equivalent $ from `costUsdTicks`).
- Persistent local quota history and processed-token summaries from local logs (Codex/Claude dollars; Grok API-equivalent $ from session ticks when present; unpriced/partial rows stay excluded from $ totals with coverage disclosure).
- Credential discovery and setup paths for the focused providers.
- A persistent capacity-event observer keyed by provider, account, source, and semantic window identity (including resets that happened while closed).
- Startup re-baselining, confirmation for surprising changes, alert de-duplication, and a toast burst circuit breaker.
- Windows notifications only for confirmed scheduled or surprise resets, plus opt-in high-usage pace warnings.
- First-party website analytics and a private `/admin` dashboard backed by PostHog and GitHub metrics.
- Microsoft Store offline installer artifact on signed releases.

## Product rules

1. Provider-reported meters are authoritative. Local logs are contextual activity data, never a substitute for a subscription limit.
2. Keep providers, accounts, sources, and named windows isolated from one another.
3. Treat an absent known window as a state: tracked, not currently enforced, or unavailable.
4. Never infer entitlements, remaining allowance, or subscription spend from plan names or local token totals.
5. Local log token/cost totals are machine-wide for a provider unless a source is truly account-scoped; Charts show one section per provider and disclose that limitation.
6. Re-baseline after startup. Do not replay changes that happened while Ceiling was closed as if they just occurred (away-reset path may announce them with correct wording).
7. Require fresh, consistent evidence for surprising changes. Notify only for resets and intentional high-usage warnings.
8. Prefer no notification over an ambiguous or duplicate notification.

## Next coherent work (see Linear)

High-signal open items (verify status in Linear before starting):

1. Remaining charts trust polish and spend features (quota-run snapshots, efficiency, spend anomaly).
2. Cursor trustworthy local spend path (Grok token charts largely shipped in 1.5.6).
3. Active-provider floatbar modes, themes, localization leftovers, in-app login beyond Copilot.
4. Deliberate multi-monitor floatbar (one managed bar per selected display) if still desired.

Do not cut a release solely for small polish merges unless asked.

## Relevant implementation areas

- `apps/desktop-tauri/src-tauri/src/capacity_events.rs` — reset/capacity observation and confirmation.
- `apps/desktop-tauri/src-tauri/src/commands/providers.rs` — refresh integration and notification eligibility.
- `apps/desktop-tauri/src-tauri/src/commands/chart.rs` — local usage summary, API-value card, reset-window coverage.
- `apps/desktop-tauri/src-tauri/src/usage_history.rs` — local quota history.
- `apps/desktop-tauri/src-tauri/src/taskbar_widget.rs` — native strip (account pin, multi-account tags).
- `apps/desktop-tauri/src-tauri/src/floatbar/` — Windows floating-bar lifecycle and positioning.
- `apps/desktop-tauri/src/surfaces/` — dashboard surfaces (Charts, flyout, settings).
- `apps/desktop-tauri/src/lib/providerRow.ts` — multi-account row keys and strip/flyout selection helpers.
- `rust/src/providers/` — provider-specific fetch, auth, and parsing (including `grok/`).
- `rust/src/cost_scanner.rs`, `rust/src/grok_costs.rs` — local log scans.
- `rust/src/notifications.rs` — Windows notification manager and burst protection.
- `worker.mjs` and `site/` — public site, analytics capture, and private dashboard.

## Verification

From the repository root:

```powershell
node --test worker.test.mjs
pnpm --dir apps/desktop-tauri install --frozen-lockfile
pnpm --dir apps/desktop-tauri test
pnpm --dir apps/desktop-tauri run build
cargo fmt --all --check
cargo test --manifest-path rust/Cargo.toml
cargo test --manifest-path apps/desktop-tauri/src-tauri/Cargo.toml
cargo clippy --manifest-path rust/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path apps/desktop-tauri/src-tauri/Cargo.toml --all-targets -- -D warnings
```

Use `pnpm --dir apps/desktop-tauri tauri:dev` (or `.\dev.ps1`) for Windows behavior. Preserve the MIT license and upstream attribution, and do not rename the internal `codexbar` crate as part of feature work.
