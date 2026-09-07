# settings.json

Ceiling stores user preferences in `%APPDATA%\Ceiling\settings.json`.

The table below mirrors the fields in `rust/src/settings.rs` and
`rust/src/settings/raw.rs`. Missing fields fall back to the defaults shown.
Prefer changing settings through the UI; hand-editing is supported for the
fields marked safe, but Ceiling may overwrite the file when the app exits.

## Top-level fields

| Field | Type | Default | Notes |
|---|---|---|---|
| `enabled_providers` | array of strings | `["claude","codex","cursor","grok"]` | Provider CLI names to monitor. Safe to edit. |
| `refresh_interval_secs` | number | `300` | `0` means manual refresh only. Safe to edit. |
| `refresh_all_providers_on_menu_open` | boolean | `false` | Legacy compatibility field. Nothing reads this flag — opening a tray/menu surface does not force-refresh. Leave it unchanged. |
| `start_minimized` | boolean | `false` | Start minimized. Safe to edit. |
| `start_at_login` | boolean | `false` | Start at Windows sign-in. Safe to edit. |
| `show_notifications` | boolean | `true` | Show usage notifications. Safe to edit. |
| `capacity_event_notifications_enabled` | boolean | `true` | Raise OS alerts for scheduled and early resets. Safe to edit. |
| `sound_enabled` | boolean | `true` | Play sound effects for threshold alerts. Safe to edit. |
| `sound_volume` | number | `100` | Range `0..=100`. Safe to edit. |
| `high_usage_threshold` | number | `85` | High warning percentage. Safe to edit. |
| `critical_usage_threshold` | number | `90` | Critical severity percentage. Safe to edit. |
| `spend_budget_alerts_enabled` | boolean | `false` | Enable estimated local API value budget alerts. Safe to edit. |
| `spend_budget_period` | string | `"daily"` | `"daily"` or `"monthly"`. Safe to edit. |
| `spend_budget_warning_usd` | number | `5` | Soft alert threshold in USD. Safe to edit. |
| `spend_budget_limit_usd` | number | `15` | Near-cap alert threshold in USD. Safe to edit. |
| `spend_anomaly_alerts_enabled` | boolean | `false` | Warn when today's estimated API value runs far above the recent daily median. Needs no budget. Days under $1 do not build that median, and fewer than three days that do leaves no median at all; a large absolute spend still alerts. Safe to edit. |
| `spend_anomaly_multiplier` | number | `3` | Times the recent daily median that counts as a spike. Clamped to 1.5-20. Safe to edit. |
| `provider_incident_badges_enabled` | boolean | `false` | Poll public provider status pages and badge providers having an incident. Sends no account data. Safe to edit. |
| `notification_policy_version` | number | `1` | Internal migration marker. Not a UI preference. |
| `provider_usage_thresholds` | object | `{}` | Per-provider overrides; keys are CLI names, optionally suffixed with `:session` or `:weekly`; values are `{ "high": number, "critical": number }`. Safe to edit. |
| `switcher_shows_icons` | boolean | `true` | Show provider icons in merged switcher UI. Safe to edit. |
| `menu_bar_shows_highest_usage` | boolean | `false` | Legacy compatibility field. The branded tray always follows the most constrained healthy provider and ignores this flag. Leave it unchanged. |
| `menu_bar_shows_percent` | boolean | `false` | Legacy compatibility field. The branded tray always draws the Ceiling icon and ignores this flag. Leave it unchanged. |
| `show_as_used` | boolean | `true` | Show usage as used (`true`) or remaining (`false`). Safe to edit. |
| `enable_animations` | boolean | `true` | Enable UI animations. Safe to edit. |
| `reset_time_relative` | boolean | `true` | Show reset times as relative values. Safe to edit. |
| `show_reset_when_exhausted` | boolean | `false` | Replace exhausted quota text with the concrete reset time. Safe to edit. |
| `predictive_pace_warning_enabled` | boolean | `false` | Warn when provider pace predicts exhaustion before reset. Opt-in prediction. Safe to edit. |
| `menu_bar_display_mode` | string | `"detailed"` | Legacy compatibility field (`"minimal"`, `"compact"`, or `"detailed"`). The branded tray ignores this setting. Leave it unchanged. |
| `show_all_token_accounts_in_menu` | boolean | `false` | Show all token accounts instead of collapsing behind switchers. Safe to edit. |
| `provider_configs` | object | `{}` | Per-provider configuration map. See below. Safe to edit. |
| `unrecognized_provider_configs` | object | `{}` | Written by Ceiling, not by you. Holds `provider_configs` entries whose provider id this build does not recognize, so they survive until a build that knows them folds them back in. Leave it unchanged. |
| `disable_keychain_access` | boolean | `false` | Disable all OS keychain reads and writes (SBS-1023). Credential paths skip the keyring and fall through to Preferences, environment, or files. Safe to edit. |
| `hide_personal_info` | boolean | `false` | Hide emails and account names for streaming/sharing. Safe to edit. |
| `update_channel` | string | `"stable"` | `"stable"` or `"beta"`. Safe to edit. |
| `provider_metrics` | object | `{}` | Per-provider metric preference by CLI name; values include `automatic`, `session`, `weekly`, `model`, `tertiary`, `credits`, `extraUsage`, and `average`. Safe to edit. |
| `provider_order` | array of strings | `[]` | Empty means canonical provider order. Safe to edit. |
| `global_shortcut` | string | `"Ctrl+Shift+U"` | Shortcut to open the menu. Safe to edit. |
| `taskbar_toggle_shortcut` | string | `"Ctrl+Shift+H"` | Shortcut to show/hide the taskbar capacity strip. Safe to edit. |
| `codex_custom_sessions_dirs` | array of strings | `[]` | Additional Codex home/session directories for local cost scans. Safe to edit. |
| `agent_sessions_enabled` | boolean | `false` | Discover local and configured SSH agent sessions. Safe to edit. |
| `agent_session_ssh_hosts` | array of strings | `[]` | SSH targets for remote agent sessions. Safe to edit. |
| `auto_download_updates` | boolean | `false` | Automatically download updates in the background. Safe to edit. |
| `install_updates_on_quit` | boolean | `false` | Install pending updates when quitting. Safe to edit. |
| `ui_language` | string | `"english"` | `english`, `chinese`, `chinesetraditional`, `japanese`, `korean`, or `spanish`. Safe to edit. |
| `theme` | string | `"auto"` | `auto`, `light`, or `dark`. Safe to edit. |
| `window_scale_percent` | number | `100` | Range `100..=250`. Safe to edit. |
| `tray_scale_percent` | number | `100` | Range `100..=200`. Safe to edit. |
| `powertoys_status_pipe_enabled` | boolean | `false` | Enable the local PowerToys Command Palette status pipe. Safe to edit. |
| `float_bar_enabled` | boolean | `false` | Show the floating capacity bar. Safe to edit. |
| `taskbar_widget_enabled` | boolean | `true` | Show the native taskbar usage readout. Safe to edit. |
| `taskbar_widget_all_monitors` | boolean | `false` | Mirror the taskbar readout on every verified horizontal taskbar. Safe to edit. |
| `float_bar_opacity` | number | `80` | Range `30..=100`. Safe to edit. |
| `float_bar_scale` | number | `100` | Range `75..=200`. Safe to edit. |
| `float_bar_orientation` | string | `"horizontal"` | `"horizontal"` or `"vertical"`. Safe to edit. |
| `float_bar_style` | string | `"floating"` | Legacy display style; new settings store `"floating"`. Safe to edit. |
| `taskbar_widget_open_on_hover` | boolean | `true` | Open the taskbar glance panel after pointer dwell. Safe to edit. |
| `float_bar_density` | string | `"standard"` | `"compact"`, `"standard"`, or `"detailed"`. Safe to edit. |
| `float_bar_information_mode` | string | `"exact"` | `"exact"` or `"calm"`. Safe to edit. |
| `float_bar_selection_mode` | string | `"pinned"` | `"pinned"` (the configured list), `"active"` (the focused supported app), or `"activePlusCritical"` (active plus any provider in the display set at or above the warning threshold). The display set is `float_bar_provider_ids` when that is non-empty, otherwise every enabled provider. Matching is exact and untrimmed, so `"Active"` or `" active "` fall back to `"pinned"`, as does any other unrecognized string. The active provider is sticky: an unrecognized window keeps it, but focusing an app mapped to a provider that is disabled or failing replaces it, and the bar falls back to the display set until a usable provider is focused. The display set is also what you see before anything has matched. Safe to edit. |
| `float_bar_foreground_detection` | boolean | `true` | When `false`, `"active"` / `"activePlusCritical"` keep the pinned list and do not read the focused window. Safe to edit. |
| `float_bar_contrast` | string or null | `"auto"` | `"auto"`, `"light-text"`, or `"dark-text"`; an invalid string normalizes to `"auto"`. `null` is accepted and deserializes to `None`, which resolves through the legacy `float_bar_dark_text` flag instead. Safe to edit. |
| `float_bar_click_through` | boolean | `false` | Make the floating bar fully click-through. Safe to edit. |
| `float_bar_provider_ids` | array of strings | `[]` | Empty means all enabled providers. Safe to edit. |
| `taskbar_account_by_provider` | object | `{}` | Provider CLI name to directory-account UUID for compact taskbar/float-bar strips. Safe to edit. |
| `float_bar_dark_text` | boolean | `false` | Dark-on-light palette for the floating bar. Safe to edit. |
| `float_bar_show_reset_inline` | boolean | `true` | Show the next reset inline in each pill. Safe to edit. |
| `float_bar_show_cost` | boolean | `false` | Legacy compatibility field; the current UI no longer renders local cost pills, so it has no visible effect. Leave it unchanged. |

