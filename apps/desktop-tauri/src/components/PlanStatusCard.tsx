import type { CSSProperties } from "react";
import type { ProviderUsageSnapshot, RateWindowSnapshot } from "../types/bridge";
import { ProviderIcon } from "./providers/ProviderIcon";
import { accountIdentityLabel } from "../lib/providerRow";
import { getProviderIcon } from "./providers/providerIcons";
import { expectedOverlay } from "../lib/expectedPace";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { useLocale } from "../hooks/useLocale";
import { formatLocale } from "../lib/formatLocale";
import {
  capacityFreshness,
  bankedResetCredits,
  glanceMeters,
  type ConstrainingWindow,
} from "../lib/capacityPresentation";

function displayPlanName(
  planName: string | null,
  providerName: string,
): string | null {
  if (!planName) return null;
  const trimmed = planName.trim();
  const normalized = trimmed.toLowerCase();
  if (normalized === "default_claude_ai") return "Claude AI";
  const prefix = `${providerName.trim()} `;
  if (trimmed.toLowerCase().startsWith(prefix.toLowerCase())) {
    return trimmed.slice(prefix.length).trim() || trimmed;
  }
  return trimmed;
}

function levelOf(remainPct: number, exhausted: boolean): string {
  if (exhausted) return "exhausted";
  if (remainPct <= 5) return "critical";
  if (remainPct <= 25) return "high";
  return "normal";
}

function pressureLabel(level: string): string | null {
  if (level === "exhausted") return "Depleted";
  if (level === "critical") return "Almost out";
  if (level === "high") return "Near limit";
  return null;
}

function inactiveWindowSummary(
  provider: ProviderUsageSnapshot,
  state: "notEnforced" | "unavailable",
): string | null {
  const labels = [...new Set(
    (provider.inactiveRateWindows ?? [])
      .filter((window) => (window.state ?? "notEnforced") === state)
      .map((window) => window.title.trim())
      .filter(Boolean),
  )];
  if (labels.length === 0) return null;
  const visible = labels.slice(0, 2).join(", ");
  const remaining = labels.length - 2;
  return remaining > 0 ? `${visible} +${remaining}` : visible;
}

function MeterRow({
  meter,
  showAsUsed,
  resetTimeRelative,
  showResetWhenExhausted,
  hero,
}: {
  meter: ConstrainingWindow;
  showAsUsed: boolean;
  resetTimeRelative: boolean;
  showResetWhenExhausted: boolean;
  hero: boolean;
}) {
  const { t } = useLocale();
  const snap = meter.window;
  const usedPct = Math.max(0, Math.min(100, snap.usedPercent));
  const remain = Math.max(0, Math.min(100, snap.remainingPercent));
  const displayPct = showAsUsed ? usedPct : remain;
  const barPct = showAsUsed ? usedPct : remain;
  const suffix = showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix");
  const level = levelOf(remain, snap.isExhausted);
  const status = pressureLabel(level);
  const formattedReset = useFormattedResetTime(
    snap.resetsAt,
    snap.resetDescription,
    resetTimeRelative,
  );
  // Overview always surfaces reset when known — at 100% that is the answer.
  const showReset = !!formattedReset;
  const awaitingReset = snap.isExhausted && showReset;
  // Optional setting: promote reset into the hero slot when depleted.
  const resetAsHero = awaitingReset && showResetWhenExhausted;
  // Where usage should be by now, on long windows only.
  const expected = expectedOverlay(snap, showAsUsed);

  return (
    <div
      className={[
        "plan-status-card__meter",
        hero ? "plan-status-card__meter--hero" : null,
        awaitingReset ? "plan-status-card__meter--awaiting-reset" : null,
      ]
        .filter(Boolean)
        .join(" ")}
    >
      <div className="plan-status-card__meter-top">
        <span className="plan-status-card__meter-label">{meter.label}</span>
        {resetAsHero ? (
          <>
            <span className="plan-status-card__meter-pct plan-status-card__meter-pct--quiet">
              {Math.round(displayPct)}% {suffix}
            </span>
            {status && (
              <span className="plan-status-card__pressure" data-level={level}>
                <span aria-hidden />
                {status}
              </span>
            )}
            <strong className="plan-status-card__meter-reset plan-status-card__meter-reset--hero">
              {formattedReset}
            </strong>
          </>
        ) : (
          <>
            <strong className="plan-status-card__meter-pct">
              {Math.round(displayPct)}% {suffix}
            </strong>
            {status && (
              <span className="plan-status-card__pressure" data-level={level}>
                <span aria-hidden />
                {status}
              </span>
            )}
            {showReset && (
              <span
                className={`plan-status-card__meter-reset${
                  awaitingReset ? " plan-status-card__meter-reset--emphasis" : ""
                }`}
              >
                {formattedReset}
              </span>
            )}
          </>
        )}
      </div>
      {meter.amount && (
        <div className="plan-status-card__meter-amount">
          {meter.amount.formattedLimit
            ? `${meter.amount.formattedUsed} ${t("PanelAmountOf")} ${meter.amount.formattedLimit}`
            : meter.amount.formattedUsed}
        </div>
      )}
      <div
        className="pace-overlay"
        aria-hidden
        title={
          expected
            ? t("UsageExpectedByNow").replace(
                "{}",
                String(Math.round(expected.expectedUsedPercent)),
              )
            : undefined
        }
      >
        <div className="plan-status-card__bar">
          <div
            className="plan-status-card__bar-fill"
            data-level={level}
            style={{ width: `${barPct}%` }}
          />
          {expected?.gap && (
            <div
              className="pace-overlay__gap"
              style={{
                left: `${expected.gap.left.toFixed(1)}%`,
                width: `${expected.gap.width.toFixed(1)}%`,
              }}
            />
          )}
        </div>
        {expected && (
          <div
            className="pace-overlay__tick"
            data-ahead={expected.ahead ? "true" : "false"}
            style={{ left: `${expected.tickPercent.toFixed(1)}%` }}
          />
        )}
      </div>
    </div>
  );
}

