import { useEffect, useMemo, useState } from "react";
import type {
  PaceSnapshot,
  ProviderChartData,
  ProviderLocalUsageSummary,
  ProviderUsageSnapshot,
  RateWindowSnapshot,
  WindowAmountBridge,
} from "../types/bridge";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import { expectedOverlay } from "../lib/expectedPace";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { useLocale } from "../hooks/useLocale";
import { formatLocale } from "../lib/formatLocale";
import { formatRelativeUpdated } from "../lib/relativeTime";
import { getProviderChartData } from "../lib/tauri";
import { providerSupportsChartData } from "../lib/providerCharts";
import { accountIdentityLabel } from "../lib/providerRow";
import DeepSeekHarnessUsage from "../components/DeepSeekHarnessUsage";
import {
  allMeasuredWindows,
  bankedResetCredits,
  formatShortDuration,
  isPrimaryPlaceholderId,
  primaryNamedState,
} from "../lib/capacityPresentation";

type DetailWindow = {
  id: string;
  label: string;
  window: RateWindowSnapshot;
  amount?: WindowAmountBridge | null;
};

interface ProviderDetailViewProps {
  provider: ProviderUsageSnapshot;
  resetTimeRelative: boolean;
  showAsUsed: boolean;
  isRefreshing?: boolean;
  hideEmail?: boolean;
}

function displayPlanName(planName: string | null): string | null {
  if (!planName) return null;
  if (planName.trim().toLowerCase() === "default_claude_ai") return "Claude AI";
  return planName;
}

function formatCompactCount(value: number | null): string {
  if (value == null || value <= 0) return "—";
  return new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: value >= 1_000_000 ? 1 : 0,
  }).format(value);
}

function usageLevel(window: RateWindowSnapshot): string {
  if (window.isExhausted) return "exhausted";
  if (window.remainingPercent <= 5) return "critical";
  return "normal";
}

function paceLabel(stage: PaceSnapshot["stage"]): string {
  switch (stage) {
    case "far_ahead":
      return "Far ahead of budget";
    case "ahead":
      return "Ahead of budget";
    case "slightly_ahead":
      return "Slightly ahead of budget";
    case "far_behind":
      return "Far below budget";
    case "behind":
      return "Below budget";
    case "slightly_behind":
      return "Slightly below budget";
    default:
      return "On pace";
  }
}

function paceTone(pace: PaceSnapshot): "risk" | "calm" {
  return pace.deltaPercent > 5 ? "risk" : "calm";
}

function percentFor(window: RateWindowSnapshot, showAsUsed: boolean): number {
  const used = Math.max(0, Math.min(100, window.usedPercent));
  return showAsUsed ? used : 100 - used;
}

function DetailProgress({
  window,
  showAsUsed,
}: {
  window: RateWindowSnapshot;
  showAsUsed: boolean;
}) {
  const { t } = useLocale();
  // Where usage should be by this point in the window. Long windows only:
  // see `expectedUsedPercent`.
  const expected = expectedOverlay(window, showAsUsed);
  return (
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
      <div className="provider-focus__progress">
        <div
          className="provider-focus__progress-fill"
          data-level={usageLevel(window)}
          style={{ width: `${percentFor(window, showAsUsed)}%` }}
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
  );
}

function SecondaryWindow({
  metric,
  resetTimeRelative,
  showAsUsed,
}: {
  metric: DetailWindow;
  resetTimeRelative: boolean;
  showAsUsed: boolean;
}) {
  const reset = useFormattedResetTime(
    metric.window.resetsAt,
    metric.window.resetDescription,
    resetTimeRelative,
  );
  const { t } = useLocale();
  const percent = Math.round(percentFor(metric.window, showAsUsed));
  return (
    <div className="provider-focus__limit-row">
      <div className="provider-focus__limit-head">
        <strong>{metric.label}</strong>
        <span className="provider-focus__limit-value">
          <b>{percent}%</b> {showAsUsed ? "used" : "left"}
        </span>
        {reset && <span className="provider-focus__limit-reset">{reset}</span>}
      </div>
      {metric.amount && (
        <div className="provider-focus__limit-amount">
          {metric.amount.formattedLimit
            ? `${metric.amount.formattedUsed} ${t("PanelAmountOf")} ${metric.amount.formattedLimit}`
            : metric.amount.formattedUsed}
        </div>
      )}
      <DetailProgress window={metric.window} showAsUsed={showAsUsed} />
    </div>
  );
}

