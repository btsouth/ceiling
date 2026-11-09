import { useCallback, useEffect, useRef, useState } from "react";
import type {
  PaceSnapshot,
  ProviderChartData,
  ProviderIncident,
  ProviderLocalUsageSummary,
  ProviderUsageSnapshot,
  RateWindowSnapshot,
  WindowAmountBridge,
} from "../types/bridge";
import { getProviderChartData, openExternalUrl } from "../lib/tauri";
import { useLocale } from "../hooks/useLocale";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { formatRelativeUpdated } from "../lib/relativeTime";
import type { LocaleKey } from "../i18n/keys";
import { paceCategory } from "../surfaces/tray/paceCategory";
import { SimpleBarChart, StackedBarChart } from "./MiniBarChart";
import { providerSupportsChartData } from "../lib/providerCharts";
import { getPaceBudget } from "../lib/paceBudget";
import PaceVerdict from "./PaceVerdict";
import {
  activePromoBoosts,
  activePromoInclusions,
} from "../lib/capacityPresentation";
import PaceDetailsChart from "./PaceDetailsChart";
import { ProviderIcon } from "./providers/ProviderIcon";
import { getProviderIcon } from "./providers/providerIcons";
import { accountIdentityLabel } from "../lib/providerRow";
import { maskEmail } from "../lib/privacy";
import DeepSeekHarnessUsage from "./DeepSeekHarnessUsage";

/** Small copy-to-clipboard button matching macOS CopyIconButton (doc.on.doc → checkmark). */
export function CopyIconButton({ text }: { text: string }) {
  const { t } = useLocale();
  const [state, setState] = useState<"idle" | "success" | "failure">("idle");
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const copyRequestRef = useRef(0);

  const handleCopy = useCallback(async () => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    const requestId = ++copyRequestRef.current;
    let next: "success" | "failure";
    try {
      await navigator.clipboard.writeText(text);
      next = "success";
    } catch {
      next = "failure";
    }
    if (requestId !== copyRequestRef.current) return;
    setState(next);
    timeoutRef.current = setTimeout(() => {
      if (requestId !== copyRequestRef.current) return;
      setState("idle");
    }, 900);
  }, [text]);

  useEffect(() => {
    return () => {
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
    };
  }, []);

  const label = state === "success"
    ? t("PanelCopied")
    : state === "failure"
      ? t("ActionCopyError")
      : t("ProviderIssueCopy");

  return (
    <button
      type="button"
      className="menu-card__copy-btn"
      onClick={handleCopy}
      aria-label={label}
      title={label}
    >
      {state === "success" ? "✓" : (
        <svg width="12" height="12" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
          <rect x="5" y="5" width="9" height="9" rx="1.5" stroke="currentColor" strokeWidth="1.5"/>
          <path d="M11 3V2.5A1.5 1.5 0 009.5 1H2.5A1.5 1.5 0 001 2.5v7A1.5 1.5 0 002.5 11H3" stroke="currentColor" strokeWidth="1.5"/>
        </svg>
      )}
    </button>
  );
}

interface MenuCardProps {
  provider: ProviderUsageSnapshot;
  hideEmail: boolean;
  resetTimeRelative: boolean;
  showResetWhenExhausted?: boolean;
  showAsUsed?: boolean;
  compactMetrics?: boolean;
  /** When false, hide local token activity (overview glance). Default true. */
  showActivitySection?: boolean;
  /** True when this provider has more than one account. */
  showAccount?: boolean;
  isRefreshing?: boolean;
  /** Live status-page incident for this provider, when one is being reported. */
  incident?: ProviderIncident | null;
  onLayoutChange?: () => void;
}

/** Localize raw provider window labels using the active locale. */
function localizeWindowLabel(
  raw: string | undefined,
  t: (key: LocaleKey) => string,
): string {
  if (raw?.trim().toLowerCase() === "weekly") {
    return t("ProviderWeeklyLabel");
  }
  return raw ?? "";
}

