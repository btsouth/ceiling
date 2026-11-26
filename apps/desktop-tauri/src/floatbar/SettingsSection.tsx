import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Field, Select, Toggle } from "../components/FormControls";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import { useLocale } from "../hooks/useLocale";
import { formatLocale } from "../lib/formatLocale";
import { getDirectoryAccounts, getTaskbarWidgetStatus } from "../lib/tauri";
import type { LocaleKey } from "../i18n/keys";
import type {
  FloatBarOrientation,
  FloatBarContrast,
  FloatBarDensity,
  FloatBarInformationMode,
  FloatBarSelectionMode,
  ProviderAccountsBridge,
  SettingsSnapshot,
  SettingsUpdate,
  TaskbarWidgetStatus,
} from "../types/bridge";

/** Keep in sync with `MAX_TASKBAR_WIDGET_PROVIDERS` in taskbar_widget.rs. */
const MAX_STRIP_PROVIDERS = 5;

const PROVIDER_LABELS: Record<string, string> = {
  codex: "Codex",
  claude: "Claude",
  cursor: "Cursor",
  grok: "Grok",
  gemini: "Gemini Code Assist",
  copilot: "Copilot",
  openaiapi: "OpenAI",
};

interface Props {
  settings: SettingsSnapshot;
  saving: boolean;
  set: (patch: SettingsUpdate) => void;
}

function providerLabel(id: string): string {
  return PROVIDER_LABELS[id] ?? id.charAt(0).toUpperCase() + id.slice(1);
}

/** Localized status line for the Taskbar Usage group; `null` hides the row. */
function taskbarWidgetStatusMessage(
  status: TaskbarWidgetStatus | null,
  t: (key: LocaleKey) => string,
): string | null {
  if (!status) return null;
  switch (status.kind) {
    case "active":
      return status.taskbars === 1
        ? t("TaskbarWidgetShownOnOne")
        : formatLocale(t("TaskbarWidgetShownOnMany"), String(status.taskbars));
    case "noFit":
      return t("TaskbarWidgetNoFit");
    case "waitingLandmarks":
      return t("TaskbarWidgetWaitingLandmarks");
    case "noProviders":
      return t("TaskbarWidgetNoProviders");
    case "disabled":
    case "unavailable":
      return null;
  }
}

/** Providers that can track more than one config-directory account. */
const MULTI_ACCOUNT_PROVIDER_IDS = new Set(["codex", "claude"]);

function accountOptionLabel(account: {
  label: string;
  email?: string | null;
}): string {
  const email = account.email?.trim();
  if (email && email !== account.label) {
    return `${account.label} · ${email}`;
  }
  return account.label;
}

/** Enabled providers in Providers-tab order (fallback: settings.enabled list). */
function enabledProvidersInDisplayOrder(settings: SettingsSnapshot): string[] {
  const enabled = new Set(settings.enabledProviders);
  const order =
    settings.providerOrder && settings.providerOrder.length > 0
      ? settings.providerOrder
      : settings.enabledProviders;
  const ordered = order.filter((id) => enabled.has(id));
  for (const id of settings.enabledProviders) {
    if (!ordered.includes(id)) ordered.push(id);
  }
  return ordered;
}

function useDraftNumber(value: number) {
  const [draft, setDraft] = useState(value);

  useEffect(() => {
    setDraft(value);
  }, [value]);

  const commit = useCallback(
    (next: number, onCommit: (value: number) => void) => {
      // Dedupe against the committed prop value, which is the persisted
      // source of truth. The parent's save is fire-and-forget, so we can't
      // observe success/failure here — comparing to `value` (rather than an
      // optimistically-advanced marker) means a failed save leaves the prop
      // unchanged and a re-commit of the same number still fires the retry.
      if (next === value) return;
      onCommit(next);
    },
    [value],
  );

  return { draft, setDraft, commit };
}

/**
 * Settings UI for the two independent at-a-glance surfaces.
 */
const KEEP_ONE_HINT_ID = "strip-keep-one-hint";

