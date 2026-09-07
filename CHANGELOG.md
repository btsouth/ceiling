# Changelog

## [Ceiling] Unreleased

## [Ceiling] 1.5.36 - 2026-08-23

This release closes out the current maintenance queue with safer local state, more accurate usage and cost reporting, and better behavior across the desktop, CLI, and MCP server. It also adds multi-account CLI usage, restores detached Settings window geometry, and strengthens accessibility regression coverage.

The Windows server token now rotates after unsafe exposure. Concurrent and corrupt settings files fail safely, startup ownership is respected, refresh intervals work as configured, and account or quota views no longer substitute misleading data.

### Security
- **A leaked `serve.token` is rotated on Windows, not just Unix.** After SBS-953, a world-readable token was replaced on Unix, but Windows still tightened the DACL and reused the same secret. The ACL is now inspected before tightening; if anyone other than the current user, SYSTEM, or Administrators can read the file, the token is replaced. Closes SBS-1043.

### Fixed
- **Timed-out Kiro, Augment, and Vertex CLI fetches no longer leave orphaned children.** Those providers spawned `tokio::process::Command` without `kill_on_drop`, so a desktop refresh timeout dropped the `Child` and the CLI kept running. Fetch-path commands now kill on cancel; Augment's inner 15s deadline also kill+waits like `command_runner`. Closes SBS-1078.
- **Frontend tests now catch accessibility regressions automatically.** A shared axe assertion checks representative quota cards, mini charts, and update banners in the existing Frontend CI job. Color contrast remains outside jsdom coverage because it requires a rendered browser. Fixes #222.
- **`usage --all-accounts` now fetches every configured Codex and Claude account.** Account fetches run with bounded concurrency, preserve configured order, and report failures independently. Text and JSON identify each configured account while the default output remains unchanged. Fixes #274.
- **The detached Settings window now reopens where you left it.** Its saved size and position are restored and clamped on screen instead of being overwritten by a second frontend resize on every open. Closes #275.
- **`serve --refresh-interval` now controls response caching.** Successful usage and cost responses are cached separately by provider selection for the requested TTL. A zero interval disables caching, and provider failures are retried on the next request. Fixes #273.
- **Codex latest-session cost follows transcript time.** Copying or touching an older rollout no longer makes it replace a newer session in local cost summaries. Fixes #271.
- **A corrupt `window_geometry.json` no longer wipes other windows' saved positions.** SBS-1024 locked the persist so two surfaces could not drop each other's keys, but a file that would not read or parse still loaded as empty defaults, and the next save replaced the whole file with only the window that just moved. Persist now refuses that write — the same fail-closed rule API keys already use — instead of rewriting siblings to an empty store. Closes SBS-1041.
- **Loading settings no longer rewrites another install's start-at-login command.** Every `Settings::load` repaired `HKCU\...\Run\Ceiling` whenever the value was not the quoted path of this process. A portable CLI or a second tree therefore replaced the installed desktop's startup entry, and any extra arguments were stripped. Repair now runs only when this process owns that entry — the same intended exe, or a stale `codexbar-cli.exe` / `codexbar-desktop.exe` sibling in the same directory — and leaves custom arguments and other trees alone. Closes SBS-1053.
- **MCP `get_status` no longer hides an exhausted Weekly behind a healthy session.** Top-level `remaining_percent` copied only `usage.primary`, so a Claude/Codex 5-hour window with room made the advertised cap-check sink look fine while Weekly was already at 100%. It now uses the same constraining-window ranking as the desktop strip across primary, secondary, and tertiary (exhausted first, then highest used %). Closes SBS-1055.
- **Remembered window positions no longer drop a sibling when two surfaces save at once.** `window_geometry.json` was updated with an unlocked read-modify-write, so moving Settings while the float bar or Pop Out also wrote could replace the file with a snapshot that had never seen the other key. Geometry persist now holds the same cross-process state lock as settings and credentials. Closes SBS-1024.
- **`codexbar` with no subcommand now runs `usage`.** CLI.md and `--help` already called usage the default command, but a bare `codexbar` printed an error asking for an explicit subcommand. It now does what those docs said. Closes SBS-1026.
- **A corrupt `settings.json` is no longer renamed to `.bak` without the state lock.** SBS-954 moved an unparseable file aside so the next save could not overwrite it, but `Settings::load` did that rename before taking the lock. A concurrent `try_update` that had already written a good repair could then have that repair moved to `settings.json.bak`. The unlocked load now only parses; if the file is corrupt (or still carries embedded credentials) it takes the lock, re-reads, and quarantines only then. Closes SBS-1029.
- **MCP `get_status` no longer runs a 30-day local cost scan to fill today's spend.** The tool is advertised as a cheap "am I about to hit my cap?" check, but it called the same 30-day Codex/Claude/Grok walk as `get_spend` just to read `today`. It now scans one inclusive local day for `today_spend`. Use `get_spend` for 7/30-day totals. Closes SBS-1033.
- **MCP no longer labels billed period spend as session cost.** `get_usage` and `get_status` stuffed provider `CostSnapshot.used` — monthly credits, last-30-days, extra usage — into `session_cost_usd`, so an agent reading STATE treated a period total as this conversation. The field is now `period_cost_usd` plus `cost_period` (the provider's period label). `get_spend` / `today_spend` stay local estimated log spend. Closes SBS-1031.

## [Ceiling] 1.5.35 - 2026-08-22

Charts stops showing numbers it cannot stand behind. A month whose models have no published prices painted as an empty one, the busiest day on the activity heatmap could share a shade with the quietest, and a Codex session carried whatever calendar day it was first parsed on, so a DST change or a trip moved usage onto the wrong day. Two cards scanning at once could overwrite each other's index, and a price refresh landing mid-scan stamped old dollars as current for every card already in the file.

The rest is state a surface reported but could not actually reach. A settings or credentials write could run unserialized on a filesystem that cannot `flock`, Install and Restart could check a staged installer against a different release's digest, and dismissing an update mid-download un-dismissed itself on the next progress chunk. A machine reporting no home directory no longer reads Gemini credentials from — or writes refreshed Google tokens into — whatever directory it happened to start in.

Ceiling also asks for a GitHub star now. At most twice, ever, and only after a provider has actually reported a reading.

### Added
- **Ceiling asks for a GitHub star, at most twice ever.** A card in the bottom-right of the dashboard, and the only thing Ceiling has ever asked of anyone using it. The first ask waits until a provider has actually reported a reading and that reading has been on screen for twenty seconds, so it lands after the app has done something useful rather than during setup, when it would be asking to be paid before the work. The second, if the first went unanswered, waits for a later version to have shipped and for a week to have passed since the first, because a version bump the day after is still two asks in one week. Clicking through to GitHub ends it permanently; so does reaching the second ask. Later and the close button mean the same thing and never count as interest. It is not a Windows notification, it takes no focus and traps none, Escape closes it, and the only network call involved is opening the repository in your browser when you ask for it.

### Security
- **Gemini treats a missing home directory as not logged in.** With no `HOME` or `USERPROFILE`, Ceiling used the working directory as `~/.gemini`, so a checkout fixture could be read and then written back after a refresh. Both the credential read and the credential write now refuse that fallback, matching `client_config.json`.

### Fixed
- **Grok project names read from a Windows path no longer come back as the whole path.** A session's `cwd` is written by whichever machine ran Grok, so a Windows path reaches Ceiling on Linux and inside WSL. `Path::file_name` only treats `\` as a separator on Windows, so everywhere else a `cwd` of `C:\projects\personal\ceiling` was reported as a project literally named `C:\projects\personal\ceiling`, and the URL-encoded session folder split the same way. Both now split on either separator on every host. A Codex sessions directory joined onto a Windows root also keeps `\` instead of picking up the host's separator. Windows behaviour is unchanged.
- **A corrupted Claude de-duplication key no longer silently inflates Charts and Estimated API value.** The usage index treated invalid UTF-8 in a present `dedup_key` as "this record has no key", so the same transcript event could be counted twice if its counterpart still had one. Invalid UTF-8 now rejects the whole index the way a required string already does, and the next scan rebuilds it from the transcripts. A key that was never written is still encoded as the length sentinel, not as a failed decode.
- **A filesystem that cannot flock no longer writes settings unserialized.** The state-write lock treated every `flock` errno other than contention as "no lock needed" and continued, so an NFS/FUSE/SMB home without `lockd` let the tray and the CLI replace `api_keys.json` over each other. Only `ENOTSUP` — the filesystem saying it does not implement locking at all — now falls back, read from the errno rather than from how a given Rust version happens to decode it, and the fallback is an exclusive-create sibling that a second writer cannot take. `ENOLCK` is no longer treated as a broken filesystem: it also means the kernel lock table is full or `lockd` failed for that one call, while another process still holds a real flock, so falling back to the sibling would have let both writers through. It fails the write instead. An unknown errno already did. Because the sibling has no kernel-backed release, a leftover from a killed process still has to age out, but the age it has to reach is now two minutes rather than the ten seconds an acquirer waits — a holder doing slow work can no longer be outlasted and have its lock taken, and a lock file stamped in the future by a skewed server clock reads as held rather than as expired. A holder unlinks the sibling on release only while it is still the same file it created, so a takeover cannot cascade into deleting a third writer's lock. A `state-write.lock` the current user cannot open — left by a `sudo` run, or by another account — now fails the write and names the path to remove. It used to be unlinked and recreated, which put the running holder and the new one on two different inodes and let both writes proceed. A directory in the lock path still fails the write. Releasing that sibling also hands the lock cleanly to the next writer on Windows, which reports a name whose deletion has begun as access-denied rather than as already-existing; a waiter that arrived in that window used to get a hard failure instead of the lock a moment later.
- **The 1.5.33 notes no longer claim the incident badge is Ceiling's only non-provider request.** That sentence was false: models.dev (prices) and GitHub (update checks) already run without that opt-in. The badge is still off by default. The replacement makes no exclusive claim at all: the badge adds each enabled provider's public status page to the hosts Ceiling talks to, on top of the usage requests those providers already receive, while models.dev and GitHub are fixed hosts that run either way. A copy test bans the exclusive phrasings so neither version can come back.
- **The activity heatmap no longer paints the busiest day the same shade as the quietest.** Quartile cuts that collapsed to one value still held every active cell at mid-scale, including a day orders of magnitude above the rest, and with four or fewer active cells the 75th-percentile cut sat on the maximum so the legend's top swatch never appeared. An outlier now reaches the darkest step, a genuinely flat month stays mid-scale, and the busiest cell of a small set reaches the top of the ramp. The top swatch is decided by the busiest cell rather than by the 75th-percentile cut, which was wrong in both directions: when the upper half tied, the busiest cell fell into the mid band, and with four distinct cells the cut sat low enough that the second-busiest matched the busiest.
- **A single light day no longer becomes the spend-anomaly baseline.** After a week away, one $1.50 session was "the recent daily median", so the first ordinary $12 day back toasted as an 8x spike and marked the day sent. A real runaway later that evening then said nothing. The detector now needs three floor-clearing days before it will compare; fewer is the same as a zero baseline, which already stays quiet. Closes SBS-965.
- **Install and Restart no longer checks the downloaded installer against a different release.** Clicking Download and Check for updates at the same moment could let the check finish second and store a newer release digest next to the installer that had just completed. Install then failed closed and discarded a valid download. Download now takes the updater lock for the whole start, a check that lost the slot does not overwrite an in-flight download, and apply verifies the digest captured when that download began. Dismissing an update while its download is still running now sticks: a later progress chunk used to write the Downloading state back, which let the finishing download store Ready and bring Install & Restart back after the banner had been dismissed. That discarded download's installer is deleted rather than left in the staging directory.
- **Two Charts scans no longer wipe each other's usage index.** The API-value card and the heatmap can finish at the same time, and each commit encoded its snapshot then released the lock before writing the file. The slower writer could put the older snapshot back on disk, so files the faster scan had just indexed were gone after a restart and had to be parsed again. The lock now covers the write, so the file on disk is always the latest encode. In-memory totals were already correct.
- **A light user whose every day is under $1 can now receive a spend-anomaly alert.** Days under a dollar still do not build the recent-median baseline — a five-cent leftover next to a real day would otherwise manufacture a false spike. When that leaves no median at all, the detector used to go silent, so a $0.60/day user never heard about a $180 loop. It now falls back to a $20 absolute trigger (20x the $1 floor, the largest spike factor the setting accepts) so a runaway still toasts and an ordinary first day does not. The budget alert is a separate opt-in and is no longer this feature's only answer for that user. The trigger is compared at cent precision, so a day worth exactly $20.00 cannot be missed because a sum of floating-point estimates landed a fraction under it while the toast still printed "$20.00".
- **The activity heatmap follows the UI language.** The rest of Charts already translated; this card had been written after locale landed and never joined. Titles, empty states, axis labels, screen-reader text, and dollar/token formatting now go through the same bundles as the rest of the app, so a Chinese language setting no longer leaves this card in English. A language without its own bundle formats in English rather than picking up the machine's locale, so English copy no longer appears beside another language's dates and separators. Switching language re-renders the card in place instead of re-running the transcript scan behind a spinner, and a failed scan is reported in the selected language rather than passing the backend's English sentence through.
- **The first-run cleanup of old temp-folder logs no longer repeats forever on a shared `/tmp`.** Earlier versions wrote `codexbar_launch_*.log` into the system temp directory. The upgrade pass that removes those files refused to mark itself done whenever anything it did not delete was still there, and on a multi-user machine `/tmp` is sticky, so another account's leftover is PermissionDenied. Every later `codexbar` command then walked the whole temp directory before doing any work. Another account's file this process cannot unlink is no longer treated as unfinished work, so the pass writes its marker and stops looking. Logs this account still owns — including a Windows leftover that fails with PermissionDenied after an elevated run in the same `%TEMP%` — and logs still inside the 24-hour age bound, keep the pass alive until they are gone. An in-age log belonging to another account is treated the same way, rather than holding the marker open until it ages out. A log that disappears under the sweep, or that is replaced by a directory, is not counted as unfinished either.
- **`codexbar serve` no longer opens an unbounded number of connections, and no longer re-reads settings on every request.** The accept loop spawned a task per TCP client with no cap, and each `/usage` or `/cost` call loaded `settings.json` and account dirs again. A script polling `/usage` could multiply into one live provider fetch per connection, and a settings file that still carried legacy credentials serialized those tasks on the state write lock. Settings and configured accounts are loaded once at start, and at most eight connections run at once — the same cap the desktop refresh already uses. A ninth client is answered `HTTP/1.1 503` straight away rather than queued, so a caller that polls hard sees backpressure it can retry rather than a stall. The listening line is now printed only once the server is actually accepting. Restart the server to pick up a Preferences change. `--refresh-interval` is still unused; that is a separate ticket.
- **Codex charts no longer freeze a local date onto a session.** Each Codex usage record used to store the machine calendar day at parse time. After a DST change, a trip, or an OS timezone switch, an already-indexed rollout stayed on the old day: a one-day window could include usage a fresh parse would drop, and the daily bars could disagree with the heatmap. The index now keeps only the UTC timestamp and derives the local day on every scan. A log line whose timestamp is not RFC3339 (no offset) is left out of every bucket, instead of landing in the daily total and vanishing from hourly and reset-window totals. Closes SBS-944. Rebuilding the Codex index no longer discards the Claude one: the two files sat under one shared format tag, so a Codex-only layout change failed Claude's tag check and made the next scan reparse every Claude transcript for nothing. Each record type now versions its own index.
- **A first Charts scan no longer holds every newly parsed transcript in RAM until the index is written.** The parse path already worked a batch at a time so a 90-day walk would not keep gigabytes of records live; the index path undid that by collecting every `Store` into one `updates` vec and committing after the last file. Claude and Codex now apply each parse batch to the live index before the next batch is read, and write the snapshot once at the end of the scan. A user with a large corpus opening Charts after a cold start, or after the index is discarded, no longer pays for the whole corpus twice before a single byte is written.
- **A provider with no model prices no longer paints the activity heatmap as an idle month.** Unpriced models have always contributed tokens and zero dollars, which is correct for the dollar figure. The heatmap only received those dollars, so a busy Codex month on a newly released model id painted every cell at the empty shade, printed a zero total, and dropped the busiest-hour line. Hour rows now carry priced vs total token counts, the same coverage the Estimated API value card already uses, and the card names the gap instead of looking unused. Unknown prices are their own hatch, not the idle shade. The hatch marks the Estimated API value view only: under Tokens the count is fully known, so hatching it there would have blanked the intensity of a genuinely busy cell and called a known number unknown. The coverage note is stated once, in the card header, rather than repeated in the By-day line where a screen reader read it out twice.
- **Windows stops handing an npm shim or a directory to the PTY as the Codex or Claude binary.** `where.exe` prints every match on PATH and the resolver took the first line, so an extensionless POSIX shim named `codex` sitting ahead of `codex.exe` went straight to `CreateProcessW` and failed with error 193. Every candidate is now examined, a native `.exe` or `.com` wins over a `.cmd` or `.bat` shim wherever the shim sits in the list, and directories and extensionless files are refused — for PATH results, for the `CODEX_BINARY` and `CLAUDE_BINARY` overrides, and for an explicit path. A `.cmd` or `.bat` shim now runs through `%COMSPEC% /d /c` instead of being launched directly, and its arguments are escaped for the interpreter rather than for `CreateProcessW`, because the two do not agree on `&`, `|`, or `>`. The few tails `cmd.exe` cannot express at all fail with a message rather than launch something other than what was asked for. Closes #270. Thanks @ITSMERNB!
- **A price change that lands mid-scan is no longer stamped as current.** Charts can take tens of seconds over a large corpus. If models.dev refreshes while that walk is still running, the usage index and the Estimated API / heatmap cache used to write the old-price dollars under the new fingerprint, then serve them as current until a file changed or the 14-day TTL expired. Both writes now capture the fingerprint when the scan starts and drop the persist if prices have moved. Unknown (never captured) is its own state, not "current". The reverse case is covered too: a scan that starts wholly under the new prices may be written, but the values already in the cache file were priced under the old ones, so they are dropped rather than relabelled current alongside it.

### Internal
- **The no-glow prebuild check now sees a glow behind a CSS variable, and one written as an inline `boxShadow`.** `isGlow` stopped at the first non-length token, so `box-shadow: var(--halo)` was waved through even when `--halo` was `0 0 10px`. The check now resolves `var(--x)` against custom properties declared in the same file before judging the layer, and it reads `boxShadow: "..."` string literals in `.ts` / `.tsx`, including the `el.style.boxShadow = "..."` assignment form that is how an inline shadow is usually set. Regex literals are no longer mistaken for comments — `/^[a-z][a-z0-9+.-]*:\/\//` blanked the rest of its line, and a regex containing `/*` blanked the rest of the file, so any glow after one went unread. The property name written inside a string no longer counts as a painted glow. A custom property holding several comma-separated layers is re-split after substitution, so a glow sitting in the second layer of `--card: 0 1px 3px red, 0 0 8px red` is no longer read as the first layer's drop shadow. The live `box-shadow: var(--panel-shadow)` stays allowed: every declaration of `--panel-shadow` is an offset drop shadow.

## [Ceiling] 1.5.34 - 2026-08-18

Charts opens in about two seconds instead of about thirty. Each local transcript is now parsed once into a small index beside your settings rather than re-read from the top by every card, every time, and the cards keep their last result so a restart is not a cold start. The numbers are unchanged: an indexed scan is checked against a full re-parse, and the index is discarded outright whenever model prices move.

The rest of this release is about surfaces reporting a state they could not actually reach. Signing out of StepFun could leave a live token behind if a refresh was in flight, Gemini treated a token endpoint it could not reach as a signed-out account, a countdown could read "0m" while a window still had a minute in it, and About labelled a failed download as a failed update check and said it in half-translated English.

### Fixed
- **Charts opens in a couple of seconds instead of half a minute.** On a machine holding gigabytes of Codex and Claude transcripts, opening Charts started three separate walks of the same logs at once, and each one read every file from the top: Estimated API value scanned ninety days when the furthest period it shows reaches back sixty, the activity heatmap scanned thirty, and the provider charts scanned again on top. Nothing was kept, so switching tabs paid for all of it again, and clicking Yesterday or 30 days re-ran a full scan for numbers the card already had in hand. Each transcript is now parsed once and its records are kept in a small index beside your settings; a file that grew since is resumed from where the last read stopped rather than re-read from the start, and a file that was replaced rather than appended to is read again in full. Providers are scanned at the same time instead of one after another, both cards keep their last result on disk so a restart is not a cold start, and the work runs in the background shortly after launch for anyone who opens these cards, so the wait lands where nobody is watching it. The numbers are unchanged: an indexed scan is checked against a full re-parse, and the index is discarded outright if model prices move, since it stores the dollars they produced.
- **Signing out of StepFun no longer leaves a live Oasis token in the keyring.** A successful refresh wrote the new token to the OS keyring (`codexbar-stepfun` / `api_key`), and Revoke stored credentials only cleared Preferences, cookies, and token-accounts. The next fetch then read the leftover and stayed signed in. Revoke now asks the provider to delete that copy. A keyring error fails the revoke instead of reporting success while the token remains.
- **Gemini no longer treats a still-refreshable seat as signed out.** The quota poll used the access token until the exact expiry second and turned a 401 into AuthRequired, so a valid refresh token still produced one failed 5-minute cycle each hour. Gemini now refreshes five minutes early, like Claude and Vertex, and a 401 retries once after refresh. A revoked refresh token is still AuthRequired.
- **Reset countdowns agree at a remaining day and in the last minute.** The CLI statusline used `hours > 24`, so 24h 10m stayed "24h 10m" while the tray and the TypeScript hooks already said "1d 0h". The native tooltip and taskbar strip floored a still-future 30s remainder to "0m", the stuck-timer leftover SBS-621 fixed in the hooks. Every reset surface now floors one total of minutes, clamps a sub-minute remainder to 1, and cuts a day at 1440 minutes. A user looking at the tray, the CLI, and the tooltip at 24h 1s sees "1d 0h" (or "Resets in 1d" in the locale sentence); at 30s they see "1m", never "0m".
- **A failed update check no longer tells you that you are current.** Checking for updates treated a GitHub outage, a rate-limit, or an unreadable release payload the same as "no newer release", so About said you were up to date. Only a successful "latest is not newer" is Idle now. Failures, including the existing 15s timeout, are Error, and About shows that the check could not run. A second check after a download is ready no longer clears Install & Restart.
- **`codexbar cost` and serve `/cost` now include Grok.** CostScanner and Charts already read `~/.grok/sessions` (`costUsdTicks`). The CLI and the local HTTP endpoint still treated Grok as unsupported and said only Codex and Claude have local logs. After the usage command started enumerating enabled providers (default `claude`, `codex`, `cursor`, `grok`), every bare `codexbar cost` listed Grok as unavailable. The same machine's Charts tab already showed those sessions. `codexbar mcp` get_spend used the same Codex/Claude-only list. Cursor, Gemini, and Copilot stay unsupported.
- **WSL.md no longer documents automatic browser-cookie import or `gcloud auth login` for Gemini.** Cookie extraction is disabled; Gemini reads the Gemini CLI file `~/.gemini/oauth_creds.json`. The site no longer calls cookies "browser imports".

## [Ceiling] 1.5.33 - 2026-08-16

Adds an activity heatmap to Charts and an opt-in spend warning that needs no budget set, and lets the floating bar follow whichever app you are working in. Mostly, though, this release stops surfaces reporting a state they could not actually read: a provider outage is now told apart from an empty quota, a missing Cursor plan reads as unavailable rather than 0% used, SuperGrok's weekly figure is decoded rather than guessed at, the taskbar strip stops cutting the last character off a reset, and prices, chart caches, and CLI logs stop losing or hoarding data on disk. Release builds also drop the loopback exception their content policy was carrying from the dev server.

Supersedes 1.5.32. That version was tagged, signed, and drafted on 2026-08-16, and its installers were uploaded to the versioned download path, but the GitHub release was never published and no one received it. The `v1.5.32` tag stays as a marker. Everything from it is included here, plus the taskbar reset fix that was found in its build.

### Added
- **Providers having a public outage get a badge on their card.** A "0 tokens left" reading and a provider outage looked identical, so the second was read as the first. The provider card now carries the status page's own wording, plus a control that opens that page. Off by default and opt-in under Notifications, because it adds another set of hosts derived from your enabled providers - each provider's public status page - on top of the usage requests those providers already receive. Ceiling already contacts models.dev for public model prices and GitHub for update checks; those run whether this switch is on or not. Nothing about you is sent. Only enabled providers whose status page can actually be read are polled, at most once every fifteen minutes, and only while a surface is asking. An operational provider gets no badge, and neither does a page that could not be read.
- **The floating bar can follow the app you are in.** Pinned providers stay the default. Active shows the provider for the focused supported app or terminal agent. Active plus critical also keeps providers at or above the warning threshold. An unrelated window keeps the last active provider. Detection is local, cached, and does not call provider APIs. You can turn watching off.
- **Charts has an activity heatmap.** It answers when you actually work from timestamps already on disk: a 30-day strip and a weekday-by-hour grid, machine-wide across Codex, Claude, and Grok. Nothing new is collected and nothing leaves the machine. The hourly series is built in the same pass as the daily one and is a strict refinement of it, so the calendar view and the peak-hours view cannot disagree about a total. Intensity bands follow the quartiles of the cells that have activity, so a single heavy day does not flatten the rest of the month into one shade.
- **A warning when today's spend runs far above your recent norm.** A budget alert only helps someone who set a budget. This one needs no cap: it asks whether today looks unlike the days around it, which is the shape of a runaway agent loop well before the bill arrives. Off by default and opt-in under Notifications, alongside the other spend alerts, and it reads your local Codex and Claude logs, so spend on other providers is in neither today's total nor the baseline. It compares against the median rather than the mean, because one heavy day last week would otherwise raise the bar enough to hide a real spike today, and it leaves today out of its own baseline so a bigger runaway cannot make itself harder to detect. Under a dollar it stays quiet, and a baseline of zero reports nothing at all, since a fresh install and a genuinely idle week look the same from the dollars alone.

### Security
- **Release builds no longer allow the webview to reach loopback ports.** The shipped `connect-src` carried four `localhost` / `127.0.0.1` entries on any port. They exist for the Vite dev server and its hot-reload socket and were never needed at runtime: the webview makes no network calls of its own, and every provider request runs in Rust, including the Wayfinder gateway. Any script that ran inside the webview could previously reach every loopback service on the machine. The permissive rules now live in a dev-only overlay that `tauri:dev` merges in.

### Fixed
- **The taskbar strip stops cutting the last character off a reset.** The second line of a tile was trimmed to fifteen characters, which is a guess at a pixel width rather than a measurement of one. "Monthly · 10d 1h" is sixteen, so every monthly window with a two-digit day lost its trailing "h" and read as a countdown that was an hour out rather than as a line that had been cut, while "Weekly · 3d 23h" fit at exactly fifteen and hid the problem. The line is now measured in the font it is drawn in, against the width of the tile it belongs to. Where the whole thing will not fit it gets shorter in steps you can still trust: the countdown loses its finer unit first, so "Monthly · 23h 59m" becomes "Monthly · 23h" rather than nothing at all. That step matters most in the last day before a reset, when the countdown switches to hours and minutes and becomes the widest thing this line ever carries, wide enough to miss even the roomiest tile. Only when a tile cannot hold a coarse countdown does it fall back to the window name alone, which still says which ceiling it is; the figure is a hover away in the flyout.
- **SuperGrok's weekly percentage and reset were silently wrong.** The decoder scanned the response payload looking for anything that resembled a float or a timestamp, so it could pick up the wrong field and report a confident number that did not match Grok's own meter. Both the used percentage and the period end are now read from their named fields by number.
- **Grok picks the same account every time on a multi-seat login.** Account selection took whichever entry a hash map happened to yield first, so the SuperGrok meter could point at a different seat after a restart with nothing changed. It now prefers an explicit active marker, then the newest expiry, then a stable sort.
- **`codexbar usage` with no provider flag now reads the providers you enabled.** It was documented as printing your enabled providers and always queried Claude regardless. It resolves through the same enabled set the desktop app uses. `-p all` and `-p <provider>` are unchanged.
- **Spinner arrows, the caret, and selection highlights follow the app theme.** Those three parts of a form field are painted by the engine rather than by our CSS, and they were left to inherit `color-scheme` from the root. The app theme and the Windows theme are independent settings, so that inheritance was one refactor away from a light spinner on a dark field. Each control now sets it directly.
- **The last glow in the app is gone.** One rule still drew an 8px halo on the promo boost chip, against the rule the UI doc sets out. It is now a zero-blur accent ring, which keeps the emphasis. A prebuild check fails the build on any new shadow with no offset and a non-zero blur, and it self-checks so a detector that stops working fails loudly instead of passing quietly.
- **Linux and WSL look for sessions where they are actually kept.** Cursor's session database is read from `~/.config/Cursor` before the old guess, Claude Desktop's from `~/.config/Claude`, and the Windows profile behind a WSL install is no longer inferred from directory listing order, which could silently pick the wrong user. `CEILING_WINDOWS_USERNAME` names it explicitly when the guess is not wanted. Windows paths and the existing overrides are unchanged.
- **A crash on Linux or WSL no longer leaves a lock nothing can clear.** The state-write lock was a file created exclusively, so a second writer failed on the spot instead of waiting, and a process killed outright left the file behind for good. It now takes an exclusive `flock`, which waits its turn and is released by the kernel when the process dies however it dies. Windows locking is unchanged.
- **Opening Settings from the floating bar no longer lands on General.** The bar asked for a `menuBar` tab the Settings window does not have, so the window fell back to General instead of Display. It now opens Display, the same tab the dashboard already uses for that action. The lists that name a Settings tab on each side of the bridge are compared in CI so a renamed tab cannot silently send you to General again.
- **The OpenCode mark now matches the real brand, and stays visible on a light taskbar.** OpenCode's square-ring logo is monochrome, black on white or white on black, but nothing showed it that way. The taskbar strip painted it in an invented blue, and the dashboard drew it in the near-black taken straight from the brand asset, which left it almost invisible against a dark card. The strip now paints whichever half of the brand the taskbar behind it can actually show, which also fixes the Grok monogram beside it: both were a fixed near-white that disappeared on a light Windows theme. The dashboard cannot switch per theme the same way, so it takes one mid gray that clears both, the same one its chart bars use; the bundled SVGs now take their fill from that value instead of carrying their own. OpenCode Go charts pick up that gray too, having previously fallen through to the generic cost blue.
- **Cursor no longer treats missing usage as zero.** An empty `individualUsage` object used to paint a 0% monthly bar and hide a real team pool sitting next to it. Monthly is now marked unavailable when Cursor reports no reading, and an empty individual object falls through to team usage. Glance surfaces no longer paint a 0% Plan bar when monthly is unavailable — Overview, flyout, detail, floating bar, and the native taskbar tile show the named state instead. On-demand stays billed spend; plan and included dollars are labeled **Included** so they are not read as an invoice. A missing Composer tracking database is shown as unavailable, not as no activity.
- **Leftover English on glance surfaces now goes through locale keys.** Floatbar settings, freshness chips, account-status labels, Charts tab names, and About update copy used hardcoded English. They now use `en-US.ftl` (and zh-CN) so chips no longer show raw `stale` / `error` tokens.
- **Refreshing model prices no longer blanks out the prices you already had.** The models.dev price cache was emptied before the new copy was written, so a second Ceiling process reading during that moment saw nothing, and a crash mid-write threw the cache away for good. Either way token costs quietly disappeared until a network refresh worked. The new copy is now written beside the old one and swapped in, so there is never a moment with no prices on disk.
- **A Claude token refresh no longer replaces a symlinked credentials file.** Claude Code owns `.credentials.json`, and people who manage it with chezmoi or stow, or who share one file between WSL and Windows, keep a symlink at that path. Writing a refreshed token used to drop a fresh private file over the link, so the real file kept the old tokens and the next `chezmoi apply` signed Claude Code out. The refresh now follows the link and keeps the file's own permissions. It also takes a per-file lock on the shared file rather than on each path that points at it. If the app and the command line refresh the same login at the same moment, Claude retires the token the slower one used, so that one now steps aside and picks up the tokens that actually work instead of writing back dead ones and signing Claude Code out. Gemini and Grok already worked this way.
- **The chart cache no longer grows forever.** Its keys include the reset window they were built for, so every provider reset started a new entry and left the old one on disk for good. Someone who opened Charts after each reset added entries every day, and all of them were read back at startup. An entry is now dropped once it is two days old, which is what retires a rolled window; a 256-entry ceiling sits behind that as a backstop and should not be reached on a normal machine. A cache file left oversized by an earlier build is trimmed and rewritten on the next launch, and so is one written in an older cache format or one that no longer reads back.
- **The command line stops filling your temp folder with log files.** Every `codexbar` run wrote a small log named after its process id and never removed it. The statusline command runs once per editor redraw, so an active day could leave thousands of files behind, and Windows does not reliably clear temp. Logs now go to a private folder of your own instead of the shared temp folder, a run that works removes its own log, and statusline writes none at all. Logs from a run that was killed are cleared after a day. A run that genuinely fails still keeps its log so there is something to read. A typo like a wrong command does not, since the error is already on screen. The files older versions left in your temp folder are swept up on the first runs after this update, and after that the temp folder is not scanned again. On Linux and macOS the new folder is created private to your account, so nobody else signed in to the same machine can read those logs or leave anything in it for `codexbar` to write into.

### Internal
- Cursor usage-summary and Composer-activity paths now have deterministic fixtures for normal, partial, duplicate, and malformed data.
- The Claude credential persist is now covered on Windows, the platform its CI runs on: the rule that settles two concurrent refreshes, the file keeping its own security descriptor, and a symlinked credential path where the account may create one.

## [Ceiling] 1.5.31 - 2026-08-14

Hardens updates, credentials, and the local serve API, and ships the unpublished 1.5.30 work: a second tray click hides the dashboard, Grok banked resets show, and the taskbar strip finds a second lane when the usual gap is gone.

Supersedes 1.5.30. The GitHub release was tagged and drafted on 2026-08-13 but never published; the `v1.5.30` tag stays as a marker. A Microsoft Store submission was created from that tag. Everything from it is included here.

### Security
- **Ceiling checks who signed an update before it runs it.** An automatic update was trusted on the strength of the SHA256 that GitHub's release metadata reported, so that metadata was the only thing standing between Ceiling and launching an attacker's installer with the user's privileges. Every downloaded installer is now independently checked against Windows Authenticode and pinned to Ceiling's publisher identity, once when the download completes and again immediately before launch. An installer that fails either check is deleted instead of being left on disk to be retried. The publisher identity is pinned rather than the signing key, because Azure Trusted Signing issues a fresh short-lived leaf certificate for every release and a key pin would reject the next legitimate one.
- **`codexbar serve` now requires a per-user bearer token.** The local HTTP API bound to loopback with only a Host check, so any process on the machine could read usage, email, organization, and raw provider errors. `/usage` and `/cost` now need `Authorization: Bearer` unless you pass `--allow-unauthenticated`. The token is created at `<config>/Ceiling/serve.token` with current-user ACL on Windows and mode `0600` elsewhere, and is printed only the first time. Identity and raw provider errors are omitted by default; `--include-identity` opts back in. `/health` stays open.
- **Windows-owned PowerShell and `where.exe` no longer launch from PATH.** Notifications, Antigravity, the updater, and a few `where` lookups used a bare name, so a hostile current directory or PATH entry could substitute the binary. They now resolve under `%SystemRoot%\System32` and refuse to start if that file is missing. Claude, Codex, and `gh` still use PATH.
- **Cookie and token paste fields are masked, and destructive credential actions confirm first.** Manual cookies and token accounts used raw textareas, and Remove/Revoke fired on a single click. Secrets are hidden by default with an explicit reveal, and cookie, API-key, token-account, and provider-wide revoke go through an in-app dialog that names the provider and credential type. A successful removal refreshes provider state so leftover quota or identity is not left on screen.

### Added
- **A second click on the tray icon hides the dashboard.** Left-click always reopened the stats window, so a glance meant hunting for the close button. First click shows it, second click hides it. A Windows double-click is ignored so the window does not flash open and disappear. Right-click and the keyboard shortcut are unchanged. Closes #280.
- **Grok banked resets show up the way Codex already did.** Grok's Settings → Usage now lists redeemable reset tokens. Ceiling already counted those for Codex and left Grok's chip blank. The same count now appears on Overview, provider detail, and the taskbar flyout. Redeeming still happens on grok.com. Ceiling only displays how many are left.
- **Settings says why the taskbar widget is hidden.** The native strip can vanish because there is no gap, because Start cannot be found, or because no providers are enabled, and none of that was visible anywhere, so the feature looked broken. The Taskbar Usage group now reports "Shown on N taskbar(s)", no free space, waiting for landmarks, or no enabled providers. The row stays off when the widget is disabled. Thanks @diogochaves!
- **OpenCode and OpenCode Go show their mark on the taskbar strip.** Those providers had no bitmap in the native overlay, so the tile was a blank disc. They now use the square-ring glyph and the same blue as the rest of the app.

### Fixed
- **`diagnose` no longer copies provider error text into an export meant to be shared.** The command already withheld cookies, tokens, account emails, and response bodies, but a failed fetch put the provider's own error string straight into the payload, and that string can carry a signed URL, an account identifier, or an echoed request header. Every failure now reports a local category and a fixed local message instead of anything the provider said.
- **Heavy Codex sessions no longer vanish from Charts and cost totals.** Codex usage was parsed into 32-bit integers, so a cumulative total past roughly 2.1 billion tokens wrapped to a negative number, which the next step clamped to zero. The effect was not a wrong number but a missing one: the session's tokens and its dollars simply stopped being counted. Token counts are now 64-bit from the log line through to the summary, cumulative deltas saturate instead of wrapping, a genuinely corrupt total is skipped without poisoning the running baseline, and a decreasing cumulative total no longer lowers that baseline so the next valid line over-counts.
- **Adding, removing, or switching an account no longer races another change.** Every account edit read the store, changed it in memory, and wrote it back, with nothing holding those three steps together. Two edits that overlapped could each write a copy of the file that predated the other, so one of them disappeared with no error and no sign anything had gone wrong. A Copilot device login finishing while the user edited accounts in Settings could do the same. Account changes now run as a single locked transaction against the latest state on disk.
- **A token refresh no longer rewrites the whole Gemini or Grok credential file.** Both providers refreshed by reading the file, rebuilding it from the fields Ceiling models, and writing it back with no lock and no atomic replace. Three things could go wrong: two refreshes at once could interleave and lose one, a crash partway through the write could truncate the file and sign the user out, and any field Ceiling did not model was dropped on the floor, including a newer refresh token the provider's own CLI had just written. A refresh now holds a lock for the whole read-modify-write, merges only the fields that actually changed, and replaces the file atomically while preserving its original permissions. Grok writes `refresh_token` only when the token endpoint issued a new one. If the credential file is missing, persist recreates it instead of failing the refresh.
- **Charts no longer mix one account's usage into another.** A removed or unresolved account could inherit the machine-wide transcript cache, so Charts showed someone else's tokens under the wrong tab. Each configured account now has its own chart tab and its own cache identity. Compare stays an explicit aggregate. Polling stops for identities that no longer resolve.
- **Provider detail no longer applies a stale refresh.** Switching providers or accounts quickly could let an older request finish last and paint the previous seat's numbers onto the new one. Detail commits now require a matching request epoch plus provider and account identity, account selection resets at provider boundaries, and bursty provider updates coalesce before a refresh.
- **Credential status is scoped to the provider you have selected.** Shared credential-file protection was treated as "this provider has a credential", so opening provider B could show a protected store and a revoke action that belonged to provider A. Presence is now per selected provider, and per selected token account. An unreadable store stays unknown instead of reporting a false absence.
- **The taskbar widget takes a second lane when the usual one does not fit.** On a left-aligned taskbar, Start sits at the left edge and the Widgets-to-Start gap's right edge goes negative, so the strip hid with no explanation. Windhawk's "Start button always on left" did the same. The Widgets-to-Start lane is still preferred. If it has no verified gap, the widget tries the stretch between Start and the tray and lands in the largest empty gap, never covering a pinned icon. Left alignment with Windows Widgets enabled used to freeze the strip on top of the app icons instead. That path now places into the fallback lane and treats the Widgets entry as an obstacle. Closes #261. Thanks @diogochaves!

### Internal
- The privileged PR-review runner pins `actions/checkout` to a reviewed commit instead of a moving tag.

## [Ceiling] 1.5.29 - 2026-08-12

Finishes what 1.5.27 started: Cursor's on-demand spend now reads as money on every surface that shows it, not only the Overview card. Also lets a monthly quota raise a pace warning, and gives every settings control a name a screen reader can reach.

Supersedes 1.5.28, which was tagged but whose build was cancelled before it signed or published anything; everything from it is included here. The `v1.5.28` tag stays as a marker.

### Fixed
- **Cursor's on-demand spend reads as money on the taskbar, not just on the Overview.** 1.5.27 taught the Overview card to show dollars, but the surfaces you actually glance at were left behind. The taskbar tile picked the on-demand lane correctly and then drew "62%" — the fraction of a spend cap, which says nothing about the $1,112.92 behind it, and which inverts to a cheerful "38%" if you display remaining rather than used. The flyout was worse: it discarded any lane whose name contained "on-demand" before building its rows, so the tile named a lane that the panel beneath it refused to show, and because the same filter ran before the "+N more limits" count, nothing hinted that a row had been dropped. The free-floating bar carried the identical percentage-only defect. Activity had the same habit from the other direction: it listed the On-demand row faithfully and then headlined it "56% used", describing the shape of the bar instead of the bill. A lane billed in currency now leads with the amount on every surface that shows it — taskbar tile, hover flyout, floating bar, and the Activity schedule — the flyout lists On-demand with its spend beside the bar and reserves it a slot so a depleted plan cannot crowd it out, and on-demand running uncapped reports spend-to-date rather than going blank for want of a denominator. Reported in #191.
- **A monthly quota can raise a pace warning.** 1.5.27 fixed the pace verdict shown on screen but not the toast behind it. Predictive pace warnings looked only at a provider's primary and secondary windows, and a monthly quota arrives in the tertiary slot — so a month on course to run dry before it reset could never say so, while the weekly window beside it warned freely. Every core slot is now a candidate, and two slots reporting the same cadence collapse to a single warning rather than clobbering each other's state.
- **Settings controls have names a screen reader can reach.** A settings row rendered its visible label as plain text with nothing tying it to the control beside it, so a screen reader arrived at an unlabelled toggle, dropdown, or number box; text inputs could not be given a name at all. Each row now supplies its label to the control it wraps, without overriding one a control already carries. Closes #215. Thanks @Vermitrude!

## [Ceiling] 1.5.27 - 2026-08-11

Makes Cursor's on-demand spend readable at a glance, corrects two numbers Ceiling was reporting wrong, and replaces browser cookie import with a manual setup you can see.

Supersedes 1.5.26, which was built but never published; everything from it is included here.

### Changed
- **Browser cookie import is gone, replaced by a manual copy-and-paste setup.** Ceiling could read cookies out of the browser's own database to authenticate a provider, and provider fallbacks could reach for that database without being asked. Handing an app your browser's cookie store is a large amount of trust for a usage meter, and it happened where you could not watch it. Providers that need a cookie now ask for one, with a guide for copying it out of the browser's developer tools, and the value goes to the same secure store as every other credential. **If a provider was authenticated by browser import, it will ask you to set it up again.** Everything else is untouched: CLI credentials, IDE credentials, OAuth sign-in, and Claude Desktop sessions are all still detected automatically, and providers on those paths need no attention.

### Added
- **Tab strips can be driven from the keyboard.** Settings, the Charts provider selector, the account switcher, and the chart-type tabs all announced themselves as tab strips to assistive technology while ignoring arrow keys entirely, and each one spent a Tab stop per tab. Left and Right move between tabs, Home and End jump to the ends, focus wraps, and a strip is now a single Tab stop. Each panel names the tab that opened it, and panels are focusable, so tabbing out of a strip lands in the content it just opened and a scrolling panel can be scrolled from the keyboard.

### Fixed
- **Cursor's on-demand usage is readable without opening a provider.** On-demand is the only Cursor lane that bills real money, and once the included plan is at 100% it is the only number that matters. It now shows its percentage and its dollars together on the Overview card — "56%" beside "$1,002.16 of $1,800.00" — and takes the taskbar strip's slot once spending starts or the included allowance is exhausted. An unused `$0` on-demand lane stays out of the way on a healthy card. Reported in #191.
- **Codex no longer overcharges `gpt-5.1-codex-mini`.** Pricing lookup collapsed `*-codex` model names onto their base model, which is right for the plain aliases but folded the distinct `mini` and `max` variants into `gpt-5.1`. Mini input was billed at the `gpt-5.1` rate of $1.25 per million tokens instead of its own $0.25, so every cost total containing mini usage read high. Dated and `openai/`-prefixed spellings of those variants now resolve to the same correct entry.
- **The pace verdict describes the window that is actually running hot.** Pace was chosen by cadence — the weekly window, always — so a provider reporting both a weekly and a monthly quota could show a reassuring verdict about the window nobody was spending. An OpenCode Go account read "Weekly pace · Far below budget · -15.0%" directly beneath a monthly bar whose own marker showed it fifteen points over. Monthly quotas were doubly unreachable: pace looked only at the primary and secondary slots, and monthly quotas are reported in a third slot, and the cadence test would have rejected anything longer than fourteen days anyway. Every window a provider reports is now a candidate, and the one running hottest against its own clock is the one you are told about. The twelve-hour floor stays, so a five-hour session still gets no verdict rather than a meaningless one.
- **Concurrent writes can no longer lose stored settings or credentials.** Settings, API keys, manual cookies, and token accounts were each read, changed, and written back without a lock, so two writers overlapping — the desktop app and the CLI, or two changes landing together — could drop whichever change was read first. All four now serialize through one cross-process lock, and revoking a provider's credentials happens inside a single validated critical section rather than as several separate writes.
- **Configured Claude accounts no longer collapse onto one globally active session.** When multiple accounts use separate `CLAUDE_CONFIG_DIR` directories, each card now fetches through OAuth credentials scoped to its own directory. Ceiling no longer tries the process-wide Claude Desktop, browser, ambient CLI, or API-token session first, which could make two distinct accounts show the same email and usage. Single-account automatic fallback behavior is unchanged.

### Internal
- Cost pricing tables and cost math have regression coverage for the first time, including the mini-rate case above, along with new tests for the usage-level threshold classifier and the critical usage boundary.
- Owner pull requests are reviewed by a pinned Grok action.

## [Ceiling] 1.5.25 - 2026-08-09

Patch release for credential and settings integrity, safer custom Codex endpoints, and a Claude taskbar tile that stays focused on usable account capacity.

### Security
- **A custom Codex `chatgpt_base_url` can no longer be pointed at a remote host over plaintext.** Every usage refresh sends the Codex access token to that URL, and the check that was meant to confine plaintext to a local proxy compared string prefixes. `http://localhost@attacker.example` passed it — `localhost` is userinfo there, and the real host is remote — as did `http://localhost.attacker.example`, where the loopback literal is just a label of someone else's domain. Either one received the token in the clear on every refresh. The value is now parsed as a URL and judged on its actual host, with user info rejected outright. Legitimate local proxies on `127.0.0.1`, `localhost`, or `[::1]` still work, and a rejected value falls back to the default backend instead of being used.

### Fixed
- **A maxed model sub-limit no longer takes over the Claude bar.** Claude reports per-model caps — the scoped weekly lanes ("Fable only", "Opus only", "Sonnet only") and the seven-day Opus/Sonnet cap — alongside the real Session and Weekly pools. The taskbar shows one lane per provider and picked whichever was closest to its ceiling, ranking already-blocked lanes first, so a model cap at 100% claimed the whole Claude tile and hid a session and a week that still had capacity. Hitting one model's cap does not stop work, it means you use another model, so these lanes no longer compete for that slot. Both the native taskbar tile and the floating bar are fixed, and they no longer disagree. Model caps still appear in provider detail, the taskbar flyout, the tray menu, and the Activity timeline, which list every window. This also stops Claude being reported as exhausted, and stops it deciding which account owns the tile, on the strength of a model sub-limit alone.
- **An unreadable credential store is no longer overwritten with a partial one.** Saving an API key, saving a manual cookie, or revoking one provider read the store first, and a store that could not be decoded — corrupt JSON, a DPAPI unprotect failure, a secure-file version this build does not understand — was read as empty. The following save replaces the whole file, so one provider's change destroyed every other stored secret. Those operations now fail with a diagnosable error and leave the file untouched. A store that was never created is still simply empty, so first-run setup is unaffected.
- **One unrecognized value no longer resets every preference.** `provider_configs` was keyed strictly, so a single provider id this build does not recognize — a file written by a newer build, a renamed provider, a hand-edited typo — failed the entire settings parse. Settings then loaded as defaults, and the next tray toggle or preference edit persisted those defaults over the real file. Unknown ids are now parsed past and parked, so they survive a downgrade and are folded back in by a build that knows them. The same treatment covers the fixed-choice settings (language, theme, update channel, per-provider metric), where a value from a newer build used to fail the document just as completely; each now falls back on its own instead of taking every unrelated preference with it. A legacy inline credential stored under an unrecognized provider id is migrated to the secure store rather than dropped.
- **Token accounts stored under an unrecognized provider id survive an unrelated save.** The store dropped ids it could not resolve on load and then rewrote the whole file from what was left, so adding, removing, or switching an account for any known provider permanently erased credentials belonging to another build. They are now carried across writes. Removing a provider this build does recognize still removes it.
- **Deleting a token account no longer silently switches which one is active.** The active account was tracked by position and only corrected when that position ran off the end of the list, so removing any account above it left the marker pointing at a different seat — with three accounts and the second selected, deleting the first quietly promoted the third. Fetches then ran as an account the user had not chosen. The selection is now tracked by identity, matching how directory accounts already worked. Deleting the active account itself still falls back to its neighbor.
- **A countdown just under an hour boundary no longer loses most of an hour.** Hours were floored while minutes came from a separately rounded total, so the two could disagree: with 1h59m30s left the minutes wrapped to zero before the hours advanced and the window reported "1h 0m".
- **The relative reset countdown no longer reads "Resets in 0m".** The final minute before a reset floored to zero, which looks like a stalled timer rather than an imminent reset; it now holds at one minute until the reset actually arrives and reports it as due.

## [Ceiling] 1.5.24 - 2026-08-05

Answers "am I going to run out before this resets?" on the usage bars themselves, and makes Cursor's on-demand spend visible in dollars. Both came from user feature requests (#190, #191).

Supersedes 1.5.23, which was built but never published; everything from it is included here.

### Added
- **Expected usage is marked on the bars.** Every weekly and monthly bar, in the Overview and in a provider's detail view, now shows where usage should be at this point in the window. One rule everywhere: the marker is where the bar's edge should be right now. Overspending fills the span between the edge and the marker with a striped band, so the question reads as "how far ahead am I" rather than "which side of a line am I on". Bars that show remaining capacity mirror the marker so it keeps the same meaning either way.
- The marker is derived from elapsed time against the window's own duration, so it needs nothing from the provider and appears on every long window at once, rather than only on the single window a pace prediction is calculated for. Windows shorter than twelve hours are skipped: a five-hour session is not spent evenly, so a marker there would sweep across the bar and mean nothing.
- **The tray card carries a pace verdict.** It names the outcome (on track, ahead of pace, plenty left, or running out early) and states the consequence. The detailed expected-versus-actual breakdown was hidden in the tray for being too tall, so the prediction Ceiling already computed was invisible exactly where people look for it. The taller breakdown still appears in the main window.
- **Predictive pace warnings are available again**, as an opt-in under Settings > Notifications, alerting when a window is on course to be exhausted before it resets. The setting had been pinned off on every load with no way to enable it, and was additionally restricted to Claude and Codex. Any provider that reports a reset can raise one now, and warnings are named by the window's real cadence, so a monthly quota is no longer announced as a "Session" limit.
- **A metered window can carry the money behind it.** Cursor's on-demand lane shows its dollars beside its bar, which is what makes an overdraft readable once the plan is at 100%.

### Fixed
- **Cursor on-demand could disappear entirely.** It was only read when the account reported a `plan` object, so accounts reporting `overall` lost the overdraft meter; and deriving a percentage needs a cap, so anyone running on-demand uncapped got no meter at all and their spend was dropped. Uncapped spend is now reported as an explicit non-metering line rather than discarded for want of a denominator.
- On-demand is the only Cursor lane that bills real money, so it takes the cost slot ahead of plan and pooled-team usage, and is labeled "On-demand" rather than folded into a generic "Monthly". It also no longer gets filtered out of the app window by name, which had kept it off the surface most people actually use.
- **Removed the Cursor "Promotional" meter and its badge.** The percentage was plan usage minus the included allotment, and the included lane is its own closed set, so the subtraction was always zero and the meter read 0% permanently. The badge beside it claimed the bonus expired at the end of the billing cycle, while Cursor credits expire on their own schedule. Both numbers were wrong and neither can be derived from what the API reports, so the lane is gone rather than guessed at.
- Claude's Opus model pool can raise usage alerts alongside the session and weekly windows; it could previously sit at 99% in silence. Monthly windows stay excluded on purpose, since crossing a threshold mid-cycle is normal there rather than news.
- Threshold alerts are keyed by cadence rather than by slot, so a weekly window promoted into the primary slot is no longer reported as a session.
- Devin resolves its usage percentages across every reported window at once, so a response holding `0.4` beside `32` reads the `0.4` as 0.4% instead of rescaling it to 40%.

### Internal
- Release binaries are built with link-time optimization, a single codegen unit, and symbol stripping for the first time. Cargo only reads profiles from the workspace root, and this one lived where cargo ignored it and warned about it on every build, so shipped binaries were larger and slower than intended. `panic = "abort"` was in that ignored block and is deliberately not carried over: it has never applied to a shipped build, and would turn a panic in a background refresh into an immediate process kill.
- Cost-scanner tests no longer mutate `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, or `GROK_HOME`. Changing process environment while another thread reads it is undefined behavior, and nine other modules read those variables constantly, so unrelated scans intermittently resolved the wrong home. One such failure also poisoned a shared mutex and cascaded into three more, which disguised a single fault as four. The scanner now takes an injected ambient home; the suite went from failing roughly one run in five to twelve consecutive clean runs.

## [Ceiling] 1.5.22 - 2026-08-03

### Fixed
- A lightly used OpenCode Go account could report its rolling window as 100% used while the dashboard showed 1%. The usage page reports each window as either whole percentages or fractions of the limit, and a lone `1` means 1% in one and 100% in the other. The old rule scaled any value at or below 1 by 100 window-by-window, so the first 1% of use rendered as a maxed-out rolling window. The scale is now resolved once per response, and only read as fractions when a window actually contains a fractional value, which is the case that proves it.
- The same 1%-reads-as-100% scale bug affected the OpenCode, Qoder, Chutes, and Sakana providers, all of which scaled values at or below 1 by 100 window-by-window. OpenCode (same backend as OpenCode Go), Qoder, and Chutes now resolve the scale once per response like OpenCode Go. Sakana no longer scales literal percent text at all (a page showing "1%" cannot mean 100% used), and only its JSON percent keys use the evidence-based scale.
- The OpenCode Go card now names its monthly bar "Monthly" instead of the generic "Extra", so the third usage window on that card is no longer anonymous.
- The Microsoft Store submission for 1.5.21 was rejected because Partner Center limits installer parameters to 40 characters and the inherited value was longer. The parameters are now normalized to that limit, startup-prompt suppression and the restart-required exit code are preserved in the Inno installer, and a deterministic Store-package preparation test guards it in CI.

## [Ceiling] 1.5.21 - 2026-08-01

Patch release for duplicate scheduled-reset notifications and reliability hardening around local state and account-scoped history.

### Fixed
- A confirmed monthly or weekly reset now advances its own baseline immediately, even when another quota window is still awaiting confirmation. The stale baseline could otherwise replay the same reset and send two or three identical notifications during one provider refresh cycle.
- Chart history, quota-run efficiency, and chart caches now follow the stable account ID, with email and organization fallbacks. Accounts sharing an email no longer blend history, and organization-only providers can read back the history they recorded.
- Settings, credentials, history, geometry, and cache files are replaced atomically, so an interrupted write cannot truncate the last known-good local state.
- Claude refreshes report HTTP client initialization failures instead of panicking a background task, and refreshed OAuth credentials are replaced reliably on Windows.

### Internal
- The release smoke test now asserts that the installed Start Menu shortcut carries Ceiling's AppUserModelID. That property is what lets Windows keep a notification, and 1.5.17 shipped without it: every gate checked source, while the one step that inspects the installed build never looked at it.
- Shortcut identity validation now asks the Windows Shell for `System.AppUserModel.ID`. The custom property marshaller misread a correctly packaged shortcut as empty and blocked the unpublished 1.5.20 release.

## [Ceiling] 1.5.19 - 2026-07-29

Follow-up to 1.5.17, which shipped its own notification fix inert on a clean install. Also stops a lightly used Claude account reporting itself as maxed out, and makes Hide Personal Info do what it says.

### Fixed
- A lightly used Claude account could report its 5-hour session as 100% used, and raise an exhausted alert, while the account was at 1%. Anthropic reports utilization as either whole percentages or fractions of the limit, and a lone `1` means 1% in one and 100% in the other. A response whose windows were all `0` or `1` was assumed to be fractions, so the first 1% of use rendered as a maxed-out session. The scale is now only read as fractions when the response actually contains a fractional value, which is the case that proves it.
- Release builds no longer fail because a large installer is still becoming publicly readable. The check now verifies the tiny checksum sidecar first, which proves the download path is serving, and only warns if the installer itself is still settling. A wrong status such as a redirect or a permission error still fails immediately instead of being waited out.
- The Start Menu shortcut created by the installer now carries Ceiling's AppUserModelID. Without it Windows could not resolve who a toast came from, so it drew the banner and discarded it, and 1.5.17 shipped with its own notification-history fix inert on a clean install. If you installed 1.5.17, reinstalling picks up the corrected shortcut.
- The updater refuses to apply an update that would not replace the running copy. A Ceiling started from anywhere other than the installed directory (a local build, an install from older packaging) would watch the installer succeed elsewhere, stay on the old version, and offer the same update forever with nothing explaining why. It now says so instead of looping.
- Cached update installers are pruned once they are superseded. Every download was kept forever; one machine had ten going back to 0.43.3, 305 MB of them.
- **Hide Personal Info** now actually hides it. The setting only ever reached the Accounts list, while the Overview, taskbar flyout, plan cards, activity timeline and provider detail all printed the raw address, because each built its account line from a helper that did no masking. Masking now happens inside that helper so a surface cannot opt out by forgetting, and it masks addresses inside labels too, since an account label defaults to `email (plan)` and was a second copy of the same address.
- Switching accounts on the Providers page works. The selected account id was accepted by the frontend command wrapper and then never sent to the backend, so every click re-fetched whichever account the backend picks on its own and nothing appeared to happen.
- Enabled providers sort to the top of the Providers list, with the rest by name. A configured provider could otherwise sit far down among ones you do not use. Drag order is kept within the enabled group, because that same order decides how cards are arranged in the tray flyout and pop-out.

## [Ceiling] 1.5.17 - 2026-07-29

Windows notifications are the headline: they now carry the Ceiling name and logo, stay in the notification center instead of vanishing after a few seconds, and stop dropping the weekly and monthly resets worth interrupting for. Also adds Simplified Chinese and in-app provider login.

### Added
- **Simplified Chinese** is a real, switchable interface language, selectable under Settings > General. Missing keys fall back to English, so new strings keep shipping without waiting on translation. (#113, #130)
- In-app login flows for Claude and Codex, alongside the existing Copilot device flow, with typed and localized progress shown in provider settings. Providers without an in-app flow now return explicit credential guidance instead of silently opening a dashboard. Gemini remains a known gap because its CLI only authenticates through an interactive TUI. (#179)

### Fixed
- Estimated API value no longer under-reports on machines with more than one Codex or Claude seat. Unscoped scans only read the ambient home, so secondary accounts added under **Accounts** contributed nothing; every configured account home is now included, with dedup so overlapping copies do not inflate the total. Per-account charts still read only that account's home. (#176)
- The one-number strip (native taskbar and float bar) could sit on a maxed-out Cursor API lane while Auto still had room. Cursor now shows the hottest non-exhausted Auto/API window, falling back to whichever resets soonest when both are spent. (#175)
- Windows notifications carry the Ceiling name and logo instead of arriving as unattributed text, and now stay in the notification center instead of flashing once and disappearing. Toasts were published under the AUMID `Ceiling`, which no Start Menu shortcut claimed, so Windows treated the app as unregistered and stamped `ShowInActionCenter=0` on that channel. They now use the installer's bundle identifier, which is a registered identity, and an existing `ShowInActionCenter=0` is repaired on launch. Portable builds are unaffected by design: Windows only keeps toasts for an app claimed by a Start Menu shortcut, and Ceiling will not add one to a machine where you chose a portable build, so portable alerts appear as banners without notification-center history.
- A confirmed reset is no longer dropped by the one-toast-per-refresh limit. Providers refresh concurrently, so an unrelated alert could silently swallow the only weekly-reset notification a user would get all week. Resets now bypass that limit and the rolling cooldown, bounded only by a per-refresh storm guard.

### Changed
- Scheduled 5-hour session resets no longer raise a Windows notification; they happen several times a day and are exactly what the user already expects. Weekly, monthly and other long windows always notify, and unexpected resets (early, partial, banked) still notify at any cadence.

### Internal
- Microsoft Store submissions are automated after a tagged release, gated behind an explicit enable flag until a validation-only run succeeds. Store validation runs in its own environment so manual runs cannot reach the release signing and R2 credentials. (#177, #178)

## [Ceiling] 1.5.16 - 2026-07-28

Hotfix: Estimated API value **Custom** date range showed no data even when local logs had spend for that window.

### Fixed
- Custom ranges fall back to the scanned daily dollar series when the dedicated custom window is empty (#173)
- Custom range UI keeps the ring layout while loading and shows a cleaner date bar (#173)
- Codex local scan no longer drops window totals when a daily bucket key is missing (#173)

## [Ceiling] 1.5.15 - 2026-07-28

Patch for reported Antigravity quota mismatch, Claude sign-in vs charts confusion, and custom chart date ranges.

### Fixed
- Antigravity capacity uses `RetrieveUserQuotaSummary` shared model-group weekly / five-hour pools (matches Settings to Models) instead of only per-model `remainingFraction`. (#163, #167)
- Claude capacity errors no longer collapse multi-source Auto failures into an OAuth-only line; copy points at CLI `claude` login and notes Charts can still show local session spend without live capacity. (#165, #166)

### Added
- Estimated API value **Custom** date range (inclusive local From/To, up to 366 days) alongside Today / Yesterday / 30 days. (#164, #168)

## [Ceiling] 1.5.14 - 2026-07-26

Patch so Weekly window API-equivalent dollars match priority/fast pricing after 1.5.13.

### Fixed
- Invalidate persisted chart cache after Codex priority/fast pricing so the Weekly window card no longer keeps pre-2x standard dollars while the API-value ring shows the correct doubled amount. (#161)

## [Ceiling] 1.5.13 - 2026-07-26

Patch for local cost accuracy vs ccusage (Codex priority/fast tiers and Claude day windows).

### Fixed
- Codex local cost now matches ccusage speed tiers: `service_tier = "priority"` in `~/.codex/config.toml` prices at the fast (2×) rate, with `codexbar cost --codex-speed auto|standard|fast` and JSON `cost.codex_speed` / `cost.codex_service_tier` for fair A/B compares. (#158)
- Claude local cost `--days N` uses the same inclusive local calendar window as Codex (and ccusage), instead of a rolling UTC duration that labeled N+1 calendar days. (#158)

## [Ceiling] 1.5.12 - 2026-07-26

Patch release for reported Antigravity and multi-account bugs, plus a taskbar strip hygiene fix for providers that are enabled but not yet ready.

### Fixed
- Detect Antigravity on Windows when the language server uses `--https_server_port 0` and omits `--extension_server_port` (modern Antigravity 2.3+). (#153)
- Detect Antigravity **CLI** (`agy` / `antigravity-cli`) as well as the IDE language server. The CLI hosts the same local quota API without a CSRF token; Ceiling now probes it when `agy` is running and signed in.
- Drop ambient ghost Codex/Claude readings after Accounts registers the signed-in directory, so Overview no longer shows two identical cards for one seat. (#155)
- Keep unauthenticated / not-installed providers off the always-visible taskbar strip (they still show on Overview and Settings for setup). Antigravity error placeholders no longer surface as a blank "Claude" pill.
- Give Gemini (and Antigravity) a real native taskbar glyph instead of the hollow-ring fallback, and keep strip SVG marks brand-colored so first-class seats stay identifiable.

## [Ceiling] 1.5.11 - 2026-07-25

Signed draft of everything since public **1.5.6**: multi-account strip controls, Charts trust/efficiency, strip density polish, flyout alignment, constraining-window taskbar meters, and Grok API-equivalent dollar charts. Supersedes draft tags **1.5.7**–**1.5.10**.

### Added
- Price Grok Charts from local session `costUsdTicks` (same API-equivalent Cost as Grok Build `/usage`), including the Estimated API value card. Token/cache/effort/project rollups still apply; partial sessions without ticks stay unpriced with coverage disclosure.

### Fixed
- Native taskbar tiles use the constraining usage window (session vs weekly) instead of always showing the primary window, so a maxed weekly pool no longer reads as a free 5h bar.
- Taskbar flyout **On strip** row no longer shifts left of the other providers. The strip seat keeps the brand tint without rewriting margin/padding, so icons and meters share one left edge.

## [Ceiling] 1.5.10 - 2026-07-25

Draft-only tag; use **1.5.11** for installs. On-strip flyout alignment (included in 1.5.11).

## [Ceiling] 1.5.9 - 2026-07-25

Tag exists but does not satisfy release validation (not on protected main). Use **1.5.11** for installs.

## [Ceiling] 1.5.8 - 2026-07-25

Draft-only tag; use **1.5.11** for installs. Strip tile density and flyout chip hierarchy (included in 1.5.11).

### Fixed
- Native strip detail line is window label only (Weekly / 5h) plus optional reset. Long account names no longer run into the next provider tile; seat identity stays in the flyout (**On strip** + account line).
- Flyout **On strip** chip is quieter and smaller; banked-resets chip is slightly larger so the hierarchy is clear.

## [Ceiling] 1.5.7 - 2026-07-25

Draft-only tag; use **1.5.11** for installs. Multi-account strip controls, Charts trust, and quota-run efficiency (included in 1.5.11).

### Added
- Pin which multi-account seat drives each taskbar strip tile (Settings → Taskbar). The strip no longer always picks the hottest account when you care about a specific Codex or Claude seat.
- Mark the strip account in the taskbar flyout (**On strip**) and list it first within each multi-account provider.
- Persist completed quota runs when a reset is confirmed, and show a **Quota run efficiency** card on Charts: tokens per 1% used, cache-read share during that run, projected tokens at 100% (once the run has enough peak), and run-over-run change vs the previous complete run on the same window. Labeled as local observation, not a published allowance.

### Fixed
- Include multi-project and partial Grok local session usage on Charts so toolport/multi-cwd work is not missing from project rollups.
- Show **N% of tokens priced** on reset-window and calendar period cards when unpriced models shrink the dollar total, matching the Estimated API value card. Fully priced windows stay quiet.

## [Ceiling] 1.5.6 - 2026-07-24

Signed release of Grok charts polish since 1.5.5.

### Added
- Scan local Grok Build sessions under `~/.grok/sessions` for Charts: tokens over time, cache vs fresh input, reasoning tokens, reasoning-effort tiers, and project rollups (unpriced SuperGrok pool usage; no fabricated API dollars).

### Fixed
- Label Grok's weekly pool as Weekly on the taskbar strip and popout instead of Extra credits.

## [Ceiling] 1.5.5 - 2026-07-24

Installable signed draft of the full first-class Grok story. Supersedes the unpublished 1.5.3 draft and the mis-pointed 1.5.4 tag (same product content, tagged from protected `main`).

### Added
- Treat Grok as a first-class provider alongside Claude, Codex, and Cursor: default-enabled, early catalog order, dedicated data-source copy, and enforcement tracking for the weekly pool.
- Show Grok on Charts (sampled weekly-pool history), raise the native taskbar strip to five providers so Grok can sit after Cursor, and add Settings → Display controls to pick and reorder strip providers.
- Use the official Grok monogram for tray, overview, providers, charts, and the taskbar strip.
- Refresh expired Grok OIDC access tokens from `~/.grok/auth.json` via auth.x.ai (same silent refresh pattern as Claude).

### Fixed
- Make Grok usage tracking work with a normal `grok login`, the same way Claude and Codex pick up their local CLI sign-in. Empty cookie settings no longer force a "CLI not supported" path, and SuperGrok Heavy weekly pool responses that omit a zero percent reading show 0% with the correct weekly reset instead of failing to sync. The plan name (for example SuperGrok Heavy) is read from your Grok account when available.
- Prefer `grok login` credentials over browser cookies, and surface clear re-login guidance when Grok auth fails.

## [Ceiling] 1.5.4 - 2026-07-24

Tag exists but does not satisfy release validation (pre-squash tip). Use **1.5.5** for installs.

## [Ceiling] 1.5.3 - 2026-07-24

Draft-only tag; use 1.5.5 for installs.

## [Ceiling] 1.5.2 - 2026-07-23

### Fixed
- Add a signed Microsoft Store installer that bundles the full Microsoft Edge WebView2 Evergreen Standalone Installer and needs no runtime download during setup. The regular smaller installer remains available for GitHub and Winget.

## [Ceiling] 1.5.1 - 2026-07-23

Supersedes 1.5.0, which was never released.

### Added
- Track more than one Codex or Claude account at the same time, from a new Accounts tab. Both accounts show side by side, so you can watch a personal and a work seat at once. An account is a config directory (`CODEX_HOME` for Codex, `CLAUDE_CONFIG_DIR` for Claude) rather than a token you paste, because each CLI refreshes its own sign-in in place and a copy would stop working within hours. Sign a second account in with `mkdir "<path>"; $env:CODEX_HOME="<path>"; codex login`, point Ceiling at that folder, and it reads the name and plan off the folder itself so there is nothing to type. Adding an account checks the folder first and tells you whose account is in it before you commit.
- Name the account each provider card is reporting by its email, so two accounts of one provider are easy to tell apart. Each account can carry an optional accent color.
- Your currently signed-in account is listed automatically, so adding a second one leaves you with two rather than replacing the first.

### Changed
- Show local activity on the Charts page once per provider, not per account. Token and cost history is scanned from local logs, which record the plan but not the account, so a per-account split of that data was never real. Account-specific usage (the quota bars and resets) stays per-account on the overview, flyout and tray, since that comes from each account's API.
- Balance the usage-period boxes so a provider with one active window and one with two lay out consistently.

### Removed
- The tray icon mode and merge tray icons settings. Neither had any effect: Ceiling has always drawn a single tray icon and nothing read those values.

### Fixed
- Tell you about a reset that happened while Ceiling was closed. These were absorbed silently to avoid announcing stale news as if it just happened, which meant an overnight reset was never mentioned at all. They now say when they actually happened, for example "This happened at 2:00 AM, while Ceiling was closed".
- Fire usage-threshold alerts per account. With two accounts on one provider, a quiet account was clearing a busy account's pending alert on the same refresh, so the warning never fired.
- Read the right Claude sign-in when `CLAUDE_CONFIG_DIR` is set. Ceiling read that folder's history but always took credentials from the default location, so a second Claude profile showed the wrong account's numbers.
- Stop one account showing another's usage. Readings did not record which account they came from, so two accounts shared a single baseline, a transient auth error could substitute the other account's data, and the usage chart fell back to whichever account had data most recently.
- Show every account's usage instead of one replacing the others across the overview, taskbar flyout, tray, charts tabs, provider detail and activity timeline, and drop a removed account's card instead of leaving it on screen.
- Keep both accounts' sign-ins cached when tracking more than one Codex account, instead of each check evicting the other.

## [Ceiling] 1.4.0 - 2026-07-21

### Added
- Tell you when local totals cover more than one subscription plan. Codex records the plan behind each request, so if a machine has been used by more than one plan the Charts page now says so instead of letting the figures read as the signed-in account's. Local logs never record which account produced them, so this reports what was seen and does not guess.

### Fixed
- Count Codex cached input once when working out a model's cache rate. Codex reports cached tokens inside its input count, and adding the cache bucket on top counted them twice, so a model that was really about 97% cached displayed as 49%.
- Include archived Codex sessions in the Charts page, the reset windows, and the estimated API value. Only the active sessions folder was being read there, so archiving a task quietly shrank every total while the older summary still counted it. Expect these figures to rise if you archive.
- Say which window each cache percentage measures. The per-model figure covers 30 days while the token mix above it covers 7, and they can legitimately differ.

## [Ceiling] 1.3.2 - 2026-07-21

### Fixed
- Stop the Today / Yesterday / 30 days buttons on the Charts page from jumping between rows. Selecting a period without a change to report made the card shorter, which removed the page scrollbar, widened the content, and reflowed the header above it. The card now keeps one height across every period and metric, and the heading wraps its own text long before the buttons move.

## [Ceiling] 1.3.1 - 2026-07-21

Supersedes 1.3.0, which was withdrawn before general release.

### Added
- Show estimated API-value dollars beside the token count on every usage period, including each provider's current 5-hour and weekly reset window, so you can see what you have spent since your last reset. Models without a public price stay excluded rather than reading as $0.00.
- Add a seven-day trend to the Estimated API value card, and keep idle providers in its legend, so a single active provider no longer leaves the card looking blank.

### Changed
- Label the Compare cards as rolling windows and state plainly that they put both providers on one shared clock, rather than each provider's own reset boundary. Reset-aligned figures live in each provider's chart drill-in.
- Surface the window that is actually constraining you in the floating bar and the overview tiles. An exhausted window now wins over one that merely reads higher, and an exact tie goes to whichever resets first.

### Fixed
- Restore the Compare tab, which never finished loading and sat on "Comparing local history". The rolling windows it compares had stopped being produced, so it waited for data that never arrived.
- Stop long reset timestamps from running across the neighbouring usage cards. The detail line now wraps inside its own card, and the dollar figure sits on its own line instead of breaking mid-value.
- Stop a freshly reset Claude window from briefly reading as 100% full. Anthropic reports usage as either a fraction or a percentage, and a lone `1` (meaning 1% used) was being read as 100%, which also fired a false "limit reached" notification. The unit is now settled once per response instead of guessed per value.
- Announce a reset within seconds instead of up to five minutes. Ceiling now refreshes as soon as a known reset boundary passes, rather than waiting for the next scheduled poll, and no longer depends on a background window timer that Windows suspends.

## [Ceiling] 1.2.1 - 2026-07-19

### Fixed
- Collapse the "Cost by project" list to the top 8 projects behind a "Show all" toggle, so a long project list no longer pushes the charts far below the fold.

## [Ceiling] 1.2.0 - 2026-07-19

### Added
- Show a concrete "about ~42m left" estimate in Calm mode and on the dashboard, so a running-low window tells you roughly how long you have instead of just flagging it.
- Break down 30-day spend by project, alongside the existing per-model and per-effort views, using the working directory recorded in each session.
- Export a provider's 30-day spend to a CSV in your Downloads folder from the charts view, covering period totals and the per-model, per-effort, and per-project rows.
- Add a cache-only `statusline` command that prints remaining capacity for editor status bars from the last saved snapshot, without waking the app or hitting the network.
- Show a Cursor activity-by-model card from local request logs, framed as activity share rather than tokens or spend.

### Changed
- Leave estimated cost blank for models without a public price everywhere spend is shown, including the new project view and the CSV export, so unpriced usage never reads as $0.00.

### Fixed
- Reset the Codex project attribution when a child or forked session has no working directory of its own, and ignore filesystem roots, so spend is bucketed to the right project.

### Added
- Show a total estimated API-value card that aggregates local usage across Codex and Claude, with Today, Yesterday, and 30-day views, an API value or Tokens metric, a provider ring, and a ranked legend.
- Break down 30-day spend by model, and by Codex reasoning effort, each with a running total and clear "Not priced" rows for models without a public rate.
- Surface pricing coverage (for example, "96% of tokens priced") and name the unpriced models, so estimated totals stay transparent.

### Changed
- Label token-derived dollars as estimated API value, not a bill or subscription spend, across the new cost views.

### Fixed
- Stop counting a child or sub-agent session's replayed parent history, which could inflate Codex token and cost estimates many times over.
- Include archived Codex sessions and de-duplicate rollouts across locations, so 30-day usage is neither under-counted nor double-counted.
- Stop reporting dollars for models without a canonical price (their tokens are still counted), so a period of only unpriced usage no longer reads as $0.00.
- Attribute Codex usage to the real reasoning-effort tier recorded in the session logs instead of guessing from the model name.

## [Ceiling] 1.0.0 - 2026-07-18

First stable release of Ceiling for Windows.

### Added
- Monitor provider-reported capacity and reset windows for Codex, Claude, Cursor, Gemini, and GitHub Copilot from the tray, dashboard, and taskbar-adjacent floating bar.
- Detect confirmed unexpected resets, restored or lifted windows, reset-time shifts, and newly granted banked resets without replaying old events at startup.
- Offer Exact and Calm information modes, taskbar-aware placement, persistent history, charts, and guided first-run provider setup.

### Changed
- Ship an English-only v1 with explicit provider data-source and privacy explanations.
- Treat provider meters as authoritative and show unavailable or not-currently-enforced windows without inventing entitlement data.

### Security
- Protect local credentials with user-scoped DPAPI and current-user NTFS ACLs, restrict the optional PowerToys status pipe, and narrow Tauri command capabilities by window.
- Remove legacy plaintext cookie storage, harden temporary browser database handling, and update the dependency chain for known advisories.

### Fixed
- Deliver confirmed reset notifications through the Windows toast pipeline and report Windows notification blocks accurately.
- Keep the pop-out dashboard, native controls, taskbar placement, chart tooltips, and provider-window presentation stable across supported Windows configurations.

## [Ceiling] 0.43.3 - 2026-07-16

### Security
- Restrict secret files (API keys, manual cookies, and settings) to the current Windows user with a locked-down NTFS ACL, and encrypt them with user-scoped DPAPI without any machine-scope fallback.
- Lock the optional PowerToys status pipe to the current user so other local processes can no longer read usage and cost snapshots.
- Move any credentials left in settings into the dedicated encrypted stores, remove leftover plaintext cookie caches at startup, and wipe temporary cookie databases on every exit path.
- Scope each window to only the commands it needs and allow folder-opening only within Ceiling's own locations.
- Update serde_with to 3.21.0 to resolve GHSA-7gcf-g7xr-8hxj.

## [Ceiling] 0.43.3-beta.2 - 2026-07-14

### Added
- Add private, first-party website analytics for visitors, downloads, and GitHub traffic.
- Add persistent quota history and factual processed-token summaries to Charts.
- Add compact, standard, and detailed floating-bar density presets with automatic contrast.
- Add taskbar-aware floating-bar placement across primary and secondary Windows taskbars.

### Changed
- Replace API-equivalent dollar estimates with processed-token and cache-traffic breakdowns.
- Polish the repository README around the released app and current Windows UI.
- Pin release and CI actions to immutable commits and test the website Worker in CI.
- Rework floating-bar recovery around Windows events instead of repeated z-order writes.

### Fixed
- Align the Tauri JavaScript API and CLI with the patched Rust runtime so signed release builds validate cleanly.
- Keep Codex's regular weekly limit distinct from Codex Spark Weekly so scheduled resets notify reliably.
- Correct malformed chart time labels and keep provider charts responsive while history loads.
- Keep the floating bar above normal taskbar activity without taking focus.
- Keep taskbar placement stable across mixed resolutions, DPI scales, negative coordinates, and taskbar restarts.

## [Ceiling] 0.43.2 - 2026-07-14

### Added
- Add a persistent, taskbar-adjacent capacity strip with provider-aware usage lanes.
- Add reset and capacity-change detection with restrained strip animations and Windows alerts.
- Add dedicated Overview, Activity, Accounts, and Charts surfaces.
- Add first-class setup and credential discovery for Codex, Claude, Cursor, Gemini, and Copilot.
- Add Cursor plan, Auto, and API usage lanes.

### Changed
- Rebrand the Windows app, tray, dashboard, icons, installer, and release assets as Ceiling.
- Rework the Windows UI around a calm, glanceable capacity model with explicit freshness and unavailable states.
- Make the installer and release pipeline independent from Win-CodexBar packaging identity.

### Fixed
- Hide the dashboard to the system tray on minimize and reliably foreground it from the tray icon.
- Update the frontend development dependency chain to patched Vite, Vitest, Babel, form-data, and ws releases.
- Update the Rust dependency chain to patched anyhow, crossbeam-epoch, quick-xml, and quinn-proto releases.
- Prevent Rust test fixtures and debug builds from dispatching real Windows notifications, and add a hard toast burst circuit breaker.
- Require time-separated confirmation for surprise capacity changes and reserve Windows alerts for scheduled or surprise resets only.
- Prevent stale usage history and provider-window swaps from replaying incorrect usage, depletion, restoration, or promotional notifications at startup.
- Avoid notifications for Cursor promotional and on-demand capacity pools.
- Refresh current Gemini CLI credentials and detect eligible Claude Desktop sessions without exposing secrets.

## [Windows] 0.42.0 - 2026-07-12

### Added
- Add the Wayfinder provider.
- Add opt-in local and SSH Agent Sessions for Codex and Claude.
- Add predictive pace warnings and provider/window-specific usage thresholds.
- Add GPT-5.6 Sol, Terra, and Luna pricing and model aliases.
- Show Claude model-scoped weekly quotas from OAuth, web, and CLI sources.

### Changed
- Reorganize Settings and restore Providers as a dedicated tab.
- Keep every Settings tab at the same window size.
- Improve token-cost pricing coverage, freshness, and refresh coalescing.

### Fixed
- Keep refresh intervals anchored and provider cards synchronized with completed work.
- Avoid quota refreshes for visual-only settings and provider reorder changes.
- Improve Antigravity detection, Gemini paid-tier labels, Ollama authentication, and other provider parsing.
- Eliminate the duplicate FloatBar cost control and keep local-cost display opt-in.

---

## [Windows] 0.41.3 - 2026-07-11

### Added
- Add local cost summaries to FloatBar.
- Add a PowerToys Command Palette status pipe.

### Fixed
- Keep FloatBar topmost without repeatedly stealing focus.
- Preserve the OpenCode Go workspace override during authentication.
- Open Cursor's usage dashboard at the correct URL.
- Align the popup correctly with a side-mounted Windows taskbar (#159).
- Correct Codex daily usage reporting (#153, thanks @0reki).

---

## [Windows] 0.41.2 - 2026-07-08

### Added
- Add Antigravity `agy` CLI alias setup guidance.
- Add Traditional Chinese (Taiwan) localization.

### Changed
- Add repository interaction guardrails and switch the tray panel to a masonry card layout.

---

## [Windows] 0.41.1 - 2026-07-08

### Fixed
- Localize the tray panel and native tray menu for Japanese, Chinese (Simplified), Korean, and Spanish.
- Add missing `es-MX` UI key translations.
- Stop baking the UI language into cached provider snapshots so locale changes remain independent of provider data.
- Localize the native tray menu proof harness.

---

## [Windows] 0.41.0 - 2026-07-07

### Changed
- Port scoped upstream CodexBar 0.41.0 Rust/provider updates into the Windows/Tauri app.

### Fixed
- Fix the Windows tray/background launch auto-popup regression from #129 and fix Kimi auth cookie fallback.

---

## [Windows] 0.38.3 - 2026-07-06

### Fixed
- Fix NanoGPT usage parsing when the API omits the monthly usage block.

---

## [Windows] 0.38.2 - 2026-07-05

### Fixed
- Fix tray flyout flicker/hide when opened from the Windows tray overflow.

---

## [Windows] 0.38.1 - 2026-07-04

### Changed
- Bump the Windows/Tauri release version to 0.38.1 after merging the tray flyout, vertical-taskbar placement, Claude OAuth refresh, Claude cost, README language, and repo cleanup fixes.

### Fixed
- Port upstream 0.38.1 parser hardening for OpenAI API non-finite cost values, OpenCode reset timestamps, and z.ai BigModel CN quota responses without optional messages.

---

## [Windows] 0.38.0 - 2026-07-03

### Added
- Port upstream v0.38.0 provider support for CrossModel, Qoder, and Sakana AI into the Windows/Tauri app.
- Add Tauri provider icons, provider catalog metadata, manual cookie support for Qoder/Sakana, and saved API-key settings for CrossModel.

### Fixed
- Accept current Command Code `commandcode_prod` manual cookie headers.

---

## [Windows] 0.37.6 - 2026-07-02

### Added
- Add window mode with a taskbar-visible PopOut window, custom title bar controls, maximize/restore behavior, and display scaling.
- Add Mexican Spanish (`es-MX`) locale support with a centralized backend language catalog.

### Fixed
- Ship the installed console CLI as `codexbar-cli.exe` and verify it is a real console-subsystem binary with redirected stdout.
- Keep browser cookie imports scoped to exact provider domains and validate imported cookie header length before saving.
- Validate provider workspace/base URL extras in Rust before persistence so saved credentials cannot be retargeted to unsafe endpoints.
- Allow clearing the global shortcut setting without attempting to register an empty shortcut.
- Mask Unicode API keys without slicing through UTF-8 boundaries.
- Resolve automatic theme mode from the current OS light/dark preference.
- Fix the cost scanner token-count regression test so it does not age out of the 30-day scan window.

### Changed
- Harden CI and release workflows with pinned GitHub Action SHAs, read-only build permissions, and separate release publishing jobs.
- Package `codexbar.exe` as the desktop app, `codexbar-cli.exe` as the console CLI, and `codexbar-desktop.exe` as a compatibility alias.

---
## [Windows] 0.37.5 - 2026-06-27

### Fixed
- Fix Windows desktop startup paths that could leave CodexBar running with only the tiny internal Tauri shell window visible.
- Reopen the tray panel for normal or blank-argument desktop launches unless **Start Minimized** is enabled.
- Recover startup tray reveals that remain hidden or stuck at a tiny shell-window size.

---

## [Windows] 0.37.4 - 2026-06-24

### Changed
- Remove stale release scripts, unused fetch planning code, the fake Synthetic provider, and dead settings toggles.

---

## [Windows] 0.33.2 - 2026-06-12

### Fixed
- Hide the tray panel when it loses focus, matching normal tray-popover behavior.
- Allow Escape to dismiss the tray panel without quitting the app.
- Prevent the tray icon click that caused a blur-dismiss from immediately reopening the panel.

---

## [Windows] 0.33.1 - 2026-06-11

### Fixed
- Show GitHub Copilot over-budget quota values when GitHub reports negative remaining quota, such as displaying `115% used` instead of clamping to `100%`.
- Keep Copilot progress bars visually capped at full width while preserving the true overage percentage in tray, pop-out, provider sidebar, and settings details.

---

## [Windows] 0.33.0 - 2026-06-11

### Added
- Add Japanese as a selectable interface language in the Tauri Settings UI.

### Changed
- Port upstream CodexBar 0.33.0 provider and cost-accounting fixes into the Windows/Tauri Rust backend.
- Route provider HTTP clients through a shared same-origin redirect policy so credentialed requests do not follow cross-origin redirects with provider auth context.
- Update Claude local cost pricing for Fable 5, Opus 4.6, Sonnet 4.6, and 1-hour cache writes.

### Fixed
- Avoid showing Doubao API keys as falsely exhausted when Ark returns successful zero-remaining request-limit headers that are not reliable quota state.
- Preserve existing Copilot unlimited-chat and Antigravity untracked-quota behavior from the upstream 0.33.0 cycle.

---

## [Windows] 0.32.9 - 2026-06-11

### Fixed
- Apply Display settings changes to the native tray immediately after saving, including tray metric mode, highest-usage selection, percent icon mode, provider metric preferences, and enabled-provider changes.
- Make the **Show provider icons** setting affect the tray and pop-out provider switcher grids instead of only being stored.
- Make **Show percent in tray** render a real numeric tray icon.
- Make **Tray icon mode** affect native tray status rows by switching between a single summary row and per-provider rows.
- Shorten native tray tooltip reset text to relative countdowns such as `resets in 2h 05m` and bound long tooltip lines so Windows does not trim provider status text mid-line.

---

## [Windows] 0.32.8 - 2026-06-09

### Changed
- Install `codexbar.exe` as the tray app and `codexbar-cli.exe` as the console CLI so Start Menu shortcuts launch the desktop UI while terminal diagnostics print real output.
- Build the console CLI during every Windows release packaging run.

### Fixed
- Run provider auto-refresh from the Tauri backend even when the tray panel is closed and the floating bar is disabled.
- Keep `refresh_interval_secs = 0` as manual-only and prevent overlapping background refreshes.
- Extend Windows smoke install validation to prove installed CLI `--version` and `--help` output.

---

## [Windows] 0.32.7 - 2026-06-08

### Added
- Expand Alibaba Coding Plan support with selectable Singapore, US, Germany, Hong Kong, and China Mainland regions.

### Changed
- Route Alibaba Coding Plan cookies, dashboard links, gateway requests, and SEC_TOKEN caching through a canonical region model so region behavior stays consistent.

### Fixed
- Fix Cursor usage percentages by trusting Cursor's `totalPercentUsed`, `autoPercentUsed`, and `apiPercentUsed` fields as 0-100 percentages instead of recalculating or multiplying them.
- Keep Cursor bonus-credit breakdown totals from distorting fallback percentage calculations.

---

## [Windows] 0.32.6 - 2026-06-05

### Changed
- Reveal the Windows tray panel only after the frontend completes its first layout pass, avoiding the blank backing-frame flash on tray startup.
- Lazy-load heavier secondary surfaces so first tray activation does not compete with Settings, Pop Out, or Floating Bar module startup.
- Limit concurrent provider refreshes and emit provider-updated events after releasing the provider-cache lock to reduce refresh contention.
- Keep tray and pop-out provider ordering aligned with the configured provider catalog order.

### Fixed
- Restore the full bootstrap bridge contract for surface modes, commands, and events instead of exposing test-only descriptors.
- Keep dense tray overview layout visible while provider data is still loading, using stable placeholders instead of waiting indefinitely for the first providers to fetch.
- Handle surface-state mutex errors at the Tauri command boundary instead of panicking.
- Fix NanoGPT monthly-only usage parsing.

---

## [Windows] 0.32.5 - 2026-06-02

### Fixed
- Treat GitHub Copilot Business token-based billing zero-entitlement quota rows as unavailable instead of showing misleading `0% used` usage.
- Keep percent-only Copilot quota snapshots and fully consumed positive-entitlement quotas working while dropping only explicit zero-entitlement placeholders.
- Prioritize OpenAI Web login and Cloudflare blocking states over public-route detection so blocked dashboard responses do not get misclassified.

---

## [Windows] 0.32.4 - 2026-06-02

### Fixed
- Fix OpenRouter credits fetching by routing requests to the canonical `/api/v1/credits` endpoint instead of the broken `/api/v1/auth/credits` path.
- Align OpenRouter key introspection with the upstream `/api/v1/key` endpoint and add regression coverage for both endpoint URLs.

---

## [Windows] 0.32.3 - 2026-06-01

### Fixed
- Detect modern Chrome/Edge `v20` App-Bound encrypted cookies during browser import so Codex/ChatGPT imports no longer misreport protected signed-in sessions as missing cookies.
- Replace the outdated Chromium cookie-import guidance with a clearer manual-cookie or Firefox fallback when Windows browser encryption blocks direct import.

---

## [Windows] 0.31.1 - 2026-05-30

### Fixed
- Fix Antigravity usage on Windows when the local language server binds its API to a random listening port instead of a port near `--extension_server_port`.
- Prefer the Antigravity language-server process's actual listening ports before falling back to heuristic API port probes.

---

## [Windows] 0.31.0 - 2026-05-29

### Added
- Support AWS Bedrock usage through named AWS CLI profiles, including SSO, assume-role, and credential-process profiles that `aws configure export-credentials` can resolve.
- Show Codex Spark 5-hour and weekly quota lanes from ChatGPT/Codex `additional_rate_limits` payloads.

### Changed
- Port upstream CodexBar 0.31.0 provider behavior into the Windows/Tauri Rust backend while keeping macOS-only AppKit menu and Homebrew changes out of the Windows shell.
- Make local Codex/Claude chart scans cancellation-aware so repeated chart refreshes stop obsolete JSONL scans sooner.
- Document Bedrock profile credentials in the provider settings help text.

### Fixed
- Hide Claude's obsolete Design quota lane while preserving the remaining OAuth apps and Daily Routines usage lanes.

---

## [Windows] 0.30.4 - 2026-05-29

### Added
- Add `codexbar diagnose`, a generic safe provider diagnostic export that reports provider/source/config/fetch health without exposing cookies, tokens, account emails, or raw secrets.

### Changed
- Port upstream CodexBar 0.30.1 provider diagnostics behavior to the Rust CLI while omitting macOS-only AppKit status-item handling that has no Windows Tauri equivalent.
- Add trailing breathing room to Providers settings sidebar rows so row controls do not crowd the scrollbar.

### Fixed
- Treat Claude OAuth usage HTTP 429s as rate limits, preserve cached credentials, and back off repeated background retries.
- Reopen the tray panel from Windows shortcut/tray activation when the app is hidden, keep Claude usage on the current OAuth API path when no manual cookie is configured, and avoid flashing console windows during Windows CLI path probes.

---

## [Windows] 0.29.0 - 2026-05-24

### Added
- Port upstream CodexBar 0.29 Alibaba Token Plan support to the Rust/Tauri provider registry, protected token-account storage, provider icon registry, and CLI aliases.
- Show OpenCode and OpenCode Go renewal dates as a separate **Renews** usage window when their usage payloads expose `renewAt` / `renew_at`.
- Split local Codex cost output into standard vs fast/priority buckets when local session logs expose fast model naming.

### Fixed
- Preserve the upstream 0.29 quote-handling intent through the existing Rust secret cleanup paths while keeping provider tokens out of frontend state and logs.

---

## [Windows] 0.28.0 - 2026-05-24

### Added
- Port upstream CodexBar 0.28 Azure OpenAI and T3 Chat provider support to the Windows/Tauri provider registry, Settings UI metadata, protected credential storage, and CLI aliases.
- Add Ollama API-key support while keeping browser-cookie usage available as the Web source.

### Fixed
- Harden OpenAI dashboard account scraping and MiniMax billing aggregation for additional upstream response shapes.

---

## [Windows] 0.27.4 - 2026-05-23

### Added
- Add Floating Bar light-background mode for better contrast on bright desktops.
- Show localized provider reset timing in Floating Bar pill tooltips.

### Fixed
- Apply the Floating Bar opacity slider through the rendered bar so visual opacity updates reliably.
- Keep the Floating Bar release build compiling on the repo's active Tauri 2.10 API surface.

---

## [Windows] 0.27.3 - 2026-05-21

### Fixed
- Prefer Claude browser-session usage over Claude OAuth in Auto mode so the app follows the same settings-page endpoint as `claude.ai/settings/usage`.
- Use Claude's `lastActiveOrg` cookie or account memberships before falling back to the organizations list, which keeps multi-org accounts aligned with the active Claude web session.
- Parse Claude `seven_day_oauth_apps` and embedded `extra_usage` payloads from the web usage response.

---

## [Windows] 0.27.2 - 2026-05-20

### Added
- Add GitHub device-code sign-in for Copilot in the Tauri provider settings, storing the OAuth token as a protected Copilot token account.
- Reuse `gh auth token` as a Copilot auth fallback so existing GitHub CLI logins can power Copilot usage without pasting a token.

### Fixed
- Parse Copilot plan usage across paid `premium_interactions` / `chat` snapshots and free-plan `monthly_quotas` / `limited_user_quotas` responses.
- Show Copilot as OAuth-backed in the Providers UI while keeping the legacy manual token path as an optional fallback.

---

## [Windows] 0.27.1 - 2026-05-19

### Added
- Complete the upstream CodexBar 0.27 provider port for Windows/Tauri by adding Grok billing support, Claude Admin API usage, OpenAI Admin API usage with legacy credit-balance fallback, MiniMax billing summaries, OpenCode Go Zen balance display, and Kiro overage usage/cost parsing.
- Add Grok across the Rust provider registry, credential migration, token-account support decisions, Settings provider catalog, Tauri provider unions, provider icon registry, and chart colors.
- Add `codexbar serve` for loopback `/health`, `/usage`, and `/cost` JSON, with loopback Host-header validation.
- Add the upstream-compatible `--all-accounts` CLI flag surface.

### Fixed
- Correct the Windows 0.27 line so the release reflects the full portable upstream provider/CLI changes instead of only the API-key quota-provider subset.
- Update README provider counts and v0.27 notes to describe the full Windows/Tauri port.

---

## [Windows] 0.27.0 - 2026-05-19

### Added
- Port upstream CodexBar 0.27 API quota providers for ElevenLabs subscription credits, Deepgram project usage, GroqCloud Enterprise Prometheus metrics, and LLM Proxy quota-stats.
- Add the v0.27 provider wiring across the Rust registry, Settings API-key catalog, Tauri provider icons, chart colors, dashboard/status links, and frontend provider unions.
- Add `codexbar config providers`, `codexbar config enable`, `codexbar config disable`, and `codexbar config set-api-key` for scriptable provider setup.

### Fixed
- Keep API-key provider refreshes on the Windows/Tauri API source path and add deterministic parser coverage for the new providers.

---

## [Windows] 0.26.3 - 2026-05-17

### Fixed
- Fix DeepSeek refresh in the desktop app when an API key is configured by keeping API-key providers on the automatic API source path instead of falling back to unsupported CLI mode.
- Stop writing raw CLI argument values to startup diagnostics and use per-process launch log files to avoid leaking tokens or mixing concurrent launch logs.

---

## [Windows] 0.26.2 - 2026-05-16

### Added
- Add an optional always-on-top Floating Bar that shows remaining provider capacity in a compact transparent strip.
- Add Floating Bar display settings for enablement, horizontal/vertical orientation, opacity, and click-through overlay mode.
- Add native tray menu support for toggling the Floating Bar while keeping the tray check state in sync with Settings changes.

### Fixed
- Make the Settings tab's Quit button close only the Settings window so the tray service keeps running.
- Keep tray panel, pop-out, and native tray menu Quit actions on the app-level exit path.
- Keep Floating Bar updates live when provider enablement, refresh cadence, or usage thresholds change.
- Avoid Windows Tauri deadlocks by opening the Floating Bar from async command paths.

---

## [Windows] 0.26.1 - 2026-05-15

### Fixed
- Preserve Moonshot / Kimi API compatibility across international and China-region API keys by trying both endpoints when `MOONSHOT_API_REGION` is unset.
- Keep explicit `MOONSHOT_API_REGION=international` and `MOONSHOT_API_REGION=china` region pinning for users who want a single endpoint.

---

## [Windows] 0.26.0 - 2026-05-15

### Added
- Port upstream CodexBar 0.26 AWS Bedrock monthly spend tracking into the Windows/Tauri Rust backend using AWS Cost Explorer and SigV4 request signing.
- Add Bedrock provider metadata, CLI aliases, Settings provider catalog support, frontend provider types, source hints, and provider icon registry entry.
- Add OpenRouter daily, weekly, and monthly API-key spend windows from the `/api/v1/auth/key` endpoint.
- Add Moonshot / Kimi API balance parsing for international and China API regions, voucher balance, cash balance, and deficit state.

### Changed
- Stop shipping `WebView2Loader.dll` in Windows installer and portable packages because MSVC release builds statically link the WebView2 loader.
- Restore portable release packaging to a standalone `CodexBar-<version>-portable.exe` asset instead of a zip bundle.
- Update app, CLI, package, Tauri, and release metadata to 0.26.0 for the Windows artifact release.

### Notes
- Upstream 0.26 also includes several macOS/Swift-only menu, Sparkle, localization, and native settings changes. Those code paths do not exist in Win-CodexBar's Tauri/Rust shell.

---

## [Windows] 0.25.1 - 2026-05-11

### Changed
- Align the Windows/Tauri release with upstream CodexBar 0.25.1 after reviewing the upstream patch set.
- Bump app, CLI, package, Tauri, and release metadata to 0.25.1 for the follow-up Windows artifact release.

### Notes
- Upstream 0.25.1 fixes macOS SwiftPM localization bundle lookup, macOS Keychain cache prompt churn, Pi session cost cache migration, Swift concurrency annotations, and standalone Swift CLI archive version fallback. Those code paths do not exist in the Windows/Tauri port, so no runtime Rust/Tauri logic change was required beyond the release alignment.

---

## [Windows] 0.25.0 - 2026-05-11

### Added
- Port upstream CodexBar 0.25 provider support for Manus, Xiaomi MiMo, Doubao, Command Code, Crof, StepFun, Venice, and OpenAI API balance into the Windows/Tauri app.
- Add v0.25 providers to the Rust provider registry, Settings provider list, credential/API-key catalog, CLI aliases, cookie/token-account handling, and provider icon registry.
- Add credit, request, refresh-credit, token-plan, purchased-credit, DIEM/USD balance, and OpenAI API credit-grants usage snapshots.

### Changed
- Update the provider catalog, CLI metadata, frontend provider unions, and release docs for 40 supported providers.

---

## [Windows] 0.24.0 - 2026-05-10

### Added
- Port upstream CodexBar 0.24 provider support for Codebuff, DeepSeek, and Windsurf into the Windows/Tauri app.
- Add Codebuff and DeepSeek API-key setup to Preferences, including provider icons, chart colors, CLI aliases, and release metadata.
- Add Windsurf local cached-plan usage reading from the Windows application data path.

### Changed
- Update the provider catalog, CLI help text, credential metadata, frontend provider unions, and release docs for 32 supported providers.

---

## [Windows] 0.23.11 - 2026-05-10

### Fixed
- Handle Claude Web usage payloads that include overlapping design or routines alias fields without failing with a duplicate-field parse error.
- Keep Claude Web parse diagnostics useful without exposing raw response bodies in user-facing errors or logs.

---

## [Windows] 0.23.10 - 2026-05-06

### Fixed
- Route active Claude OAuth token accounts through OAuth mode and pass the selected token directly into the Claude OAuth fetcher.
- Keep Claude `sessionKey` token accounts on the web/cookie path instead of confusing them with OAuth tokens.
- Report OAuth, Web, and CLI failures together in Claude Auto mode so a final CLI parse error no longer hides earlier token or cookie failures.

---

## [Windows] 0.23.7 - 2026-05-03

### Fixed
- Parse Claude CLI's exhausted `You've hit your limit · resets ...` short form as full session usage instead of reporting `Claude CLI did not return usage data`.
- Make Claude CLI usage parsing more tolerant of compact labels, decimal percentages, and remaining/available wording.
- Keep weekly reset lines from being promoted into the session reset when the session section has no reset.

### Security
- Re-enable the Tauri content security policy and disable global Tauri injection.
- Narrow the default Tauri capability permissions to the event, window, and global shortcut APIs the frontend actually uses.
- Harden external URL opening by validating web URLs and avoiding `cmd /c start` on Windows.

---

## [Windows] 0.23.5 - 2026-04-29

### Added
- Add safe diagnostics and credential storage status reporting without exposing secret values.
- Add a Windows installer smoke-test script for silent install, installed-file, registry, shortcut, and uninstall validation.

### Changed
- Reuse fresh provider refresh results during startup and panel opening to reduce avoidable provider fetches.

### Fixed
- Redact secret-like values from provider refresh errors before they cross the Tauri bridge.
- Re-verify downloaded installer SHA-256 hashes immediately before applying an update.
- Harden desktop command inputs for provider IDs, credential values, cookie source values, region values, token accounts, and filesystem paths.

---

## [Windows] 0.23.4 - 2026-04-29

### Security
- Default browser-cookie usage to manual mode so provider refreshes no longer read and decrypt browser cookie stores unless the user explicitly selects Automatic or imports cookies.
- Respect manual/off cookie-source settings when building provider fetch contexts, reducing behavior-based antivirus triggers around DPAPI browser-cookie access.
- Save local secret-bearing files through a secure-file wrapper; Windows writes are protected with DPAPI while existing plaintext files remain readable for migration.
- Redact raw provider response bodies and browser cookie-store paths from routine diagnostic logs.

---

## [Windows] 0.23.3 - 2026-04-29

### Fixed
- Ship `WebView2Loader.dll` beside `codexbar.exe` in the Windows installer so clean installs can launch the Tauri shell.
- Replace the standalone portable executable release asset with `CodexBar-<version>-portable.zip`, which includes both `codexbar.exe` and `WebView2Loader.dll`.
- Add release workflow checks that fail the build when the WebView2 runtime sidecar is missing.

### Superseded
- Later Windows MSVC builds statically link the WebView2 loader, so release packaging no longer needs to ship `WebView2Loader.dll` beside `codexbar.exe`.

---

## [Windows] 0.23.2 - 2026-04-28

### Fixed
- Accept a raw `__Secure-session` value for Ollama Cloud manual cookies instead of requiring a full `Cookie` header.
- Normalize Ollama token-account entries the same way, so saved accounts can use either raw `__Secure-session` values or full cookie headers.
- Clarify the Ollama cookie placeholder in the desktop settings UI.

---

## [Windows] 0.23.1 — 2026-04-26

### Fixed
- Add the provider Settings picker for the tray/menu bar metric so the Windows frontend can choose session, weekly, model-specific, tertiary, average, or Cursor extra-usage display modes.
- Make the tray icon respect per-provider metric preferences, including Cursor on-demand budget and legacy credits settings.

---

## [Windows] 0.23.0 — 2026-04-26

### Upstream 0.23 Parity
- Add Mistral usage support with monthly spend parsing from the Mistral Admin billing API, browser-cookie/manual-cookie auth, token-account storage, and provider branding.
- Add Claude Designs and Daily Routines usage windows when Claude OAuth/Web quota payloads include those limits.
- Add GPT-5.5 and GPT-5.5 Pro pricing for local Codex cost scanning.
- Prefer Cursor on-demand budget data for the extra/monthly cost metric when Cursor returns it.

### Windows Release
- Bump the Tauri desktop and shared Rust crate to `0.23.0`.
- Keep macOS-only upstream 0.23 work out of the Windows port: WidgetKit metadata, Sparkle appcast, AppKit menu sizing, and full-screen confetti are not applicable here.

---

## [Windows] 0.22.1 — 2026-04-24

### Fixed
- Stabilize the tray panel height measurement so provider refreshes and provider selection no longer visibly jump or re-anchor the popup.
- Close the tray panel when opening Settings or About so those windows can take focus cleanly.
- Keep the Windows DWM helper clean under `cargo clippy --all-targets -- -D warnings`.

---

## [Windows] 0.22.0 — 2026-04-23

### New Providers
- Perplexity: cookie-based credits tracking (recurring/bonus/purchased), Pro/Max plan detection
- Abacus AI: cookie-based compute points + billing tier fetch
- OpenCode Go: cookie-based workspace usage (rolling/weekly/monthly windows)
- Kilo: API-key tRPC batch (env/keyring/auth.json), credit blocks + Kilo Pass

### Provider Updates (upstream 0.18–0.22 parity)
- Claude: broader CLI lookup (Volta, fnm, npm-global), status page URL fix
- Codex: Pro Lite/Go/Quorum/K12 plan types, dashboard URL, weekly-only rate limits
- Cursor: defensive JSON parsing with text fallback
- Synthetic: 3-slot quota (5-hour, weekly, search limits)
- Antigravity: extension_server_csrf_token extraction and fallback probing
- z.ai: dual TOKENS_LIMIT (weekly + 5-hour session), TIME_LIMIT, plan name
- Ollama: validate session cookie names
- OpenCode: expanded percent/reset key variants, absolute resetAt support
- Alibaba: region-aware endpoints (international/China), multi-domain cookies
- Copilot: verification_uri_complete for pre-filled device login URL
- Gemini: OAuth credential discovery from CLI paths (Homebrew/npm/Nix/Bun/Volta)

### Pricing & Models
- Fix stale GPT-5.4/5.4-mini/5.4-nano pricing
- Add 10 new Codex models (gpt-5-mini, gpt-5-nano, gpt-5-pro, gpt-5.1-codex, etc.)
- Add Claude Opus 4.7 and Claude Sonnet 4.6 pricing
- Add displayLabel field to CodexPricing (for Research Preview tags)

### UI
- Add keyboard shortcuts: Ctrl+R (Refresh), Ctrl+, (Settings), Ctrl+Q (Quit)
- Show shortcut hints in footer menu items
- Update PopOutPanel shortcuts from macOS ⌘ to Windows Ctrl+
- Fix settings window resize (preserve WS_THICKFRAME in DWM caption hack)
- Fix async race conditions on provider switching (stale response guards)
- Fix error visibility in API key section
- Fix GDI brush leak in DWM dark caption

### Repo Cleanup
- Remove legacy egui shell (22,539 lines of dead code)
- Rewrite README with extra-docs split (WSL, Building, Cookies)
- Fresh Windows screenshots
- Fix CI target paths for workspace layout
- Release workflow now builds Tauri app as codexbar.exe
- Add frontend CI job, Rust/npm caching, Dependabot

---

## [Windows] 1.0.2 — 2026-01-24

### UI Redesign
- Redesign main UI with 4-column grid layout for provider tabs
- Replace amber progress bars with blue color scheme
- Add section headers with chevron indicators
- Increase font sizes across all tiers for better readability
- Disable window state persistence to prevent size corruption

### Settings Page
- Complete redesign with "precision calm" aesthetic
- Underline-style tab navigation
- Settings cards with grouped settings and dividers
- Left accent bars on API key cards for status indication
- Reusable helper components for consistent styling

### New Provider
- Add JetBrains AI provider support with usage tracking
- Support aliases: jetbrains, jetbrains-ai, intellij
- Add JetBrains icon and brand color to theme

### Housekeeping
- Remove development screenshots from repository

---

## 0.18.0 — Unreleased
### Providers
- Claude: harden Windows CLI detection, prefer `.cmd` wrappers on PATH, and surface clearer startup errors for Git Bash / PowerShell wrapper failures.
- OpenCode: add web usage provider with workspace override + Chrome-first cookie import (#188). Thanks @anthnykr!
- Providers: cache browser cookies on disk (per provider) and show cached source/time in settings.
- Vertex AI: add provider with quota-based usage from gcloud ADC. Thanks @bahag-chaurasiak!
- Vertex AI: token costs are shown via the Claude provider (same local logs).
- Vertex AI: harden quota usage parsing for edge-case responses.
- Kiro: add CLI-based usage provider via kiro-cli. Thanks @neror!
- Kiro: clean up provider wiring and show plan name in the menu.
- Augment: add provider with browser-cookie usage tracking.
- Cursor: support legacy request-based plans and show individual on-demand usage (#125) — thanks @vltansky
- Cursor: avoid Intel crash when opening login and harden WebKit teardown. Thanks @meghanto!
- Cursor: load stored session cookies before reads to make relaunches deterministic.
- Codex/Claude/Cursor/Factory/MiniMax: cookie sources now include Manual (paste a Cookie header) in addition to Automatic.
- Codex/Claude/Cursor/Factory/MiniMax: skip cookie imports from browsers without usable cookie stores (profile/cookie DB) to avoid unnecessary Keychain prompts.
- Claude: fix OAuth “Extra usage” spend/limit units when the API returns minor currency units (#97).
- Usage formatting: fix currency parsing/formatting on non-US locales (e.g., pt-BR). Thanks @mneves75!
- Antigravity: compile Windows probe regexes once instead of rebuilding them on each scan.

### Preferences & UI
- Windows: open the main window automatically when tray startup is unavailable, and support `CODEXBAR_START_VISIBLE` for proof/automation flows.
- Preferences: move “Access OpenAI via web” into Providers → Codex.
- Preferences: add usage source pickers for Codex + Claude with auto fallback.
- Preferences: add cookie source pickers with contextual helper text for the selected mode.
- Preferences: add debug switch to disable Keychain access and hide cookie-based web options.
- Preferences: add per-provider menu bar metric picker (#185) — thanks @HaukeSchnau
- Preferences: tighten provider rows (inline pickers, compact layout, inline refresh + auto-source status).
- Preferences: remove the “experimental” label from Antigravity.
- Menu bar: fix combined loading indicator flicker during loading animation (incl. debug replay).
- Menu bar: prevent blink updates from clobbering the loading animation.

### Menu
- Menu: add a toggle to show reset times as absolute clock values (instead of countdowns).
- Menu: show an “Open Terminal” action when Claude OAuth fails.
- Menu: add “Hide personal information” toggle and redact emails in menu UI (#137). Thanks @t3dotgg!
- Menu: reduce provider-switch flicker and avoid redundant menu card sizing for faster opens (#132). Thanks @ibehnam!

### CLI
- CLI: respect the reset time display setting.

### Dev & Tests
- Windows: switch eframe from `glow` to `wgpu` to avoid legacy OpenGL renderer issues in the VM.
- Dev: ignore VM proof screenshots and throwaway launcher scripts in git.
- Browser detection: remove an unused `find_browser_with_cookies` stub.
- Dev: move Chromium profile discovery into SweetCookieKit (adds Helium net.imput.helium). Thanks @hhushhas!
- Dev: bump SweetCookieKit to 0.2.0.
- Dev: migrate stored Keychain items to reduce rebuild prompts.
- Tests: expand Kiro CLI coverage.
- Tests: stabilize Claude PTY integration cleanup and reset CLI sessions after probes.
- Tests: kill leaked codex app-server after tests.
- Tests: add regression coverage for merged loading icon layout stability.
- Build: stabilize Swift test runtime.

## 0.17.0 — 2025-12-31
- New providers: MiniMax.
- Keychain: show a preflight explanation before macOS prompts for OAuth tokens or cookie decryption.
- Providers: defer z.ai + Copilot Keychain reads until the user interacts with the token field.
- Menu bar: avoid status item menu reattachment and layout flips during refresh to reduce icon flicker.
- Dev: align SweetCookieKit local-storage tests with Swift Testing.
- Charts: align hover selection bands with visible bars in credits + usage breakdown history.
- About: fix website link in the About panel. Thanks @felipeorlando!

## 0.16.1 — 2025-12-29
- Menu: reduce layout thrash when opening menus and sizing charts. Thanks @ibehnam!
- Packaging: default release notarization builds universal (arm64 + x86_64) zip.
- OpenAI web: reduce idle CPU by suspending cached WebViews when not scraping. Thanks @douglascamata!
- Icons: switch provider brand icons to SVGs for sharper rendering. Thanks @vandamd!

## 0.16.0 — 2025-12-29
- Menu bar: optional “percent mode” (provider brand icons + percentage labels) via Advanced toggle.
- CLI: add `codexbar cost` to print local cost usage (text/JSON) for Codex + Claude.
- Cost: align local cost scanner with ccusage; stabilize parsing/decoding and handle large JSONL lines.
- Claude: skip pricing for unknown models (tokens still tracked) to avoid hard-coded legacy prices.
- Performance: reduce menu bar CPU usage by caching morph icons, skipping redundant status-item updates, and caching provider enablement/order during animations.
- Menu: improve provider switcher hover contrast in light mode.
- Icons: refresh Droid + Claude brand assets to better match menu sizing.
- CI: avoid interactive login-shell probes to reduce noisy “CLI missing” errors.

## 0.15.3 — 2025-12-28
- Codex: default to OAuth usage API (ChatGPT backend) with CLI-only override in Debug.
- Codex: map OAuth credits balance directly, avoiding web fallback for credits.
- Preferences: add optional “Access OpenAI via web” toggle and show blended source labels when web extras are active.
- Copilot: replace blocking auth wait dialog with a non-modal sheet to avoid stuck login.

## 0.15.2 — 2025-12-28
- Copilot: fix device-flow waiting modal to close reliably after auth (and avoid stuck waits).
- Packaging: include the KeyboardShortcuts resource bundle to prevent Settings → Keyboard shortcut crashes in packaged builds.

## 0.15.1 — 2025-12-28
- Preferences: fix provider API key fields reusing the wrong input when switching rows.
- Preferences: avoid Advanced tab crash when opening settings.

## 0.15.0 — 2025-12-28
- New providers: Droid (Factory), Cursor, z.ai, Copilot.
- macOS: CodexBar now supports Intel Macs (x86_64 builds + Sonoma fallbacks). Thanks @epoyraz!
- Droid (Factory): new provider with Standard + Premium usage via browser cookies, plus dashboard + status links. Thanks @shashank-factory!
- Menu: allow multi-line error messages in the provider subtitle (up to 4 lines).
- Menu: fix subtitle sizing for multi-line error states.
- Menu: avoid clipping on multi-line error subtitles.
- Menu: widen the menu card when 7+ providers are enabled.
- Providers: Codex, Claude Code, Cursor, Gemini, Antigravity, z.ai.
- Gemini: switch plan detection to loadCodeAssist tier lookup (Paid/Workspace/Free/Legacy). Thanks @381181295!
- Codex: OpenAI web dashboard is now the primary source for usage + credits; CLI fallback only when no matching cookies exist.
- Claude: prefer OAuth when credentials exist; fall back to web cookies or CLI (thanks @ibehnam).
- CLI: replace `--web`/`--claude-source` with `--source` (auto/web/cli/oauth); auto falls back only when cookies are missing.
- Homebrew: cask now installs the `codexbar` CLI symlink. Thanks @dalisoft!
- Cursor: add new usage provider with browser cookie auth (cursor.com + cursor.sh), on-demand bar support, and dashboard access.
- Cursor: keep stored sessions on transient failures; clear only on invalid auth.
- z.ai: new provider support with Tokens + MCP usage bars and MCP details submenu; API token now lives in Preferences (stored in Keychain); usage bars respect the show-used toggle. Thanks @uwe-schwarz for the initial work!
- Copilot: new GitHub Copilot provider with device flow login plus Premium + Chat usage bars (including CLI support). Thanks @roshan-c!
- Preferences: fix Advanced Display checkboxes and move the Quit button to the bottom of General.
- Preferences: hide “Augment Claude via web” unless Claude usage source is CLI; rename the cost toggle to “Show cost summary”.
- Preferences: add an Advanced toggle to show/hide optional Codex Credits + Claude Extra usage sections (on by default).
- Widgets: add a new “CodexBar Switcher” widget that lets you switch providers and remember the selection.
- Menu: provider switcher now uses crisp brand icons with equal-width segments and a per-provider usage indicator.
- Menu: tighten provider switcher sizing and increase spacing between label and weekly indicator bar.
- Menu: provider switcher no longer forces a wider menu when many providers are enabled; segments clamp to the menu width.
- Menu: provider switcher now aligns to the same horizontal padding grid as the menu cards when space allows.
- Dev: `compile_and_run.sh` now force-kills old instances to avoid launching duplicates.
- Dev: `compile_and_run.sh` now waits for slow launches (polling for the process).
- Dev: `compile_and_run.sh` now launches a single app instance (no more extra windows).
- CI: build/test Linux `CodexBarCLI` (x86_64 + aarch64) and publish release assets as `CodexBarCLI-<tag>-linux-<arch>.tar.gz` (+ `.sha256`).
- CLI: add alias fallback for Codex/Claude detection when PATH lookups fail.
- Providers: support Arc browser cookies for Factory/Droid (and other Chromium-based cookie imports).
- Providers: support ChatGPT Atlas browser data for Chromium cookie imports.
- Providers: accept Auth.js secure session cookies for Factory/Droid login detection.
- Providers: accept Factory auth session cookies (session/access-token) for Droid.
- Droid: surface Factory API errors instead of masking them as missing sessions.
- Droid: retry auth without access-token cookies when Factory flags a stale token.
- Droid: try all detected browser profiles before giving up.
- Droid: fall back to auth.factory.ai endpoints when cookies live on the auth host.
- Droid: use WorkOS refresh tokens from browser local storage when cookies fail.
- Droid: read WorkOS refresh tokens from Safari local storage.
- Droid: try stored/WorkOS tokens before Chrome cookies to reduce Chrome Safe Storage prompts.
- Menu: provider switcher bars now track primary quotas (Plan/Tokens/Pro), with Premium shown for Droid.
- Menu: avoid duplicate summary blocks when a provider has no action rows.
- OpenAI web: ignore cookie sets without session tokens to avoid false-positive dashboard fetches.
- Providers: hide z.ai in the menu until an API key is set.
- Menu: refresh runs automatically when opening the menu with a short retry (refresh row removed).
- Menu: hide the Status Page row when a provider has no status URL.
- Menu: align switcher bar with the “show usage as used” toggle.
- Antigravity: fix lsof port filtering by ANDing listen + pid conditions. Thanks @shaw-baobao!
- Claude: default to Claude Code OAuth usage API (credentials from Keychain or `~/.claude/.credentials.json`), with Debug selector + `--claude-source` CLI override (OAuth/Web/CLI).
- OpenAI web: allow importing any signed-in browser session when Codex email is unknown (first-run friendly).
- Core: Linux CLI builds now compile (mac-only WebKit/logging gated; FoundationNetworking imports where needed).
- Core: fix CI flake for Claude trust prompts by making PTY writes fully reliable.
- Core: Cursor provider is macOS-only (Linux CLI builds stub it).
- Core: make `RateWindow` equatable (used by OpenAI dashboard snapshots and tests).
- Tests: cover alias fallback resolution for Codex/Claude and add Linux platform gating coverage (run in CI).
- Tests: cover hiding Codex Credits + Claude Extra usage via the Advanced toggle.
- Docs: expand CLI docs for Linux install + flags.

## 0.14.0 — 2025-12-25
- New providers: Antigravity.
- Antigravity: new local provider for the Antigravity language server (Claude + Gemini quotas) with an experimental toggle; improved plan display + debug output; clearer not-running/port errors; hide account switch.
- Status: poll Google Workspace incidents for Gemini + Antigravity; Status Page opens the Workspace status page.
- Settings: add Providers tab; move ccusage + status toggles to General; keep display controls in Advanced.
- Menu/UI: widen the menu for four providers; cards/charts adapt to menu width; tighten provider switcher/toggle spacing; keep menus refreshed while open.
- Gemini: hide the dashboard action when unsupported.
- Claude: fix Extra usage spend/limit units (cents); improve CLI probe stability; surface web session info in Debug.
- OpenAI web: fix dashboard ghost overlay on desktop (WebKit keepalive window).
- Debug: add a debug-lldb build mode for troubleshooting.

## 0.13.0 — 2025-12-24
- Claude: add optional web-first usage via Safari/Chrome cookies (no CLI fallback) including “Extra usage” budget bar.
- Claude: web identity now uses `/api/account` for email + plan (via rate_limit_tier).
- Settings: standardize “Augment … via web” copy for Codex + Claude web cookie features.
- Debug: Claude dump now shows web strategy, cookie discovery, HTTP status codes, and parsed summary.
- Dev: add Claude web probe CLI to enumerate endpoints/fields using browser cookies.
- Tests: add unit coverage for Claude web API usage, overage, and account parsing.
- Menu: custom menu items now use the native selection highlight color (plus matching selection text/track colors).
- Charts: boost hover highlight contrast for credits/usage history bands.
- Menu: reorder Codex blocks to show credits before cost.
- Menu: split Claude “Extra usage” (no submenu) from “Cost” (history submenu) and trim redundant extra-usage subtext.

## 0.12.0 — 2025-12-23
- Widgets: add WidgetKit extension backed by a shared app‑group usage snapshot.
- New local cost usage tracking (Codex + Claude) via a lightweight scanner — inspired by ccusage (MIT). Computes cost from local JSONL logs without Node CLIs. Thanks @ryoppippi!
- Cost summary now includes last‑30‑days tokens; weekly pace indicators (with runout copy) hide when usage is fully depleted. Thanks @Remedy92!
- Claude: PTY probes now stop after idle, auto‑clean on restart, and run under a watchdog to avoid runaway CLI processes.
- Menu polish: group history under card sections, simplify history labels, and refresh menus live while open.
- Performance: faster usage log scanning + cost parsing; cache menu icons and speed up OpenAI dashboard parsing.
- Sparkle: auto-download updates when auto-check is enabled, and only show the restart menu entry once an update is ready.
- Widgets: experimental WidgetKit extension (may require restarting the widget gallery/Dock to appear).
- Credits: show credits as a progress bar and add a credits history chart when OpenAI web data is available.
- Credits: move “Buy Credits…” into its own menu item and improve auto-start checkout flow.

## 0.11.2 — 2025-12-21
- ccusage-codex cost fetch is faster and more reliable by limiting the session scan window.
- Fix ccusage cost fetch hanging for large Codex histories by draining subprocess output while commands run.
- Fix merged-icon loading animation when another provider is fetching (only the selected provider animates).
- CLI PATH capture now uses an interactive login shell and merges with the app PATH, fixing missing Node/Codex/Claude/Gemini resolution for NVM-style installs.

## 0.11.1 — 2025-12-21
- Gemini OAuth token refresh now supports Bun/npm installations. Thanks @ben-vargas!

## 0.11.0 — 2025-12-21
- New optional cost display in the menu (session + last 30 days), powered by ccusage. Thanks @Xuanwo!
- Fix loading-state card spacing to avoid double separators.

## 0.10.0 — 2025-12-20
- Gemini provider support (usage, plan detection, login flow). Thanks @381181295!
- Unified menu bar icon mode with a provider switcher and Merge Icons toggle (default on when multiple providers are enabled). Thanks @ibehnam!
- Fix regression from 0.9.1 where CLI detection failed for some installs by restoring interactive login-shell PATH loading.

## 0.9.1 — 2025-12-19
- CLI resolution now uses the login shell PATH directly (no more heuristic path scanning), so Codex/Claude match your shell config reliably.

## 0.9.0 — 2025-12-19
- New optional OpenAI web access: reuses your signed-in Safari/Chrome session to show **Code review remaining**, **Usage breakdown**, and **Credits usage history** in the menu (no credentials stored).
- Credits still come from the Codex CLI; OpenAI web access is only used for the dashboard extras above.
- OpenAI web sessions auto-sync to the Codex CLI email, support multiple accounts, and reset/re-import cookies on account switches to avoid stale cross-account data.
- Fix Chrome cookie import (macOS 10): signed-in Chrome sessions are detected reliably (thanks @tobihagemann!).
- Usage breakdown submenu: compact chart with hover details for day/service totals.
- New “Show usage as used” toggle to invert progress bars (default remains “% left”, now in Advanced).
- Session (5-hour) reset now shows a relative countdown (“Resets in 3h 31m”) in the menu card for Codex and Claude.
- Claude: fix reset parsing so “Resets …” can’t be mis-attributed to the wrong window (session vs weekly).

## 0.8.1 — 2025-12-17
- Claude trust prompts (“Do you trust the files in this folder?”) are now auto-accepted during probes to prevent stuck refreshes. Thanks @tobihagemann!

## 0.8.0 — 2025-12-17
- CodexBar is now available via Homebrew: `brew install --cask steipete/tap/codexbar` (updates via `brew upgrade --cask steipete/tap/codexbar`).
- Added session quota notifications for the sliding 5-hour window (Codex + Claude): notifies when it hits 0% and when it’s available again, based only on observed refresh data (including startup when already depleted). Thanks @GKannanDev!

## 0.7.3 — 2025-12-17
- Claude Enterprise accounts whose Claude Code `/usage` panel only shows “Current session” no longer fail parsing; weekly usage is treated as unavailable (fixes #19).

## 0.7.2 — 2025-12-13
- Claude “Open Dashboard” now routes subscription accounts (Max/Pro/Ultra/Team) to the usage page instead of the API console billing page. Thanks @auroraflux!
- Codex/Claude binary resolution now detects mise/rtx installs (shims and newest installed tool version), fixing missing CLI detection for mise users. Thanks @philipp-spiess!
- Claude usage/status probes now auto-accept the first-run “Ready to code here?” permission prompt (when launched from Finder), preventing timeouts and parse errors. Thanks @alexissan!
- General preferences now surface full Codex/Claude fetch errors with one-click copy and expandable details, reducing first-run confusion when a CLI is missing.
- Polished the menu bar “critter” icons: Claude is now a crisper, blockier pixel crab, and Codex has punchier eyes with reduced blurring in SwiftUI/menu rendering.

## 0.7.1 — 2025-12-09
- Menu bar icons now render on a true 18 pt/2× backing with pixel-aligned bars and overlays for noticeably crisper edges.
- PTY runner now preserves the caller’s environment (HOME/TERM/bun installs) while enriching PATH, preventing Codex/Claude
  probes from failing when CLIs are installed via bun/nvm or need their auth/config paths.
- Added regression tests to lock in the enriched environment behavior.
- Fixed a first-launch crash on macOS 26 caused by the 1×1 keepalive window triggering endless constraint updates; the hidden
  window now uses a safe size and no longer spams SwiftUI state warnings.
- Menu action rows now ship with SF Symbol icons (refresh, dashboard, status, settings, about, quit, copy error) for clearer at-a-glance affordances.
- When the Codex CLI is missing, menu and CLI now surface an actionable install hint (`npm i -g @openai/codex` / bun) instead of a generic PATH error.
- Node manager (nvm/fnm) resolution corrected so codex/claude binaries — and their `node` — are found reliably even when installed via fnm aliases or nvm defaults. Thanks @aliceisjustplaying for surfacing the gaps.
- Login menu now shows phase-specific subtitles and disables interaction while running: “Requesting login…” while starting the CLI, then “Waiting in browser…” once the auth URL is printed; success still triggers the macOS notification.
- Login state is tracked per provider so Codex and Claude icons/menus no longer share the same in-flight status when switching accounts.
- Claude login PTY runner detects the auth URL without clearing buffers, keeps the session alive until confirmation, and exposes a Sendable phase callback used by the menu.
- Claude CLI detection now includes Claude Code’s self-updating paths (`~/.claude/local/claude`, `~/.claude/bin/claude`) so PTY probes work even when only the bundled installer is used.

## 0.7.0 — 2025-12-07
- ✨ New rich menu card with inline progress bars and reset times for each provider, giving the menu a beautiful, at-a-glance dashboard feel (credit: Anton Sotkov @antons).

## 0.6.1 — 2025-12-07
- Claude CLI probes stop passing `--dangerously-skip-permissions`, aligning with the default permission prompt and avoiding hidden first-run failures.

## 0.6.0 — 2025-12-04
- New bundled CLI (`codexbar`) with single `usage` command, `--format text|json`, `--status`, and fast `-h/-V`.
- CLI output now shows consistent headers (`Codex 0.x.y (codex-cli)`, `Claude Code <ver> (claude)`) and JSON includes `source` + `status`.
- Advanced prefs install button symlinks `codexbar` into /usr/local/bin and /opt/homebrew/bin; docs refreshed.

## 0.5.7 — 2025-11-26
- Status Page and Usage Dashboard menu actions now honor the icon you click; Codex menus no longer open the Claude status site.

## 0.5.6 — 2025-11-25
- New playful “Surprise me” option adds occasional blinks/tilts/wiggles to the menu bar icons (one random effect at a time) plus a Debug “Blink now” trigger.
- Preferences now include an Advanced tab (refresh cadence, Surprise me toggle, Debug visibility); window height trimmed ~20% for a tighter fit.
- Motion timing eased and lengthened so blinks/wiggles feel smoother and less twitchy.

## 0.5.5 — 2025-11-25
- Claude usage scrape now recognizes the new “Current week (Sonnet only)” bar while keeping the legacy Opus label as a fallback.
- Menu and docs now label the Claude tertiary limit as Sonnet to match the latest CLI wording.
- PATH seeding now uses a deterministic binary locator plus a one-shot login-shell capture at startup (no globbed nvm paths); the Debug tab shows the resolved Codex binary and effective PATH layers.

## 0.5.4 — 2025-11-24
- Status blurb under “Status Page” no longer prefixes the text with “Status:”, keeping the incident description concise.
- PTY runner now registers cleanup before launch so both ends of the TTY and the process group are torn down even when `Process.run()` throws (no leaked fds when spawn fails).

## 0.5.3 — 2025-11-22
- Added a per-provider “Status Page” menu item beneath Usage that opens the provider’s live status page (OpenAI or Claude).
- Status API now refreshes alongside usage; incident states show a dot/! overlay on the status icon plus a status blurb under the menu item.
- General preferences now include a default-on “Check provider status” toggle above refresh cadence.

## 0.5.2 — 2025-11-22
- Release packaging now includes uploading the dSYM archive alongside the app zip to aid crash symbolication (policy documented in the shared mac release guide).
- Claude PTY fallback removed: Claude probes now rely solely on `script` stdout parsing, and the generic TTY runner is trimmed to Codex `/status` handling.
- Fixed a busy-loop on the codex RPC stderr pipe (handler now detaches on EOF), eliminating the long-running high-CPU spin reported in issue #9.

## 0.5.1 — 2025-11-22
- Debug pane now exposes the Claude parse dump toggle, keeping the captured raw scrape in memory for inspection.
- Claude About/debug views embed the current git hash so builds can be identified precisely.
- Minor runtime robustness tweaks in the PTY runner and usage fetcher.

## 0.5.0 — 2025-11-22
- Codex usage/credits now use the codex app-server RPC by default (with PTY `/status` fallback when RPC is unavailable), reducing flakiness and speeding refreshes.
- Codex CLI launches seed PATH with Homebrew/bun/npm/nvm/fnm defaults to avoid ENOENT in hardened/release builds; TTY probes reuse the same PATH.
- Claude CLI probe now runs `/usage` and `/status` in parallel (no simulated typing), captures reset strings, and uses a resilient parser (label-first with ordered fallback) while keeping org/email separate by provider.
- TTY runner now always tears down the spawned process group (even on early Claude login prompts) to avoid leaking CLI processes.
- Default refresh cadence is now 5 minutes, and a 15-minute option was added to the settings picker.
- Claude probes/version detection now start with `--allowed-tools ""` (tool access disabled) while keeping interactive PTY mode working.
- Codex probes and version detection now launch the CLI with `-s read-only -a untrusted` to keep PTY runs sandboxed.
- Codex warm-up screens (“data not available yet”) are handled gracefully: cached credits stay visible and the menu skips the scary parse error.
- Codex reset times are shown for both RPC and TTY fallback, and plan labels are capitalized while emails stay verbatim.

## 0.4.3 — 2025-11-21
- Fix status item creation timing on macOS 15 by deferring NSStatusItem setup to after launch; adds a regression test for the path.
- Menu bar icon with unknown usage now draws empty tracks (instead of a full bar when decorations are shown) by treating nil values as 0%.

## 0.4.2 — 2025-11-21
- Sparkle updates re-enabled in release builds (disabled only for the debug bundle ID).

## 0.4.1 — 2025-11-21
- Both Codex and Claude probes now run off the main thread (background PTY), avoiding menu/UI stalls during `/status` or `/usage` fetches.
- Codex credits stay available even when `/status` times out: cached values are kept and errors are surfaced separately.
- Claude/Codex provider autodetect runs on first launch (defaults to Codex if neither is installed) with a debug reset button.
- Sparkle updates re-enabled in release builds (disabled only for debug bundle ID).
- Claude probe now issues the `/usage` slash command directly to land on the Usage tab reliably and avoid palette misfires.

## 0.4.0 — 2025-11-21
- Claude Code support: dedicated Claude menu/icon plus dual-wired menus when both providers are enabled; shows email/org/plan and Sonnet usage with clickable errors.
- New Preferences window: General/About tabs with provider toggles, refresh cadence, start-at-login, and always-on Quit.
- Codex credits without web login: we now read `codex /status` in a PTY, auto-skip the update prompt, and parse session/weekly/credits; cached credits stay visible on transient timeouts.
- Resilience: longer PTY timeouts, cached-credit fallback, one-line menu errors, and clearer parse/update messages.

## 0.3.0 — 2025-11-18
- Credits support: reads Codex CLI `/status` via PTY (no browser login), shows remaining credits inline, and moves history to a submenu.
- Sign-in window with cookie reuse and a logout/clear-cookies action; waits out workspace picker and auto-navigates to usage page.
- Menu: credits line bolded; login prompt hides once credits load; debug toggle always visible (HTML dump).
- Icon: when weekly is empty, top bar becomes a thick credits bar (capped at 1k); otherwise bars stay 5h/weekly.

## 0.2.2 — 2025-11-17
- Menu bar icon stays static when no account/usage is present; loading animation only runs while fetching (12 fps) to keep idle CPU low.
- Usage refresh first tails the newest session log (512 KB window) before scanning everything, reducing IO on large Codex logs.
- Packaging/signing hardened: strip extended attributes, delete AppleDouble (`._*`) files, and re-sign Sparkle + app bundle to satisfy Gatekeeper.

## 0.2.1 — 2025-11-17
- Patch bump for refactor/relative-time changes; packaging scripts set to 0.2.1 (5).
- Streamlined Codex usage parsing: modern rate-limit handling, flexible reset time parsing, and account rate-limit updates (thanks @jazzyalex and https://jazzyalex.github.io/agent-sessions/).

## 0.2.0 — 2025-11-16
- CADisplayLink-based loading animations (macOS 15 displayLink API) with randomized patterns (Knight Rider, Cylon, outside-in, race, pulse) and debug replay cycling through all.
- Debug replay toggle (`defaults write com.steipete.codexbar debugMenuEnabled -bool YES`) to view every pattern.
- Usage Dashboard link in menu; menu layout tweaked.
- Updated time now shows relative formatting when fresher than 24h; refactored sources into smaller files for maintainability.
- Version bumped to 0.2.0 (4).

## 0.1.2 — 2025-11-16
- Animated loading icon (dual bars sweep until usage arrives); always uses rendered template icon.
- Sparkle embedding/signing fixed with deep+timestamp; notarization pipeline solid.
- Icon conversion scripted via ictool with docs.
- Menu: settings submenu, no GitHub item; About link clickable.

## 0.1.1 — 2025-11-16
- Launch-at-login toggle (SMAppService) and saved preference applied at startup.
- Sparkle auto-update wiring (SUFeedURL to GitHub, SUPublicEDKey set); Settings submenu with auto-update toggle + Check for Updates.
- Menu cleanup: settings grouped, GitHub menu removed, About link clickable.
- Usage parser scans newest session logs until it finds `token_count` events.
- Icon pipeline fixed: regenerated `.icns` via ictool with proper transparency (docs in docs/icon.md).
- Added lint/format configs, Swift Testing, strict concurrency, and usage parser tests.
- Notarized release build "CodexBar-0.1.0.zip" remains current artifact; app version 0.1.1.

## 0.1.0 — 2025-11-16
- Initial CodexBar release: macOS 15+ menu bar app, no Dock icon.
- Reads latest Codex CLI `token_count` events from session logs (5h + weekly usage, reset times); no extra login or browser scraping.
- Shows account email/plan decoded locally from `auth.json`.
- Horizontal dual-bar icon (top = 5h, bottom = weekly); dims on errors.
- Configurable refresh cadence, manual refresh, and About links.
- Async off-main log parsing for responsiveness; strict-concurrency build flags enabled.
- Packaging + signing/notarization scripts (arm64); build scripts convert `.icon` bundle to `.icns`.