/** Format a reserve description from raw pace data at render time. */
function formatReserveDescription(
  snap: RateWindowSnapshot,
  t: (key: LocaleKey) => string,
): string | null {
  if (snap.reservePercent == null) return null;
  if (snap.reserveWillLastToReset) {
    return t("PanelReserveLastsUntilReset");
  }
  const eta = snap.reserveEtaSeconds;
  if (eta == null) return null;
  const h = Math.floor(eta / 3600);
  if (h >= 24) {
    return t("PanelReserveRunsOutInDaysHours")
      .replace("{}", String(Math.floor(h / 24)))
      .replace("{}", String(h % 24));
  }
  return t("PanelReserveRunsOutInHours").replace("{}", String(h));
}

function formatCurrency(amount: number, code: string): string {
  try {
    return new Intl.NumberFormat("en-US", {
      style: "currency",
      currency: code,
    }).format(amount);
  } catch {
    return `${code} ${amount.toFixed(2)}`;
  }
}

function formatCompactCount(value: number | null): string {
  if (value == null || value <= 0) return "—";
  return new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: value >= 1_000_000 ? 1 : 0,
  }).format(value);
}

function LocalUsageBlock({
  summary,
}: {
  summary: ProviderLocalUsageSummary;
}) {
  const breakdown = summary.sevenDayTokenBreakdown;
  const cachedTokens = breakdown
    ? breakdown.cacheReadTokens + breakdown.cacheWriteTokens
    : 0;
  const cacheShare = breakdown && breakdown.processedTokens > 0
    ? `${((cachedTokens / breakdown.processedTokens) * 100).toFixed(1)}%`
    : "—";

  return (
    <section className="menu-card__group menu-card__local-usage">
      <div className="menu-card__local-grid">
        <div>
          <span className="menu-card__local-label">Last session</span>
          <strong>{formatCompactCount(summary.lastSessionTokens)}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">Last 7 days</span>
          <strong>{formatCompactCount(summary.sevenDayTokens)}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">Last 30 days</span>
          <strong>{formatCompactCount(summary.thirtyDayTokens)}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">7-day cache share</span>
          <strong>{cacheShare}</strong>
        </div>
      </div>

      <div className="menu-card__local-note">
        {summary.topModel && <strong>Most used model: {summary.topModel}</strong>}
        <span>Processed tokens from local logs, including cache traffic.</span>
      </div>
    </section>
  );
}

function WayfinderUsageBlock({
  usage,
}: {
  usage: NonNullable<ProviderUsageSnapshot["wayfinderUsage"]>;
}) {
  const { t } = useLocale();
  const formatAmount = (value: number) =>
    usage.priced ? `${value.toFixed(4)} ${usage.unit.toUpperCase()}` : "—";

  return (
    <section className="menu-card__group">
      <div className="menu-card__local-grid">
        <div>
          <span className="menu-card__local-label">{t("WayfinderGatewayStatus")}</span>
          <strong>{usage.gatewayStatus}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("WayfinderModels")}</span>
          <strong>{usage.modelCount}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("WayfinderRequests")}</span>
          <strong>{formatCompactCount(usage.requests)}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("WayfinderTokens")}</span>
          <strong>{formatCompactCount(usage.tokens)}</strong>
        </div>
      </div>
      <div className="menu-card__cost-line">
        {t("WayfinderSaved")}: {formatAmount(usage.saved)} ({usage.savedPercent.toFixed(1)}%)
      </div>
      {(usage.offline || usage.dryRun || usage.missingKeys.length > 0) && (
        <div className="menu-card__local-note">
          {usage.offline && <span>{t("WayfinderOffline")}</span>}
          {usage.dryRun && <span>{t("WayfinderDryRun")}</span>}
          {usage.missingKeys.length > 0 && (
            <span>{t("WayfinderMissingKeys")}: {usage.missingKeys.join(", ")}</span>
          )}
        </div>
      )}
    </section>
  );
}