export default function FloatBarSettingsSection({ settings, saving, set }: Props) {
  const { t } = useLocale();
  const opacity = useDraftNumber(settings.floatBarOpacity);
  const scale = useDraftNumber(settings.floatBarScale);
  const commitOpacity = () => {
    opacity.commit(opacity.draft, (value) => set({ floatBarOpacity: value }));
  };
  const commitScale = () => {
    scale.commit(scale.draft, (value) => set({ floatBarScale: value }));
  };

  const [taskbarStatus, setTaskbarStatus] = useState<TaskbarWidgetStatus | null>(
    null,
  );
  useEffect(() => {
    let cancelled = false;
    let latestRequest = 0;
    const refreshTaskbarStatus = () => {
      const request = ++latestRequest;
      getTaskbarWidgetStatus()
        .then((status) => {
          // Reads race: a re-fetch triggered by a newer status event may
          // resolve before an older one. Only the latest request may write.
          if (!cancelled && request === latestRequest) setTaskbarStatus(status);
        })
        .catch(() => {
          // Leave the last known status in place if the read fails.
        });
    };
    refreshTaskbarStatus();

    let unlisten: (() => void) | undefined;
    Promise.resolve(listen("taskbar-widget-status-changed", refreshTaskbarStatus))
      .then((fn) => {
        if (cancelled) {
          fn?.();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  const taskbarStatusMessage = taskbarWidgetStatusMessage(taskbarStatus, t);

  const [directoryAccounts, setDirectoryAccounts] = useState<
    ProviderAccountsBridge[]
  >([]);
  useEffect(() => {
    let cancelled = false;
    getDirectoryAccounts()
      .then((rows) => {
        if (!cancelled) setDirectoryAccounts(rows);
      })
      .catch(() => {
        if (!cancelled) setDirectoryAccounts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [settings.enabledProviders, settings.taskbarAccountByProvider]);

  const multiAccountByProvider = useMemo(() => {
    const map = new Map<string, ProviderAccountsBridge>();
    for (const row of directoryAccounts) {
      if (
        MULTI_ACCOUNT_PROVIDER_IDS.has(row.providerId) &&
        row.accounts.length > 1
      ) {
        map.set(row.providerId, row);
      }
    }
    return map;
  }, [directoryAccounts]);

  const enabledOrdered = useMemo(
    () => enabledProvidersInDisplayOrder(settings),
    [settings],
  );
  const customStrip = (settings.floatBarProviderIds?.length ?? 0) > 0;
  const selectedStripIds = useMemo(() => {
    if (!customStrip) {
      return enabledOrdered.slice(0, MAX_STRIP_PROVIDERS);
    }
    const enabled = new Set(settings.enabledProviders);
    return settings.floatBarProviderIds.filter((id) => enabled.has(id));
  }, [customStrip, enabledOrdered, settings.enabledProviders, settings.floatBarProviderIds]);

  const commitStripIds = (ids: string[]) => {
    // Empty list restores automatic order (enabled providers, Providers tab order).
    set({ floatBarProviderIds: ids });
  };

  const toggleStripProvider = (id: string, include: boolean) => {
    const base = customStrip
      ? settings.floatBarProviderIds.filter((pid) =>
          settings.enabledProviders.includes(pid),
        )
      : enabledOrdered.slice(0, MAX_STRIP_PROVIDERS);
    if (include) {
      if (base.includes(id)) return;
      if (base.length >= MAX_STRIP_PROVIDERS) return;
      commitStripIds([...base, id]);
      return;
    }
    if (base.length <= 1) return;
    commitStripIds(base.filter((pid) => pid !== id));
  };

  const moveStripProvider = (id: string, delta: -1 | 1) => {
    const base = customStrip
      ? settings.floatBarProviderIds.filter((pid) =>
          settings.enabledProviders.includes(pid),
        )
      : [...selectedStripIds];
    const index = base.indexOf(id);
    if (index < 0) return;
    const next = index + delta;
    if (next < 0 || next >= base.length) return;
    const copy = [...base];
    const [row] = copy.splice(index, 1);
    copy.splice(next, 0, row);
    commitStripIds(copy);
  };

  const pinnedAccounts = settings.taskbarAccountByProvider ?? {};
  const setPinnedAccount = (providerId: string, accountId: string) => {
    const next: Record<string, string> = { ...pinnedAccounts };
    if (!accountId) {
      delete next[providerId];
    } else {
      next[providerId] = accountId;
    }
    set({ taskbarAccountByProvider: next });
  };

  return (
    <>
      <section className="settings-section">
        <h3 className="settings-section__title">{t("TaskbarUsageTitle")}</h3>
        <div className="settings-section__group">
          <Field
            label={t("ShowTaskbarUsage")}
            description={t("ShowTaskbarUsageHelp")}
            leading
          >
            <Toggle
              checked={settings.taskbarWidgetEnabled}
              ariaLabel={t("ShowTaskbarUsage")}
              disabled={saving}
              onChange={(v) => set({ taskbarWidgetEnabled: v })}
            />
          </Field>
          <Field
            label={t("OpenOnHover")}
            description={t("OpenOnHoverHelp")}
            leading
          >
            <Toggle
              checked={settings.taskbarWidgetOpenOnHover}
              ariaLabel={t("OpenOnHover")}
              disabled={saving || !settings.taskbarWidgetEnabled}
              onChange={(v) => set({ taskbarWidgetOpenOnHover: v })}
            />
          </Field>
          <Field
            label={t("ShowOnAllMonitors")}
            description={t("ShowOnAllMonitorsHelp")}
            leading
          >
            <Toggle
              checked={settings.taskbarWidgetAllMonitors}
              ariaLabel={t("ShowOnAllMonitors")}
              disabled={saving || !settings.taskbarWidgetEnabled}
              onChange={(v) => set({ taskbarWidgetAllMonitors: v })}
            />
          </Field>
          <Field
            label={t("ShowResetTimeInline")}
            description={t("ShowResetTimeInlineHelp")}
            leading
          >
            <Toggle
              checked={settings.floatBarShowResetInline}
              ariaLabel={t("ShowResetTimeInline")}
              disabled={saving || !settings.taskbarWidgetEnabled}
              onChange={(v) => set({ floatBarShowResetInline: v })}
            />
          </Field>
          {settings.taskbarWidgetEnabled && taskbarStatusMessage && (
            <p className="settings-section__hint" role="status">
              {taskbarStatusMessage}
            </p>
          )}
        </div>

        <div className="settings-section__group taskbar-provider-picker">
          <div className="taskbar-provider-picker__header">
            <div>
              <div className="taskbar-provider-picker__title">{t("StripProvidersTitle")}</div>
              <p className="settings-section__hint">
                {formatLocale(t("StripProvidersHelp"), String(MAX_STRIP_PROVIDERS))}
              </p>
            </div>
            {customStrip && (
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                disabled={saving}
                onClick={() => commitStripIds([])}
              >
                {t("StripUseAutomaticOrder")}
              </button>
            )}
          </div>
          {enabledOrdered.length === 0 ? (
            <p className="settings-section__hint">{t("StripEnableProvidersFirst")}</p>
          ) : (
            <>
              <ul className="taskbar-provider-picker__list">
                {enabledOrdered.map((id) => {
                  const checked = selectedStripIds.includes(id);
                  const rank = selectedStripIds.indexOf(id);
                  const lastChecked = checked && selectedStripIds.length === 1;
                  const atCap =
                    !checked && selectedStripIds.length >= MAX_STRIP_PROVIDERS;
                  const multi = multiAccountByProvider.get(id);
                  return (
                    <li key={id} className="taskbar-provider-picker__row">
                      <div className="taskbar-provider-picker__main">
                        <label className="taskbar-provider-picker__label">
                          <input
                            type="checkbox"
                            className="toggle"
                            checked={checked}
                            disabled={
                              saving ||
                              !settings.taskbarWidgetEnabled ||
                              lastChecked ||
                              (atCap && !checked)
                            }
                            aria-describedby={
                              lastChecked ? KEEP_ONE_HINT_ID : undefined
                            }
                            aria-label={formatLocale(t("StripShowProvider"), providerLabel(id))}
                            onChange={(e) =>
                              toggleStripProvider(id, e.target.checked)
                            }
                          />
                          <ProviderIcon
                            providerId={id}
                            size={16}
                            title={providerLabel(id)}
                          />
                          <span>{providerLabel(id)}</span>
                          {checked && rank >= 0 && (
                            <span className="taskbar-provider-picker__rank">
                              {rank + 1}
                            </span>
                          )}
                        </label>
                        <span className="providers-sidebar__reorder-controls">
                          <button
                            type="button"
                            className="providers-sidebar__reorder-button"
                            aria-label={formatLocale(t("StripMoveUp"), providerLabel(id))}
                            disabled={saving || !checked || rank <= 0}
                            onClick={() => moveStripProvider(id, -1)}
                          >
                            ↑
                          </button>
                          <button
                            type="button"
                            className="providers-sidebar__reorder-button"
                            aria-label={formatLocale(t("StripMoveDown"), providerLabel(id))}
                            disabled={
                              saving ||
                              !checked ||
                              rank < 0 ||
                              rank >= selectedStripIds.length - 1
                            }
                            onClick={() => moveStripProvider(id, 1)}
                          >
                            ↓
                          </button>
                        </span>
                      </div>
                      {multi && checked && (
                        <label className="taskbar-provider-picker__account">
                          <span className="taskbar-provider-picker__account-label">
                            {t("StripTaskbarShows")}
                          </span>
                          <select
                            className="select"
                            aria-label={formatLocale(t("StripTaskbarAccount"), providerLabel(id))}
                            disabled={saving || !settings.taskbarWidgetEnabled}
                            value={pinnedAccounts[id] ?? ""}
                            onChange={(e) =>
                              setPinnedAccount(id, e.target.value)
                            }
                          >
                            <option value="">
                              {t("StripAutoClosest")}
                            </option>
                            {multi.accounts.map((account) => (
                              <option key={account.id} value={account.id}>
                                {accountOptionLabel(account)}
                              </option>
                            ))}
                          </select>
                        </label>
                      )}
                    </li>
                  );
                })}
              </ul>
              {selectedStripIds.length === 1 && (
                <p id={KEEP_ONE_HINT_ID} className="settings-section__hint">
                  {/* Automatic order has no choices to restore, and the button
                      that would do it is hidden, so name the real way out. */}
                  {customStrip
                    ? t("StripKeepAtLeastOne")
                    : t("StripAlwaysOneProvider")}
                </p>
              )}
            </>
          )}
          {multiAccountByProvider.size > 0 && (
            <p className="settings-section__hint">
              {t("StripAccountHint")}
            </p>
          )}
        </div>
      </section>

      <section className="settings-section">
        <h3 className="settings-section__title">{t("FloatingBarTitle")}</h3>
        <div className="settings-section__group">
        <Field
          label={t("ShowFloatingBar")}
          description={t("ShowFloatingBarHelp")}
          leading
        >
          <Toggle
            checked={settings.floatBarEnabled}
            ariaLabel={t("ShowFloatingBar")}
            disabled={saving}
            onChange={(v) => set({ floatBarEnabled: v })}
          />
        </Field>
          <Field
            label={t("FloatBarOrientation")}
            description={t("FloatBarOrientationHelp")}
          >
            <Select
              value={settings.floatBarOrientation}
              disabled={saving || !settings.floatBarEnabled}
              options={[
                { value: "horizontal", label: t("OrientationHorizontal") },
                { value: "vertical", label: t("OrientationVertical") },
              ]}
              onChange={(v) => set({ floatBarOrientation: v as FloatBarOrientation })}
            />
          </Field>
          <Field
            label={t("FloatBarDensity")}
            description={t("FloatBarDensityHelp")}
          >
            <Select
              value={settings.floatBarDensity}
              disabled={saving || !settings.floatBarEnabled}
              options={[
                { value: "compact", label: t("DensityCompact") },
                { value: "standard", label: t("DensityStandard") },
                { value: "detailed", label: t("DensityDetailed") },
              ]}
              onChange={(v) => set({ floatBarDensity: v as FloatBarDensity })}
            />
          </Field>
          <Field
            label={t("FloatBarInformation")}
            description={t("FloatBarInformationHelp")}
          >
            <Select
              value={settings.floatBarInformationMode}
              disabled={saving || !settings.floatBarEnabled}
              options={[
                { value: "exact", label: t("InformationExact") },
                { value: "calm", label: t("InformationCalm") },
              ]}
              onChange={(v) =>
                set({ floatBarInformationMode: v as FloatBarInformationMode })
              }
            />
          </Field>
          <Field
            label={t("FloatBarProvidersMode")}
            description={t("FloatBarProvidersModeHelp")}
          >
            <Select
              value={settings.floatBarSelectionMode ?? "pinned"}
              disabled={saving || !settings.floatBarEnabled}
              options={[
                { value: "pinned", label: t("SelectionModePinned") },
                { value: "active", label: t("SelectionModeActive") },
                {
                  value: "activePlusCritical",
                  label: t("SelectionModeActivePlusCritical"),
                },
              ]}
              onChange={(v) =>
                set({ floatBarSelectionMode: v as FloatBarSelectionMode })
              }
            />
          </Field>
          <Field
            label={t("WatchFocusedApp")}
            description={t("WatchFocusedAppHelp")}
            leading
          >
            <Toggle
              checked={settings.floatBarForegroundDetection}
              ariaLabel={t("WatchFocusedApp")}
              disabled={
                saving ||
                !settings.floatBarEnabled ||
                (settings.floatBarSelectionMode ?? "pinned") === "pinned"
              }
              onChange={(v) => set({ floatBarForegroundDetection: v })}
            />
          </Field>
          <>
            <Field
              label={t("FloatBarContrast")}
              description={t("FloatBarContrastHelp")}
            >
              <Select
                value={settings.floatBarContrast}
                disabled={saving || !settings.floatBarEnabled}
                options={[
                  { value: "auto", label: t("Automatic") },
                  { value: "light-text", label: t("ContrastLightText") },
                  { value: "dark-text", label: t("ContrastDarkText") },
                ]}
                onChange={(v) => set({ floatBarContrast: v as FloatBarContrast })}
              />
            </Field>
            <Field
              label={formatLocale(t("FloatBarOpacity"), String(opacity.draft))}
              description={t("FloatBarOpacityHelp")}
            >
              <input
                type="range"
                min={30}
                max={100}
                step={5}
                value={opacity.draft}
                disabled={!settings.floatBarEnabled}
                onChange={(e) => opacity.setDraft(Number(e.target.value))}
                onPointerUp={commitOpacity}
                onTouchEnd={commitOpacity}
                onBlur={commitOpacity}
                onKeyUp={commitOpacity}
                aria-label={t("FloatBarOpacityAria")}
              />
            </Field>
            <Field
              label={formatLocale(t("FloatBarSize"), String(scale.draft))}
              description={t("FloatBarSizeHelp")}
            >
              <input
                type="range"
                min={75}
                max={200}
                step={5}
                value={scale.draft}
                disabled={!settings.floatBarEnabled}
                onChange={(e) => scale.setDraft(Number(e.target.value))}
                onPointerUp={commitScale}
                onTouchEnd={commitScale}
                onBlur={commitScale}
                onKeyUp={commitScale}
                aria-label={t("FloatBarSizeAria")}
              />
            </Field>
          </>
          <Field
            label={t("FloatBarClickThrough")}
            description={t("FloatBarClickThroughHelp")}
            leading
          >
            <Toggle
              checked={settings.floatBarClickThrough}
              ariaLabel={t("FloatBarClickThrough")}
              disabled={saving || !settings.floatBarEnabled}
              onChange={(v) => set({ floatBarClickThrough: v })}
            />
          </Field>
        </div>
      </section>
    </>
  );
}
