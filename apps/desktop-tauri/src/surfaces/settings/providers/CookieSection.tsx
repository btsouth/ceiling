import { useCallback, useEffect, useState } from "react";
import {
  getManualCookies,
  removeManualCookie,
  setManualCookie,
} from "../../../lib/tauri";
import { formatLocale } from "../../../lib/formatLocale";
import { useLocale } from "../../../hooks/useLocale";
import { ConfirmDialog } from "../../../components/ConfirmDialog";
import { SecretField } from "../../../components/SecretField";
import type { CookieInfoBridge } from "../../../types/bridge";

interface Props {
  providerId: string;
  providerName?: string;
  cookieDomain: string | null;
  onCredentialsChanged?: () => void;
}

export function cookiePlaceholder(
  providerId: string,
  t: ReturnType<typeof useLocale>["t"],
): string {
  if (providerId === "ollama") {
    return t("BrowserCookiePlaceholderOllama");
  }
  if (providerId === "t3chat") {
    return t("BrowserCookiePlaceholderCurl");
  }
  return t("BrowserCookiePlaceholderDefault");
}

/**
 * Per-provider browser cookie management. Renders nothing for providers
 * that do not have a cookieDomain (i.e. don't authenticate via web cookies).
 */
export function CookieSection({
  providerId,
  providerName,
  cookieDomain,
  onCredentialsChanged,
}: Props) {
  const { t } = useLocale();
  const displayName = providerName || providerId;
  const [saved, setSaved] = useState<CookieInfoBridge | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);

  const [pasteValue, setPasteValue] = useState("");

  const reload = useCallback(async (signal: { stale: boolean }) => {
    try {
      const cookies = await getManualCookies();
      if (signal.stale) return;
      setSaved(cookies.find((c) => c.providerId === providerId) ?? null);
    } catch (err: unknown) {
      if (signal.stale) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (!signal.stale) setLoaded(true);
    }
  }, [providerId]);

  useEffect(() => {
    if (cookieDomain === null) return;
    const signal = { stale: false };
    setLoaded(false);
    setError(null);
    setStatus(null);
    setPasteValue("");
    setSaved(null);
    setConfirming(false);
    void reload(signal);
    return () => { signal.stale = true; };
  }, [reload, cookieDomain]);

  if (cookieDomain === null) return null;
  if (!loaded) return null;

  const handleRemove = async () => {
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const next = await removeManualCookie(providerId);
      setSaved(next.find((c) => c.providerId === providerId) ?? null);
      setConfirming(false);
      setStatus(t("CredentialRemoved"));
      onCredentialsChanged?.();
    } catch (err: unknown) {
      setConfirming(false);
      setError(err instanceof Error ? err.message : t("CredentialRemoveFailed"));
    } finally {
      setBusy(false);
    }
  };

  const handlePaste = async () => {
    if (!pasteValue.trim()) return;
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const next = await setManualCookie(providerId, pasteValue.trim());
      setSaved(next.find((c) => c.providerId === providerId) ?? null);
      setPasteValue("");
      onCredentialsChanged?.();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="provider-detail-section">
      <h4>{t("BrowserCookiesSectionTitle")}</h4>

      {error && (
        <div className="settings-status settings-status--error" role="alert">
          {error}
        </div>
      )}
      {status && (
        <div className="settings-status" role="status">
          {status}
        </div>
      )}

      {saved ? (
        <ul className="credential-list">
          <li className="credential-card">
            <div className="credential-card__header">
              <div className="credential-card__info">
                <span className="credential-card__meta">
                  <span className="credential-card__badge credential-card__badge--set">
                    {t("BrowserCookieSavedBadge")}
                  </span>
                  <span className="credential-card__date">{saved.savedAt}</span>
                </span>
              </div>
              <div className="credential-card__actions">
                <button
                  className="credential-btn credential-btn--danger"
                  disabled={busy}
                  onClick={() => setConfirming(true)}
                >
                  {t("BrowserCookieRemove")}
                </button>
              </div>
            </div>
          </li>
        </ul>
      ) : (
        <p className="credential-empty">{t("BrowserCookieNoneSaved")}</p>
      )}

      <div className="provider-detail-helper">
        <strong>{t("BrowserCookiePasteGuideTitle")}</strong>
        <ol>
          <li>{formatLocale(t("BrowserCookiePasteGuideSignIn"), cookieDomain)}</li>
          <li>{t("BrowserCookiePasteGuideDevTools")}</li>
          <li>{formatLocale(t("BrowserCookiePasteGuideCopy"), cookieDomain)}</li>
          <li>{t("BrowserCookiePasteGuideSave")}</li>
        </ol>
        <p>{t("BrowserCookiePasteGuidePrivacy")}</p>
      </div>

      <div className="credential-add-form">
        <SecretField
          label={t("SecretFieldCookieLabel")}
          value={pasteValue}
          onChange={setPasteValue}
          placeholder={cookiePlaceholder(providerId, t)}
          disabled={busy}
          revealLabel={t("SecretFieldReveal")}
          hideLabel={t("SecretFieldHide")}
        />
        <button
          className="credential-btn credential-btn--primary"
          disabled={busy || !pasteValue.trim()}
          onClick={() => void handlePaste()}
        >
          {t("BrowserCookieSave")}
        </button>
      </div>

      <ConfirmDialog
        open={confirming}
        title={t("ConfirmRemoveCookieTitle")}
        body={formatLocale(t("ConfirmRemoveCookieBody"), displayName)}
        confirmLabel={t("ConfirmRemove")}
        cancelLabel={t("ConfirmCancel")}
        busy={busy}
        onCancel={() => setConfirming(false)}
        onConfirm={() => void handleRemove()}
      />
    </section>
  );
}