function displayPlanName(planName: string | null): string | null {
  if (!planName) return null;
  const normalized = planName.trim().toLowerCase();
  if (normalized === "default_claude_ai") return "Claude AI";
  return planName;
}

function paceStageKey(stage: PaceSnapshot["stage"]): LocaleKey {
  switch (stage) {
    case "on_track":
      return "DetailPaceOnTrack";
    case "slightly_ahead":
      return "DetailPaceSlightlyAhead";
    case "ahead":
      return "DetailPaceAhead";
    case "far_ahead":
      return "DetailPaceFarAhead";
    case "slightly_behind":
      return "DetailPaceSlightlyBehind";
    case "behind":
      return "DetailPaceBehind";
    case "far_behind":
      return "DetailPaceFarBehind";
    default:
      return "DetailPaceOnTrack";
  }
}

type UsageLevel = "normal" | "high" | "critical" | "exhausted";
const WEEKLY_WINDOW_MINUTES = 7 * 24 * 60;

function levelOf(remainPct: number, exhausted: boolean): UsageLevel {
  if (exhausted) return "exhausted";
  if (remainPct <= 5) return "critical";
  if (remainPct <= 25) return "high";
  return "normal";
}

interface MetricEntry {
  id: string;
  label: string;
  snap: RateWindowSnapshot;
  /** Money behind this lane, for providers that meter in currency. */
  amount?: WindowAmountBridge | null;
}

interface InactiveMetricEntry {
  id: string;
  label: string;
  description: string;
  inactive: true;
}

type MetricPaceView =
  | { kind: "budget"; budget: NonNullable<ReturnType<typeof getPaceBudget>> }
  | { kind: "reserve"; percent: number }
  | { kind: "none" };

function getMetricPaceView(snap: RateWindowSnapshot): MetricPaceView {
  if (snap.isExhausted) return { kind: "none" };

  const isWeeklyWindow =
    snap.windowMinutes != null && snap.windowMinutes >= WEEKLY_WINDOW_MINUTES;
  const budget = isWeeklyWindow ? getPaceBudget(snap) : null;
  if (budget) return { kind: "budget", budget };

  if (snap.reservePercent != null) {
    return { kind: "reserve", percent: snap.reservePercent };
  }

  return { kind: "none" };
}

/**
 * Single metric row inside the card — mirrors upstream `MetricRow`:
 *   • title (body / medium)
 *   • UsageProgressBar (capsule, 6pt)
 *   • HStack: "N% used"  ··  reset countdown (right-aligned, secondary)
 */
