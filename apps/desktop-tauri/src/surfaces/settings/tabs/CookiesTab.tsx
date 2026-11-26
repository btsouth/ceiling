import { useCallback, useEffect, useState } from "react";
import {
  getManualCookies,
  removeManualCookie,
  setManualCookie,
} from "../../../lib/tauri";
import { Select } from "../../../components/FormControls";
import { ConfirmDialog } from "../../../components/ConfirmDialog";
import { SecretField } from "../../../components/SecretField";
import { formatLocale } from "../../../lib/formatLocale";
import { useLocale } from "../../../hooks/useLocale";
import { cookiePlaceholder } from "../providers/CookieSection";
import type {
  CookieInfoBridge,
  ProviderCatalogEntry,
} from "../../../types/bridge";

export default function CookiesTab({ providers }: { providers: ProviderCatalogEntry[] }) {
  const { t } = useLocale();
  const [cookies, setCookies] = useState<CookieInfoBridge[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Add-cookie form state
  const [addProviderId, setAddProviderId] = useState("");
  const [addCookieValue, setAddCookieValue] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [pendingRemove, setPendingRemove] = useState<{
    providerId: string;
    provider: string;
  } | null>(null);

  const reload = useCallback(async () => {
    try {
      setCookies(await getManualCookies());
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Only show providers with a cookie domain
  const cookieProviders = providers.filter((p) => p.cookieDomain !== null);
  const guideSite =
    cookieProviders.find((p) => p.id === addProviderId)?.cookieDomain ??
    t("BrowserCookiePasteGuideSiteFallback");

  const handleAdd = async () => {
    if (!addProviderId || !addCookieValue.trim()) return;
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const next = await setManualCookie(addProviderId, addCookieValue.trim());
      setCookies(next);
      setAddProviderId("");
      setAddCookieValue("");
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRemove = async () => {
    if (!pendingRemove) return;
    setBusy(true);
    setError(null);
    setStatus(null);
    try {
      const next = await removeManualCookie(pendingRemove.providerId);
      setCookies(next);
      setPendingRemove(null);
      setStatus(t("CredentialRemoved"));
    } catch (err: unknown) {
      setPendingRemove(null);
      setError(err instanceof Error ? err.message : t("CredentialRemoveFailed"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">Saved Cookies</h3>
      <p className="settings-section__hint">
        Saved cookie headers for browser-authenticated providers. Ceiling does
        not scan browser databases; you stay in control of exactly what is saved.
      </p>
      <div className="settings-status">
        {t("BrowserCookieMigrationNotice")}
      </div>

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

      {cookies.length > 0 ? (
        <ul className="credential-list">
          {cookies.map((c) => (
            <li key={c.providerId} className="credential-card">
              <div className="credential-card__header">
                <div className="credential-card__info">
                  <strong>{c.provider}</strong>
                  <span className="credential-card__meta">
                    <span className="credential-card__badge credential-card__badge--set">
                      Saved
                    </span>
                    <span className="credential-card__date">
                      {c.savedAt}
                    </span>
                  </span>
                </div>
                <div className="credential-card__actions">
                  <button
                    className="credential-btn credential-btn--danger"
                    disabled={busy}
                    onClick={() =>
                      setPendingRemove({
                        providerId: c.providerId,
                        provider: c.provider,
                      })
                    }
                  >
                    Remove
                  </button>
                </div>
              </div>
            </li>
          ))}
        </ul>
      ) : (
        <p className="credential-empty">No manual cookies saved.</p>
      )}

      <h3 className="settings-section__title">{t("BrowserCookiePasteGuideTitle")}</h3>
      <ol className="settings-section__hint">
        <li>{formatLocale(t("BrowserCookiePasteGuideSignIn"), guideSite)}</li>
        <li>{t("BrowserCookiePasteGuideDevTools")}</li>
        <li>{formatLocale(t("BrowserCookiePasteGuideCopy"), guideSite)}</li>
        <li>{t("BrowserCookiePasteGuideSave")}</li>
      </ol>
      <p className="settings-section__hint">{t("BrowserCookiePasteGuidePrivacy")}</p>
      <div className="credential-add-form">
        <Select
          value={addProviderId}
          options={[
            { value: "", label: "Select provider…" },
            ...cookieProviders.map((p) => ({
              value: p.id,
              label: p.displayName,
            })),
          ]}
          onChange={setAddProviderId}
          disabled={busy}
        />
        <SecretField
          label={t("SecretFieldCookieLabel")}
          value={addCookieValue}
          onChange={setAddCookieValue}
          placeholder={cookiePlaceholder(addProviderId, t)}
          disabled={busy}
          revealLabel={t("SecretFieldReveal")}
          hideLabel={t("SecretFieldHide")}
        />
        <button
          className="credential-btn credential-btn--primary"
          disabled={busy || !addProviderId || !addCookieValue.trim()}
          onClick={() => void handleAdd()}
        >
          {t("BrowserCookieSave")}
        </button>
      </div>
      <ConfirmDialog
        open={pendingRemove !== null}
        title={t("ConfirmRemoveCookieTitle")}
        body={formatLocale(
          t("ConfirmRemoveCookieBody"),
          pendingRemove?.provider ?? "",
        )}
        confirmLabel={t("ConfirmRemove")}
        cancelLabel={t("ConfirmCancel")}
        busy={busy}
        onCancel={() => setPendingRemove(null)}
        onConfirm={() => void handleRemove()}
      />
    </section>
  );
}
