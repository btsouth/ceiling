# Repository Guidelines

## Current Project State
- This branch launches the Tauri desktop shell by default (`apps/desktop-tauri/src-tauri`), while
  `rust/` remains the shared backend/domain crate and standalone CLI.
- Many files in `docs/` and some workflows reference the upstream macOS/Swift project. Treat those as historical or
  upstream-sync material unless the task is explicitly about upstream parity.
- When repo docs conflict, trust the active Tauri desktop sources in `apps/desktop-tauri` plus the shared Rust sources
  in `rust/src`.

## Project Structure & Modules
- `apps/desktop-tauri/`: Tauri desktop shell (default UI). React frontend in `apps/desktop-tauri/src/`,
  Rust backend + tray bridge in `apps/desktop-tauri/src-tauri/src/`.
- `rust/src`: Shared backend crate + CLI (`codexbar` binary). Houses providers, settings, login,
  status, sound, shortcuts, browser cookie extraction, and the shared tray-icon renderer.
- `rust/src/providers`: Provider-specific fetch/parsing/auth logic. Keep provider boundaries clean.
- `rust/src/tray` (shared): `icon.rs` + `render.rs` — pixel-level tray-icon rendering used by the Tauri shell.
- `rust/src/browser`: Browser detection + cookie extraction for Windows.
- `rust/src/core`: Shared provider-construction (`instantiate_provider`) and provider IDs.
- `rust/assets`, `rust/icons`, `rust/gen`, `rust/wix`: UI assets, generated schemas, installer packaging.
- `docs`: Mixed documentation (Windows port docs plus upstream/macOS references). Update only the relevant docs.

## Build, Test, Run
- Default desktop work runs from the repo root; `cd rust` is for backend/CLI-only tasks.
- Build the desktop shell (preferred): `cd apps/desktop-tauri && npm run tauri:build` (or `tauri:build:debug`).
  Raw `cargo build --release` on the Tauri crate produces an exe that still points at the dev URL.
- Build the CLI: `cargo build -p codexbar`.
- Test: `cargo test --manifest-path rust/Cargo.toml` and
  `cargo test --manifest-path apps/desktop-tauri/src-tauri/Cargo.toml`.
- Run CLI locally: `cargo run -p codexbar -- --help`, `cargo run -p codexbar -- usage -p claude`,
  `cargo run -p codexbar -- cost`. The CLI no longer launches a GUI when run with no subcommand.
- Run the desktop shell through Tauri's build/dev flow: `.\dev.ps1`, `./dev.sh`, or
  `cd apps/desktop-tauri && npm run tauri:dev`.
- For Rust changes, format/lint the affected crate before handoff: `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`
  with the affected manifest. Frontend-only or documentation changes do not require Rust checks.
- There is no active root-level `Scripts/` build pipeline in this port. Do not rely on legacy `Scripts/*.sh` commands.

## Coding Style & Naming
- Prefer small, typed structs/enums and focused modules; keep changes local.
- Keep provider-specific logic inside the provider module instead of adding cross-provider branching.
- Preserve clear error handling and user-facing diagnostics (`anyhow`/`thiserror` + friendly messages where applicable).
- Use `tracing` for diagnostics; do not log raw secrets, cookies, or tokens.
- Reuse existing dependencies and tooling. Add them only when needed for the
  authorized change and explain why; routine implementation choices do not
  require separate confirmation.

## Testing Guidelines
- Add or extend focused Rust tests near the changed module (`#[cfg(test)]` unit tests are common in this repo).
- For parser/fetcher changes, add deterministic samples/fixtures where practical.
- Run focused Rust tests for backend changes and the relevant frontend checks for frontend changes. Run broader required checks once before delivery; explain any unverified paths.
- For desktop/tray changes, validate through the Tauri build/dev flow in an isolated desktop or the known Windows test route. Follow the desktop isolation skill; report native behavior that remains unverified.

## Commit & PR Guidelines
- Use short imperative commit messages (for example: `Fix Claude CLI parser`, `Improve cookie import errors`).
- Keep commits scoped to one change.
- In PRs/patches, include:
  - Summary of behavior changes
  - Commands run (`cargo test`, `cargo fmt`, etc.)
  - Screenshots/GIFs for UI changes (Windows)
  - Linked issue/reference when relevant

## Release & Winget Notes
- Treat Winget updates as a normal release step after GitHub release artifacts are stable.
- Ceiling's Winget identity is `tsouth89.Ceiling`, with manifest folders under
  `microsoft/winget-pkgs/manifests/t/tsouth89/Ceiling/`. Do not confuse it with `Finesssee.Win-CodexBar`,
  which tracks a different project (`nesszer/Win-CodexBar`) despite the shared lineage.
- Winget does not track "latest" GitHub releases; every version needs its own immutable manifest folder,
  for example `manifests/t/tsouth89/Ceiling/1.5.37/`.
- For routine version bumps, copy the previous approved manifest folder and change only version-specific fields:
  `PackageVersion`, `InstallerUrl`, `InstallerSha256`, `DisplayName`, `DisplayVersion`, `ReleaseDate`,
  `ReleaseNotes`, and `ReleaseNotesUrl`.
- Keep stable package identity and installer behavior unchanged unless there is a real packaging reason:
  `PackageIdentifier`, `InstallerType`, `Scope`, `ProductCode`, `Publisher`, package URLs, and silent install behavior.
- Before opening a Winget PR, verify the release installer URL resolves and recompute the SHA-256 from the downloaded
  asset (the release's `.sha256` sidecar is a convenient cross-check). On Windows, run `winget validate` when available;
  from Linux note the skip in the PR body.
- Package history: the package was first approved as `microsoft/winget-pkgs#411757` (1.5.22), and recent updates
  shipped as `#414311` (1.5.25), `#416061` (1.5.29), and `#423050` (1.5.36). Expect Microsoft validation/review lag
  on every update.

## Agent Notes
- The default desktop app is the Tauri shell in `apps/desktop-tauri/`. The Rust crate owns shared backend logic
  and the CLI.
- New provider construction goes through `codexbar::core::instantiate_provider` — do not duplicate provider
  factories in shells or commands.
- Keep provider data siloed: never show identity/plan/email fields from provider A in provider B UI.
- Claude CLI output is user-configurable; do not depend on a customizable status line for usage parsing.
- Cookie import UX uses explicit browser selection in Preferences. Do not assume Chrome-only in general UI flows.
- Be conservative with secret handling (manual cookies, API keys, token accounts); use existing redaction/storage helpers.
- Prefer Windows-native validation for tray/DPAPI/browser-cookie behavior; WSL/Linux can be insufficient for those paths.

## Worktrees
- Use the assigned devbox task worktree for heavy work; otherwise follow the active machine policy. Reuse it through fixes.
- Remove only this task's clean, landed worktree with `git worktree remove`. If Windows leaves an empty directory, confirm it is empty before removing it; do not blindly force-delete it.
- Squash merges do not preserve branch ancestry. Check the branch's merged PR with the account wrapper and `pr list --state merged --head <branch> --json number,headRefName,headRefOid,title`. Compare `headRefOid` with `git rev-parse HEAD` before removing a worktree; a reused branch name can match an older merged PR.