function MetricRow({
  title,
  snap,
  amount,
  exhaustedLabel,
  resetTimeRelative,
  showResetWhenExhausted,
  showAsUsed,
  expanded,
  onToggleExpanded,
}: {
  title: string;
  snap: RateWindowSnapshot;
  amount?: WindowAmountBridge | null;
  exhaustedLabel: string;
  resetTimeRelative: boolean;
  showResetWhenExhausted: boolean;
  showAsUsed: boolean;
  expanded: boolean;
  onToggleExpanded: () => void;
}) {
  const { t } = useLocale();
  const usedPct = Number.isFinite(snap.usedPercent) ? Math.max(0, snap.usedPercent) : 0;
  const barPct = Math.min(100, usedPct);
  const remain = 100 - usedPct;
  const displayPct = showAsUsed ? usedPct : Math.max(0, remain);
  const barDisplayPct = showAsUsed ? barPct : Math.max(0, Math.min(100, remain));
  const displayLabel = showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix");
  const level = levelOf(remain, snap.isExhausted);
  const resetText = useFormattedResetTime(
    snap.resetsAt,
    snap.resetDescription,
    resetTimeRelative,
  );
  const resetTarget = snap.resetsAt ? Date.parse(snap.resetsAt) : Number.NaN;
  const replacesPercent =
    showResetWhenExhausted &&
    snap.isExhausted &&
    Number.isFinite(resetTarget) &&
    resetTarget > Date.now() &&
    resetText !== null;
  const paceView = getMetricPaceView(snap);
  const reserveDescription = formatReserveDescription(snap, t);
  const formatBudget = (value: number) =>
    value < 10 ? value.toFixed(1).replace(/\.0$/, "") : Math.round(value).toString();
  return (
    <div className="menu-metric">
      <span className="menu-metric__title">{title}</span>
      <div className="menu-metric__bar">
        <div className="menu-metric__bar-fill" data-level={level} style={{ width: `${barDisplayPct}%` }} />
      </div>
      <div className="menu-metric__row">
        <span className="menu-metric__pct">
          {replacesPercent ? resetText : `${Math.round(displayPct)}% ${displayLabel}`}
        </span>
        {resetText && !replacesPercent && (
          <span className="menu-metric__reset">{resetText}</span>
        )}
      </div>
      {amount && (
        <div className="menu-metric__amount">
          {amount.formattedLimit
            ? `${amount.formattedUsed} ${t("PanelAmountOf")} ${amount.formattedLimit}`
            : amount.formattedUsed}
        </div>
      )}
      {snap.isExhausted && (
        <div className="menu-metric__exhausted">{exhaustedLabel}</div>
      )}
      {paceView.kind === "budget" && (
        <div className="menu-metric__budget">
          <button
            type="button"
            className="menu-metric__budget-header"
            onClick={onToggleExpanded}
            aria-expanded={expanded}
          >
            <span>{t("PanelOnPaceBudget")}</span>
            {reserveDescription && <span>{reserveDescription}</span>}
          </button>
          <div className="menu-metric__budget-pills">
            {[
              [t("PanelNow"), paceView.budget.now],
              [t("PanelOneHour"), paceView.budget.nextHour],
              [t("PanelFiveHours"), paceView.budget.nextFiveHours],
              [t("PanelTodayBudget"), paceView.budget.today],
            ].map(([label, value]) => (
              <span className="menu-metric__budget-pill" key={String(label)}>
                {label} {formatBudget(Number(value))}%
              </span>
            ))}
          </div>
          {expanded && <PaceDetailsChart snap={snap} />}
        </div>
      )}
      {paceView.kind === "reserve" && (
        <div className="menu-metric__row menu-metric__reserve">
          <span className="menu-metric__pct">{Math.round(paceView.percent)}% {t("PanelReserveSuffix")}</span>
          {reserveDescription && (
            <span className="menu-metric__reset">{reserveDescription}</span>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Provider card — direct mirror of SwiftUI `UsageMenuCardView`.
 *
 * Layout (top to bottom):
 *   1. Header VStack(spacing: 3)
 *        – HStack: providerName (headline/semibold)  ··  email (subheadline/secondary, right)
 *        – HStack: subtitle "source · updated"        ··  plan (footnote/secondary, right)
 *   2. Divider (1pt)
 *   3. VStack(spacing: 12)
 *        – Metrics group VStack(spacing: 12) of MetricRow
 *        – (Divider) Cost group: title (body/medium) + session line + month line (footnote)
 *        – (Divider) Pace group (Tauri-only addition; placed last)
 *        – (Divider) Charts group (Tauri-only addition; placed last)
 *
 * Padding: upstream v0.32.2 uses wider horizontal card padding and slightly
 * taller header/content vertical padding so account/plan rows can breathe.
 */
export default function MenuCard({
  provider,
  hideEmail,
  resetTimeRelative,
  showResetWhenExhausted = false,
  showAsUsed = false,
  compactMetrics: _compactMetrics = false,
  showActivitySection = true,
  showAccount = false,
  isRefreshing = false,
  incident = null,
  onLayoutChange,
}: MenuCardProps) {
  const { t } = useLocale();
  const [chartData, setChartData] = useState<ProviderChartData | null>(null);
  const [expandedPaceWindow, setExpandedPaceWindow] = useState<string | null>(null);
  const formattedCostReset = useFormattedResetTime(
    provider.cost?.resetsAt ?? null,
    null,
    resetTimeRelative,
  );

  useEffect(() => {
    if (!providerSupportsChartData(provider.providerId)) {
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
        if (!cancelled) {
          setChartData(data);
          requestAnimationFrame(() => onLayoutChange?.());
        }
      })
      .catch(() => {
        /* chart data is best-effort */
      });
    return () => {
      cancelled = true;
    };
  }, [provider.providerId, provider.accountEmail, provider.accountId, provider.accountOrganization, onLayoutChange]);

  const isWayfinder = provider.providerId === "wayfinder";
  const isDsh = provider.providerId === "deepseekharness";
  const rawEmail = !isWayfinder && provider.accountEmail
    ? hideEmail
      ? maskEmail(provider.accountEmail)
      : provider.accountEmail
    : null;
  const planName = !isWayfinder ? displayPlanName(provider.planName) : null;
  // The account's email, shown only when several accounts share this provider.
  const accountName =
    !isWayfinder && showAccount
      ? accountIdentityLabel(provider, hideEmail)
      : null;
  // The account line already carries the email, so do not also print the raw
  // email row below it.
  const email = accountName ? null : rawEmail;

  const metrics: Array<MetricEntry | InactiveMetricEntry> = [
    ...(isWayfinder
      ? []
      : [
          {
            id: "primary",
            label: provider.primaryLabel ?? t("DetailWindowPrimary"),
            snap: provider.primary,
          },
        ]),
  ];
  if (provider.secondary)
    metrics.push({
      id: "secondary",
      label: localizeWindowLabel(provider.secondaryLabel, t) || t("DetailWindowSecondary"),
      snap: provider.secondary,
    });
  if (provider.modelSpecific)
    metrics.push({
      id: "model-specific",
      label: t("DetailWindowModelSpecific"),
      snap: provider.modelSpecific,
    });
  if (provider.tertiary)
    metrics.push({
      id: "tertiary",
      label: t("DetailWindowTertiary"),
      snap: provider.tertiary,
    });
  for (const extra of provider.extraRateWindows ?? []) {
    metrics.push({
      id: `extra-${extra.id}`,
      label: extra.title,
      snap: extra.window,
      amount: extra.amount,
    });
  }
  for (const inactive of provider.inactiveRateWindows ?? []) {
    metrics.push({
      id: `inactive-${inactive.id}`,
      label: inactive.title,
      description: inactive.description,
      inactive: true,
    });
  }
  const visibleMetrics = isDsh ? [] : metrics;

  const hasCreditsHistory =
    chartData !== null && chartData.creditsHistory.length > 0;
  const hasUsageBreakdown =
    chartData !== null && chartData.usageBreakdown.length > 0;
  const hasCharts =
    showActivitySection &&
    (hasCreditsHistory || hasUsageBreakdown);
  const localUsage =
    showActivitySection && !provider.error ? chartData?.localUsage ?? null : null;
  const promoBoosts = activePromoBoosts(provider);
  const promoInclusions = activePromoInclusions(provider);
  const hasMetrics = visibleMetrics.length > 0;
  const hasCost = showActivitySection && !!provider.cost;
  const hasPace = !!provider.pace;
  const wayfinderUsage = isWayfinder ? provider.wayfinderUsage : null;
  const hasDetails =
    !provider.error &&
    (hasMetrics || hasCost || hasPace || hasCharts || !!localUsage || !!wayfinderUsage || isDsh || promoBoosts.length > 0);
  const cardAgeMs = Date.parse(provider.updatedAt);
  const isStale =
    !provider.error &&
    Number.isFinite(cardAgeMs) &&
    Date.now() - cardAgeMs > 10 * 60 * 1000;
  const cardClassName = [
    "menu-card",
    provider.error ? "menu-card--error" : null,
    isStale ? "menu-card--stale" : null,
    isRefreshing ? "menu-card--refreshing" : null,
    hasDetails ? "menu-card--with-details" : "menu-card--header-only",
  ]
    .filter(Boolean)
    .join(" ");
  const brandColor = getProviderIcon(provider.providerId).brandColor;

  return (
    <article
      className={cardClassName}
      aria-busy={isRefreshing}
      style={{ ["--plan-brand" as string]: brandColor }}
    >
      <header className="menu-card__header">
        <div className="menu-card__title-row">
          <ProviderIcon
            providerId={provider.providerId}
            size={28}
            className="menu-card__provider-icon"
            title={provider.displayName}
          />
          <div className="menu-card__name-group">
            <span className="menu-card__name">{provider.displayName}</span>
            {accountName && (
              <span
                className="menu-card__account"
                style={
                  provider.accountTint
                    ? { color: provider.accountTint }
                    : undefined
                }
                title={accountName}
              >
                {accountName}
              </span>
            )}
            {!provider.error && email && <span className="menu-card__email">{email}</span>}
          </div>
        </div>
        {/* Above the error/subtitle branch on purpose. A provider that is
            mid-outage often fails its fetch too, and the error block replaces
            the whole meta row — which is precisely when knowing it is an
            outage rather than a spent cap matters most. */}
        {incident && (
          <div
            className="menu-card__incident"
            data-severity={incident.severity}
            role="status"
          >
            <span className="menu-card__incident-dot" aria-hidden />
            <span className="menu-card__incident-text">{incident.description}</span>
            {/* A button, not the raw URL as text. The tray is 328px wide and a
                nowrap URL took most of it from the description, which is the
                part worth reading — and it could not be clicked anyway.

                Opens the URL the incident itself carries, not the provider
                registry's. The two disagree: Cursor is polled through
                `get_status_page_url` but its ProviderMetadata sets
                `status_page_url: None`, so routing through the registry left
                this button silently dead for Cursor. The incident's URL is the
                page the badge is actually reporting on. */}
            <button
              type="button"
              className="menu-card__incident-link"
              title={incident.statusPageUrl}
              onClick={() => {
                void openExternalUrl(incident.statusPageUrl);
              }}
            >
              {t("IncidentStatusPage")}
            </button>
          </div>
        )}
        {provider.error ? (
          <div className="menu-card__error-block">
            <div className="menu-card__error-text">{provider.error}</div>
            <CopyIconButton text={provider.error} />
          </div>
        ) : (
          <div className="menu-card__subtitle-row">
            <span className="menu-card__subtitle">
              {Number.isNaN(Date.parse(provider.updatedAt))
                ? provider.updatedAt
                : formatRelativeUpdated(Date.parse(provider.updatedAt), t)}
            </span>
            <div className="menu-card__header-meta">
              {promoBoosts.map((promo) => (
                <span
                  key={promo.id}
                  className="menu-card__promo-chip menu-card__promo-chip--boost"
                  title={promo.description}
                >
                  {promo.title}
                </span>
              ))}
              {promoInclusions.map((promo) => (
                <span
                  key={promo.id}
                  className="menu-card__promo-chip menu-card__promo-chip--inclusion"
                  title={promo.description}
                >
                  {promo.title}
                </span>
              ))}
              {/* The account line already shows the plan, so drop the badge
                  when an account is named to avoid printing it twice. */}
              {planName && !accountName && (
                <span className="menu-card__plan-badge">{planName}</span>
              )}
            </div>
          </div>
        )}
      </header>

      {hasDetails && <div className="menu-card__divider" />}

      {hasDetails && (
        <div className="menu-card__content">
          {!provider.error && hasMetrics && (
            <section className="menu-card__group menu-card__metrics">
              {visibleMetrics.map((m) =>
                "inactive" in m ? (
                  <div className="menu-metric menu-metric--inactive" key={m.id}>
                    <span className="menu-metric__title">{m.label}</span>
                    <strong className="menu-metric__inactive-label">
                      Not currently enforced
                    </strong>
                    <span className="menu-metric__reset">{m.description}</span>
                  </div>
                ) : (
                  <MetricRow
                    key={m.id}
                    title={m.label}
                    snap={m.snap}
                    amount={m.amount}
                    exhaustedLabel={t("DetailWindowExhausted")}
                    resetTimeRelative={resetTimeRelative}
                    showResetWhenExhausted={showResetWhenExhausted}
                    showAsUsed={showAsUsed}
                    expanded={expandedPaceWindow === m.id}
                    onToggleExpanded={() => {
                      setExpandedPaceWindow((current) =>
                        current === m.id ? null : m.id,
                      );
                      requestAnimationFrame(() => onLayoutChange?.());
                    }}
                  />
                ),
              )}
            </section>
          )}

          {wayfinderUsage && <WayfinderUsageBlock usage={wayfinderUsage} />}
          {isDsh && <DeepSeekHarnessUsage rows={provider.extraRateWindows ?? []} />}

          {localUsage && (
            <LocalUsageBlock
              summary={localUsage}
            />
          )}

          {hasMetrics && hasCost && <div className="menu-card__divider" />}

          {provider.cost && (
            <section className="menu-card__group menu-card__cost">
              <div className="menu-card__group-title">
                {t("DetailCostTitle")} — {provider.cost.period}
              </div>
              <div className="menu-card__cost-line">
                {t("DetailCostUsed")}:{" "}
                {provider.cost.formattedUsed ||
                  formatCurrency(provider.cost.used, provider.cost.currencyCode)}
                {provider.cost.limit != null && (
                  <>
                    {" / "}
                    {provider.cost.formattedLimit ||
                      formatCurrency(provider.cost.limit, provider.cost.currencyCode)}
                  </>
                )}
              </div>
              {provider.cost.remaining != null && (
                <div className="menu-card__cost-line menu-card__cost-line--muted">
                  {t("DetailCostRemaining")}:{" "}
                  {formatCurrency(provider.cost.remaining, provider.cost.currencyCode)}
                </div>
              )}
              {formattedCostReset && (
                <div className="menu-card__cost-line menu-card__cost-line--muted">
                  {t("DetailCostResets")}: {formattedCostReset}
                </div>
              )}
            </section>
          )}

          {(hasMetrics || hasCost) && hasPace && <div className="menu-card__divider" />}

          {provider.pace && (
            <section className="menu-card__group menu-card__pace">
              <div className="menu-card__pace-header">
                <span className="menu-card__group-title">{t("DetailPaceTitle")}</span>
                <span
                  className="menu-card__pace-label"
                  data-pace={paceCategory(provider.pace.stage)}
                >
                  {t(paceStageKey(provider.pace.stage))} (
                  {provider.pace.deltaPercent >= 0 ? "+" : ""}
                  {provider.pace.deltaPercent.toFixed(1)}%)
                </span>
              </div>
              <div className="menu-card__pace-bars">
                <div className="menu-card__pace-track" title={t("PanelExpected")}>
                  <div
                    className="menu-card__pace-fill menu-card__pace-fill--expected"
                    style={{ width: `${provider.pace.expectedUsedPercent.toFixed(1)}%` }}
                  />
                </div>
                <div className="menu-card__pace-track" title={t("PanelActual")}>
                  <div
                    className="menu-card__pace-fill"
                    data-pace={paceCategory(provider.pace.stage)}
                    style={{ width: `${provider.pace.actualUsedPercent.toFixed(1)}%` }}
                  />
                </div>
              </div>
              <PaceVerdict pace={provider.pace} />
            </section>
          )}

          {(hasMetrics || hasCost || hasPace) && hasCharts && (
            <div className="menu-card__divider" />
          )}

          {hasCharts && (
            <section className="menu-card__group menu-card__charts">
              {hasCreditsHistory && (
                <SimpleBarChart
                  points={chartData!.creditsHistory}
                  label={t("DetailChartCredits")}
                  color="var(--provider-status-ok)"
                  formatValue={(v) => v.toFixed(1)}
                />
              )}
              {hasUsageBreakdown && (
                <StackedBarChart
                  points={chartData!.usageBreakdown}
                  label={t("DetailChartUsageBreakdown")}
                  height={56}
                />
              )}
            </section>
          )}
        </div>
      )}
    </article>
  );
}