export default function PlanStatusCard({
  provider,
  resetTimeRelative,
  showResetWhenExhausted = false,
  showAsUsed = false,
  isRefreshing = false,
  showAccount = false,
  hideEmail = false,
  onSelect,
}: {
  provider: ProviderUsageSnapshot;
  resetTimeRelative: boolean;
  showResetWhenExhausted?: boolean;
  showAsUsed?: boolean;
  isRefreshing?: boolean;
  // True when this provider has more than one account. With one, the account
  // name is noise, so it stays hidden and the plan chip shows as before.
  showAccount?: boolean;
  hideEmail?: boolean;
  onSelect?: () => void;
}) {
  const { t } = useLocale();
  const brand = getProviderIcon(provider.providerId).brandColor;
  const meters = glanceMeters(provider);
  const freshness = capacityFreshness(provider);
  const planName = displayPlanName(provider.planName, provider.displayName);
  // The account's email, shown only when several accounts share this provider.
  // It replaces the plan chip because it already carries the plan.
  const accountName = showAccount
    ? accountIdentityLabel(provider, hideEmail)
    : null;
  const notEnforcedSummary = inactiveWindowSummary(provider, "notEnforced");
  const isHarness = provider.providerId === "deepseekharness";
  const unavailableSummary = isHarness ? null : inactiveWindowSummary(provider, "unavailable");
  const resetCredits = bankedResetCredits(provider);
  const meterSummary = [meters.primary, ...meters.companions]
    .filter((meter): meter is ConstrainingWindow => meter != null)
    .map((meter) => {
      const percent = glanceDisplayPercent(meter.window, showAsUsed);
      const suffix = showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix");
      return `${meter.label}: ${percent}% ${suffix}`;
    });
  if (notEnforcedSummary) {
    meterSummary.push(`${notEnforcedSummary}: ${t("NotCurrentlyEnforced")}`);
  }
  if (unavailableSummary) {
    meterSummary.push(`${unavailableSummary}: ${t("WindowUnavailable")}`);
  }
  if (isHarness) {
    for (const row of harnessOverviewRows(provider.extraRateWindows ?? [])) {
      meterSummary.push(`${row.title} ${row.window.resetDescription ?? "未报告"}`);
    }
  }
  const accessibleLabel = provider.error
    ? `${provider.displayName}: ${provider.error}`
    : [provider.displayName, ...meterSummary].join("; ");

  const className = [
    "plan-status-card",
    "menu-card",
    provider.error ? "plan-status-card--error menu-card--error" : null,
    freshness === "stale" ? "plan-status-card--stale menu-card--stale" : null,
    isRefreshing ? "plan-status-card--refreshing menu-card--refreshing" : null,
    onSelect ? "plan-status-card--interactive" : null,
  ]
    .filter(Boolean)
    .join(" ");

  const body = (
    <>
      <header className="plan-status-card__header">
        <ProviderIcon
          providerId={provider.providerId}
          size={30}
          className="plan-status-card__icon"
          title={provider.displayName}
        />
        <div className="plan-status-card__identity">
          <div className="plan-status-card__title-row">
            <span className="plan-status-card__name">{provider.displayName}</span>
            {/* Two rows of the same provider are otherwise indistinguishable:
                both read just "Codex". The account name is what tells them
                apart, so it outranks the plan for the limited space here. */}
            {accountName ? (
              <span
                className="plan-status-card__account"
                style={
                  provider.accountTint
                    ? { color: provider.accountTint }
                    : undefined
                }
                title={accountName}
              >
                {accountName}
              </span>
            ) : (
              planName && (
                <span className="plan-status-card__plan">{planName}</span>
              )
            )}
          </div>
          {!provider.error && (freshness === "stale" || resetCredits != null) && (
            <div className="plan-status-card__meta">
              {freshness === "stale" && (
                <span
                  className={`plan-status-card__chip plan-status-card__chip--${freshness}`}
                >
                  {t("FreshnessStale")}
                </span>
              )}
              {resetCredits != null && (
                <span
                  className={`plan-status-card__reset-credit${resetCredits === 0 ? " plan-status-card__reset-credit--empty" : ""}`}
                >
                  ↻{" "}
                  {resetCredits === 1
                    ? formatLocale(t("ResetCreditsAvailableOne"), String(resetCredits))
                    : formatLocale(t("ResetCreditsAvailableMany"), String(resetCredits))}
                </span>
              )}
            </div>
          )}
        </div>
      </header>

      {provider.error ? (
        <p className="plan-status-card__error">{provider.error}</p>
      ) : isHarness ? (
        <DeepSeekHarnessUsage rows={provider.extraRateWindows ?? []} compact />
      ) : (
        <div className="plan-status-card__meters">
          {meters.primary && (
            <MeterRow
              meter={meters.primary}
              showAsUsed={showAsUsed}
              resetTimeRelative={resetTimeRelative}
              showResetWhenExhausted={showResetWhenExhausted}
              hero
            />
          )}
          {meters.companions.map((meter) => (
            <MeterRow
              key={meter.id}
              meter={meter}
              showAsUsed={showAsUsed}
              resetTimeRelative={resetTimeRelative}
              showResetWhenExhausted={showResetWhenExhausted}
              hero={false}
            />
          ))}
          {notEnforcedSummary && (
            <div className="plan-status-card__inactive">
              <span className="plan-status-card__inactive-mark" aria-hidden />
              <span className="plan-status-card__inactive-name">
                {notEnforcedSummary}
              </span>
              <span>{t("NotCurrentlyEnforced")}</span>
            </div>
          )}
          {unavailableSummary && (
            <div className="plan-status-card__inactive plan-status-card__inactive--unavailable">
              <span className="plan-status-card__inactive-mark" aria-hidden />
              <span className="plan-status-card__inactive-name">
                {unavailableSummary}
              </span>
              <span>{t("WindowUnavailable")}</span>
            </div>
          )}
        </div>
      )}
    </>
  );

  if (onSelect) {
    return (
      <button
        type="button"
        className={className}
        style={{ "--plan-brand": brand } as CSSProperties}
        onClick={onSelect}
        aria-label={accessibleLabel}
        aria-busy={isRefreshing}
      >
        {body}
      </button>
    );
  }

  return (
    <article
      className={className}
      style={{ "--plan-brand": brand } as CSSProperties}
      aria-busy={isRefreshing}
    >
      {body}
    </article>
  );
}

/** Exported for tests — percent display helper. */
export function glanceDisplayPercent(
  snap: RateWindowSnapshot,
  showAsUsed: boolean,
): number {
  return Math.round(
    showAsUsed
      ? Math.max(0, Math.min(100, snap.usedPercent))
      : Math.max(0, Math.min(100, snap.remainingPercent)),
  );
}
import DeepSeekHarnessUsage, { harnessOverviewRows } from "./DeepSeekHarnessUsage";
