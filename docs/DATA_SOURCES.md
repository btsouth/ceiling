# How Ceiling gets your data

Ceiling is a local-first Windows app. It reads your AI usage from sources that
already live on your PC, or by calling each provider's own usage endpoint.
**Your provider credentials and usage data are stored only on this PC, and
Ceiling never sends them to a Ceiling-operated server.** Fetching usage does
make network requests, but only to the provider you enabled (and, for Wayfinder,
to the local gateway URL you configure yourself), never to Ceiling's own
servers.

This document is the maintained reference behind the "How Ceiling gets this
data" panel in a provider's Settings detail view.

## What Ceiling stores locally

| Data | Where | Protection |
|------|-------|------------|
| API keys | `%APPDATA%\Ceiling` (ApiKeys store) | Windows DPAPI, user-only file ACL |
| Manual cookies | `%APPDATA%\Ceiling` (ManualCookies store) | Windows DPAPI, user-only file ACL |
| OAuth token accounts | `%APPDATA%\Ceiling` (token store) | Windows DPAPI, user-only file ACL |
| Settings | `%APPDATA%\Ceiling\settings.json` - [schema](SETTINGS_JSON.md) | Local file |

Ceiling also *reads* credentials that other tools already wrote (for example
`~/.codex/auth.json` or the Cursor IDE session database). It does not scan
browser cookie databases; website cookies must be copied explicitly. See
[COOKIES.md](COOKIES.md) for the manual setup and storage details.

## Per-provider sources

| Provider | How Ceiling reads usage | Talks to |
|----------|-------------------------|----------|
| **Claude** | Your signed-in Claude Code / Claude Desktop session, an OAuth token, or claude.ai cookies you provide; plus local `~/.claude` logs for cost and tokens. When a Claude Code OAuth token has expired and its credentials store is writable, Ceiling refreshes it and saves the new token there. Claude Desktop's session is only read, never refreshed or modified. | api.anthropic.com, claude.ai, platform.claude.com (OAuth token refresh) |
| **Codex / OpenAI** | The OAuth token from your Codex CLI (`~/.codex/auth.json`) plus local `~/.codex/sessions` logs | chatgpt.com |
| **Cursor** | Your signed-in Cursor IDE session (`state.vscdb`), or cursor.com cookies you provide. On-demand is billed spend; plan/included dollars are Cursor internal units, not an invoice. Grok Bot's weekly allowance, its on-demand spend and The X Subscription's shared weekly pool come from the same cursor.com session. Local Composer tracking (`~/.cursor/ai-tracking`) is activity share, not tokens or spend. | cursor.com |
| **GitHub Copilot** | Your GitHub CLI / Git Credential Manager token (or a device-flow sign-in) | api.github.com, or your GitHub Enterprise host |
| **Antigravity** | Model-group quota from the signed-in Antigravity CLI's read-only `/usage` command on Windows | Google's Antigravity backend through `agy` |
| **Gemini Code Assist** | The existing signed-in Gemini CLI session (`~/.gemini/oauth_creds.json`), read only. Ceiling does not modify that file; sign in again with Gemini CLI if it expires. | Google Code Assist and OAuth endpoints |
| **Grok** | The OIDC session from Grok Build (`~/.grok/auth.json` via `grok login`), or grok.com cookies you provide; tokens refresh via auth.x.ai; plus local `~/.grok/sessions` logs for tokens / cache / effort / projects / API-equivalent $ (`costUsdTicks`) | grok.com, auth.x.ai |
| **DeepSeek Harness** (`dsh`, DeepSeek's open-source coding agent) | Read-only `$DSH_HOME/sessions` JSONL/Zstandard usage, default `~/.dsh`; existing `deepseek-account-platform/default` grant in `.credentials.yaml` for the official account wallet query. No API key, credential refresh/write, model request or conversation upload. Recharge and bonus balances remain separate; local Token counts are not a bill or quota percentage. | platform.deepseek.com |
| Other providers | A local session or token on this PC, or the provider's own usage endpoint | The provider's own domain only |

### Gemini subscription quota on Windows

Sign in to Antigravity CLI 1.2.13 or later and enable **Antigravity** in Ceiling.
The card shows the available Gemini five-hour and weekly quotas. Ceiling reads
the [structured output of the CLI's read-only quota command](https://antigravity.google/docs/changelog?tab=cli).
On Windows with Antigravity CLI installed, existing Gemini display preferences
move to Antigravity when there is no Gemini CLI sign-in or when the stored
Gemini CLI credentials belong to a personal Google account (no Workspace domain
claim). Organization and Enterprise Gemini Code Assist accounts stay untouched,
and the `gemini` provider ID remains available for Gemini Code Assist.

## Network policy

- The Ceiling desktop app makes no analytics or telemetry calls to
  Ceiling-operated servers.
- Every usage request targets the provider's own domain listed above, or,
  for Wayfinder, the local gateway URL you configure yourself.
- Status-page polling is opt-in under Notifications and only runs for
  enabled providers.
- Two other non-provider hosts are contacted regardless of that switch:
  models.dev (public model prices, 24-hour TTL) and GitHub
  (`api.github.com`) for the update check on the selected channel.
- Local session activity detection on Windows looks at the names and PIDs of
  running processes on this PC. It does not read command lines, and none of
  it is sent over the network.

If you enable a provider and don't see it described here or in the in-app panel,
that's a bug. Please open an issue.
