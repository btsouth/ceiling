import { useEffect, useState } from "react";
import type { ProviderDetail } from "../../../../types/bridge";
import type { LocaleKey } from "../../../../i18n/keys";
import {
  providerLoginPhaseKey,
  type ProviderLoginPhase,
} from "../../../../lib/providerLogin";
import { CopyIconButton } from "../../../../components/MenuCard";
import { openExternalUrl } from "../../../../lib/tauri";

interface Props {
  provider: ProviderDetail;
  busy: boolean;
  loginPhase: ProviderLoginPhase | null;
  loginCode: string | null;
  loginUrl: string | null;
  onRefresh: () => void;
  onSwitchAccount: () => void;
  onOpenDashboard: () => void;
  onOpenStatusPage: () => void;
  onCopyError: () => void;
  onBuyCredits: () => void;
  t: (key: LocaleKey) => string;
}

/**
 * Quick-action toolbar. Mirrors the six buttons in
 * `rust/src/native_ui/preferences.rs::render_provider_detail_panel`.
 * Buttons that have no backing URL on the provider are omitted entirely
 * (egui parity: the button only renders when the action is meaningful).
 */
export function QuickActionsSection({
  provider,
  busy,
  loginPhase,
  loginCode,
  loginUrl,
  onRefresh,
  onSwitchAccount,
  onOpenDashboard,
  onOpenStatusPage,
  onCopyError,
  onBuyCredits,
  t,
}: Props) {
  const loginStatusKey = providerLoginPhaseKey(loginPhase);
  const [linkError, setLinkError] = useState<string | null>(null);

  // ProviderDetailPane doesn't remount this component between login
  // attempts — it just pushes new props — so a stale error from a previous
  // attempt must be cleared explicitly once a new one starts. Keyed on the
  // code rather than the URL: GitHub's plain verification_uri is a
  // constant, so two different attempts could share the same URL even
  // though the code (and thus the attempt) differs.
  useEffect(() => {
    setLinkError(null);
  }, [loginCode]);

  const handleOpenLoginUrl = () => {
    if (!loginUrl) return;
    setLinkError(null);
    void openExternalUrl(loginUrl).catch((err: unknown) =>
      setLinkError(err instanceof Error ? err.message : String(err)),
    );
  };

  return (
    <section className="provider-detail-section">
      <h4>{t("QuickActions")}</h4>
      {loginStatusKey && (
        <p className="settings-status" role="status">
          {t(loginStatusKey)}
          {loginPhase === "waitingBrowser" && loginCode && (
            <>
              {" "}
              {t("LoginPhaseEnterGithubCodePrefix")}{" "}
              <strong className="settings-status__code">{loginCode}</strong>{" "}
              <CopyIconButton text={loginCode} />
              {loginUrl && (
                <>
                  {" "}
                  <button
                    type="button"
                    className="provider-detail-datasource__link"
                    onClick={handleOpenLoginUrl}
                  >
                    {t("LoginPhaseOpenVerificationLink")}
                  </button>
                  {linkError && (
                    <span className="settings-status--error"> {linkError}</span>
                  )}
                </>
              )}
            </>
          )}
        </p>
      )}
      <div className="provider-detail-actions">
        <button
          type="button"
          className="btn btn--ghost"
          onClick={onRefresh}
          disabled={busy}
        >
          {t("ActionRefresh")}
        </button>
        {provider.supportsLogin && (
          <button
            type="button"
            className="btn btn--ghost"
            onClick={onSwitchAccount}
            disabled={busy}
          >
            {loginStatusKey && busy
              ? t(loginStatusKey)
              : t("ActionSwitchAccount")}
          </button>
        )}
        {provider.dashboardUrl && (
          <button
            type="button"
            className="btn btn--ghost"
            onClick={onOpenDashboard}
          >
            {t("ActionUsageDashboard")}
          </button>
        )}
        {provider.statusPageUrl && (
          <button
            type="button"
            className="btn btn--ghost"
            onClick={onOpenStatusPage}
          >
            {t("ActionStatusPage")}
          </button>
        )}
        {provider.lastError && (
          <button
            type="button"
            className="btn btn--ghost"
            onClick={onCopyError}
          >
            {t("ActionCopyError")}
          </button>
        )}
        {provider.buyCreditsUrl && (
          <button
            type="button"
            className="btn btn--ghost"
            onClick={onBuyCredits}
          >
            {t("ActionBuyCredits")}
          </button>
        )}
      </div>
    </section>
  );
}