## `provider_configs`

Each key is a provider CLI name. Values are objects with optional fields:

| Field | Type | Notes |
|---|---|---|
| `cookie_source` | string | Provider-specific cookie source. |
| `usage_source` | string | Provider-specific usage source; defaults to `"auto"`. |
| `api_region` | string | Provider-specific API region. |
| `workspace_id` | string | Provider-specific workspace id. |
| `gateway_url` | string | Wayfinder gateway URL override. |
| `ide_base_path` | string | Provider-specific IDE base path. |
| `openai_web_extras` | boolean | Codex-only; opt out of OpenAI web extras surfaces. |
| `spark_usage_visible` | boolean | Codex-only; show Codex Spark quota rows. |
| `historical_tracking` | boolean | Codex-only; default `false`. |
| `avoid_keychain_prompts` | boolean | Claude-only; default `false`. When true, Claude does not read or write the OS keychain (SBS-1023). |

Legacy flat fields such as `codex_cookie_source`, `claude_cookie_source`, and
`alibaba_api_region` are accepted on load and migrated into `provider_configs`.
New saves write only the unified map.

A provider id this build does not recognize does not fail the load. It moves to
`unrecognized_provider_configs` and is written back untouched, so downgrading
past a provider does not delete its configuration.

## Secrets

API keys, manual cookies, and OAuth token accounts are stored in separate
DPAPI-protected stores, not in `settings.json`. Do not write secrets into this
file.

## Example

```json
{
  "enabled_providers": ["claude", "codex", "cursor", "grok"],
  "refresh_interval_secs": 300,
  "start_at_login": true,
  "show_notifications": true,
  "sound_volume": 100,
  "high_usage_threshold": 85,
  "critical_usage_threshold": 90,
  "global_shortcut": "Ctrl+Shift+U",
  "theme": "auto"
}
```