function LocalActivity({ summary }: { summary: ProviderLocalUsageSummary }) {
  const breakdown = summary.sevenDayTokenBreakdown;
  const cached = breakdown
    ? breakdown.cacheReadTokens + breakdown.cacheWriteTokens
    : 0;
  const cacheShare =
    breakdown && breakdown.processedTokens > 0
      ? `${((cached / breakdown.processedTokens) * 100).toFixed(1)}%`
      : "—";
  const stats = [
    ["Last session", formatCompactCount(summary.lastSessionTokens), "processed"],
    ["Last 7 days", formatCompactCount(summary.sevenDayTokens), "processed"],
    ["Last 30 days", formatCompactCount(summary.thirtyDayTokens), "processed"],
    ["Cache share", cacheShare, ""],
  ];
  return (
    <section className="provider-focus__section">
      <h3>Local activity</h3>
      <div className="provider-focus__stats">
        {stats.map(([label, value, suffix]) => (
          <div key={label}>
            <span>{label}</span>
            <strong>{value}</strong>
            {suffix && <small>{suffix}</small>}
          </div>
        ))}
      </div>
      <div className="provider-focus__source-note">
        Local logs
        {summary.topModel && <> · Most used model: {summary.topModel}</>}
      </div>
    </section>
  );
}

function PaceSection({ pace }: { pace: PaceSnapshot }) {
  const tone = paceTone(pace);
  const expected = Math.max(0, Math.min(100, pace.expectedUsedPercent));
  const actual = Math.max(0, Math.min(100, pace.actualUsedPercent));
  return (
    <section className="provider-focus__section provider-focus__pace">
      <div className="provider-focus__section-head">
        <h3>{pace.windowLabel} pace</h3>
        <span data-tone={tone}>
          {paceLabel(pace.stage)} ({pace.deltaPercent >= 0 ? "+" : ""}
          {pace.deltaPercent.toFixed(1)}%)
        </span>
      </div>
      <div className="provider-focus__pace-track" aria-hidden>
        <div
          className="provider-focus__pace-fill"
          data-tone={tone}
          style={{ width: `${actual}%` }}
        />
        <i style={{ left: `${expected}%` }} />
      </div>
      <div className="provider-focus__pace-legend">
        <span>Expected {Math.round(expected)}% by now</span>
        <span>Actual {Math.round(actual)}%</span>
      </div>
      {!pace.willLastToReset &&
        typeof pace.etaSeconds === "number" &&
        Number.isFinite(pace.etaSeconds) &&
        pace.etaSeconds > 0 && (
          <p className="provider-focus__pace-eta" data-tone={tone}>
            At this pace,{" "}
            {pace.etaSeconds < 60
              ? "under a minute"
              : `about ${formatShortDuration(pace.etaSeconds)}`}{" "}
            left before this window runs out.
          </p>
        )}
    </section>
  );
}

