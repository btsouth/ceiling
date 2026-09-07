# WSL Support

Ceiling runs natively inside WSL. The CLI works out of the box; the desktop shell
requires [WSLg](https://github.com/microsoft/wslg) (Windows 11, build 22000+).

## Quick Start

```bash
git clone https://github.com/btsouth/ceiling.git
cd ceiling
./scripts/dev.sh
```

This will:
1. Detect your WSL environment
2. Build Ceiling Desktop through Tauri's no-bundle workflow
3. Launch the desktop shell (WSLg) or CLI (no display server detected)

CLI-only mode (no display server needed):
```bash
./scripts/dev.sh --cli              # codexbar usage -p all
./scripts/dev.sh --release          # optimised build
```

## How It Works

When running inside WSL, Ceiling:

- **Browser cookies**: Ceiling does not scan browser cookie databases on Windows or in WSL.
  Paste a Cookie header in Settings → Providers → provider detail → Browser Cookies, or use CLI-based provider auth.
  See [COOKIES.md](COOKIES.md).
- **Provider CLIs**: Works with `codex`, `claude`, `gemini` etc. installed inside WSL natively.
- **Desktop shell**: Requires WSLg (Windows 11) or an X server. Falls back to CLI mode automatically.
- **Notifications**: Uses `notify-send` in WSL. Falls back to logging if unavailable.

## Authentication Tips

| Provider | WSL Auth Strategy |
|----------|-------------------|
| Codex | `npm i -g @openai/codex` inside WSL, then `codex login` |
| Claude | `npm i -g @anthropic-ai/claude-code` inside WSL, then `claude login` |
| Gemini | `npm i -g @google/gemini-cli` inside WSL, then run `gemini` and sign in when it prompts. To use a different account, sign out inside it with `/auth signout` first. Ceiling reads `~/.gemini/oauth_creds.json`. |
| Cursor / Kimi | Manual cookies — copy from browser DevTools (F12 → Network → Cookie header) |
| Copilot | GitHub Device Flow works natively in WSL |

## Differences from Native Windows

| Feature | Windows | WSL |
|---------|---------|-----|
| Browser cookies | No browser DB scan; paste a cookie header | Same |
| Desktop Shell | Native | Via WSLg |
| Notifications | PowerShell toast | notify-send |