export default function ProviderDetailView({
  provider,
  resetTimeRelative,
  showAsUsed,
  isRefreshing = false,
  hideEmail = false,
}: ProviderDetailViewProps) {
  const { t } = useLocale();
  const [chartData, setChartData] = useState<ProviderChartData | null>(null);
  const supportsLocalActivity = providerSupportsChartData(provider.providerId);
  useEffect(() => {
    if (!supportsLocalActivity || provider.error) {
      setChartData(null);
      return;
    }
    let cancelled = false;
    setChartData(null);
    getProviderChartData(
      provider.providerId,
      provider.accountEmail ?? undefined,
      provider.accountId ?? undefined,
      undefined,
      undefined,
      provider.accountOrganization ?? undefined,
    )
      .then((data) => {
        if (!cancelled) setChartData(data);
      })
      .catch(() => {
        if (!cancelled) setChartData(null);
      });
    return () => {
      cancelled = true;
    };
  }, [
    provider.accountEmail,
    provider.accountId,
    provider.accountOrganization,
    provider.error,
    provider.providerId,
    supportsLocalActivity,
  ]);

  const primaryReset = useFormattedResetTime(
    provider.primary.resetsAt,
    provider.primary.resetDescription,
    resetTimeRelative,
  );
  const metrics = useMemo(
    () =>
      // Other limits = every measured window except the hero primary.
      // Filter by id, not `.slice(1)`: when the primary is a named
      // placeholder it is already omitted, and slicing would drop Auto
      // (SBS-876).
      allMeasuredWindows(provider).filter((window) => window.id !== "primary"),
    [provider],
  );
  const namedPrimary = primaryNamedState(provider);
  const otherInactiveWindows = (provider.inactiveRateWindows ?? []).filter(
    (metric) => !namedPrimary || !isPrimaryPlaceholderId(metric.id),
  );
  // Pace belongs to whichever long window has the worst delta, not always the
  // primary (`preferred_pace` in bridge.rs picks across all of them). Drop it
  // only when it is the placeholder's own verdict, which would read a fake 0%;
  // an Auto pace beside an unavailable Plan is still a real reading (SBS-876).
  const rawPrimaryLabel = provider.primaryLabel?.trim() ?? "";
  const primaryLabel = rawPrimaryLabel || "Primary";
  const paceWindow = provider.pace?.windowLabel?.trim() ?? "";
  const pace =
    namedPrimary &&
    !!rawPrimaryLabel &&
    !!paceWindow &&
    paceWindow === rawPrimaryLabel
      ? null
      : provider.pace;
  const primaryPercent = Math.round(percentFor(provider.primary, showAsUsed));
  const planName = displayPlanName(provider.planName);
  // A drill-in opens a specific account, so name it whenever there is an email.
  // Without this, two accounts on one provider open two identical detail views.
  const accountName = accountIdentityLabel(provider, hideEmail);
  const resetCredits = bankedResetCredits(provider);
  const updated = Number.isNaN(Date.parse(provider.updatedAt))
    ? provider.updatedAt
    : formatRelativeUpdated(Date.parse(provider.updatedAt), t);
  const updatedLabel = /^updated\b/i.test(updated) ? updated : `Updated ${updated}`;

  return (
    <article className="provider-focus" aria-busy={isRefreshing}>
      <header className="provider-focus__identity">
        <ProviderIcon
          providerId={provider.providerId}
          size={42}
          className="provider-focus__icon"
          title={provider.displayName}
        />
        <div>
          <h2>{provider.displayName}</h2>
          {accountName && (
            <span
              className="provider-focus__account"
              style={
                provider.accountTint ? { color: provider.accountTint } : undefined
              }
              title={accountName}
            >
              {accountName}
            </span>
          )}
          <span>{updatedLabel}</span>
        </div>
        <div className="provider-focus__badges">
          {planName && <span className="provider-focus__plan">{planName}</span>}
          {resetCredits != null && (
            <span
              className={`provider-focus__reset-credit${resetCredits === 0 ? " provider-focus__reset-credit--empty" : ""}`}
            >
              ↻{" "}
              {resetCredits === 1
                ? formatLocale(t("ResetCreditsAvailableOne"), String(resetCredits))
                : formatLocale(t("ResetCreditsAvailableMany"), String(resetCredits))}
            </span>
          )}
        </div>
      </header>

      {provider.error ? (
        <section className="provider-focus__error">
          <strong>Usage unavailable</strong>
          <span>{provider.error}</span>
        </section>
      ) : provider.providerId === "deepseekharness" ? (
        <DeepSeekHarnessUsage rows={provider.extraRateWindows ?? []} />
      ) : (
        <>
          <section className="provider-focus__primary">
            <div className="provider-focus__section-head">
              <h3>{primaryLabel} usage</h3>
              {primaryReset && !namedPrimary && <span>{primaryReset}</span>}
            </div>
            {namedPrimary ? (
              <div
                className={`provider-focus__primary-value provider-focus__primary-value--named${namedPrimary === "unavailable" ? " provider-focus__primary-value--unavailable" : ""}`}
              >
                <strong>
                  {namedPrimary === "unavailable"
                    ? t("WindowUnavailable")
                    : t("NotCurrentlyEnforced")}
                </strong>
              </div>
            ) : (
              <>
                <div className="provider-focus__primary-value">
                  <strong>{primaryPercent}%</strong>
                  <span>{showAsUsed ? "used" : "left"}</span>
                </div>
                <DetailProgress window={provider.primary} showAsUsed={showAsUsed} />
              </>
            )}
            {pace && (
              <div className="provider-focus__pace-glance" data-tone={paceTone(pace)}>
                <i />
                <strong>{pace.windowLabel} pace</strong>
                <span>
                  {paceLabel(pace.stage)} ·{" "}
                  {pace.deltaPercent >= 0 ? "+" : ""}
                  {pace.deltaPercent.toFixed(1)}%
                </span>
              </div>
            )}
          </section>

          {(metrics.length > 0 || otherInactiveWindows.length > 0) && (
            <section className="provider-focus__section provider-focus__limits">
              <h3>Other limits</h3>
              {metrics.map((metric) => (
                <SecondaryWindow
                  key={metric.id}
                  metric={metric}
                  resetTimeRelative={resetTimeRelative}
                  showAsUsed={showAsUsed}
                />
              ))}
              {otherInactiveWindows.map((metric) => {
                const unavailable = metric.state === "unavailable";
                return (
                  <div
                    className={`provider-focus__inactive${unavailable ? " provider-focus__inactive--unavailable" : ""}`}
                    key={metric.id}
                    title={metric.description || undefined}
                  >
                    <strong>{metric.title}</strong>
                    <span className="provider-focus__info" aria-hidden>i</span>
                    <span>
                      {unavailable ? t("WindowUnavailable") : t("NotCurrentlyEnforced")}
                    </span>
                  </div>
                );
              })}
            </section>
          )}

          {supportsLocalActivity && chartData === null && (
            <section className="provider-focus__section provider-focus__local-loading">
              <h3>Local activity</h3>
              <span>Reading local logs…</span>
            </section>
          )}
          {chartData?.localUsage && <LocalActivity summary={chartData.localUsage} />}
          {pace && <PaceSection pace={pace} />}
        </>
      )}
    </article>
  );
}
