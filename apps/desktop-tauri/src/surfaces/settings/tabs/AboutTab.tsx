import { useEffect, useState } from "react";
import { useLocale } from "../../../hooks/useLocale";
import { formatLocale } from "../../../lib/formatLocale";
import { useUpdateState } from "../../../hooks/useUpdateState";
import { getAppInfo, openExternalUrl } from "../../../lib/tauri";
import { Field, Toggle } from "../../../components/FormControls";
import type { AppInfoBridge } from "../../../types/bridge";
import type { TabProps } from "../../Settings";
import ceilingIcon from "../../../assets/ceiling-icon.png";

const ABOUT_LINKS = [
  {
    label: "GitHub",
    url: "https://github.com/btsouth/ceiling",
  },
  {
    label: "Website",
    url: "https://ceiling.win",
  },
] as const;

export default function AboutTab({ settings, set, saving }: TabProps) {
  const { t } = useLocale();
  const [appInfo, setAppInfo] = useState<AppInfoBridge | null>(null);
  const { updateState, checkNow, download, apply, openRelease } =
    useUpdateState();
  const [hasChecked, setHasChecked] = useState(false);
  const [linkError, setLinkError] = useState<string | null>(null);

  useEffect(() => {
    void getAppInfo().then(setAppInfo);
  }, []);

  const handleCheck = () => {
    setHasChecked(true);
    checkNow();
  };

  const openAboutLink = (url: string) => {
    setLinkError(null);
    openExternalUrl(url).catch((error) => {
      setLinkError(String(error));
    });
  };

  if (!appInfo) {
    return (
      <section className="settings-section">
        <p className="settings-section__hint">{t("AboutLoading")}</p>
      </section>
    );
  }

  const isBusy =
    updateState.status === "checking" ||
    updateState.status === "downloading";

  return (
    <section className="settings-section about-section">
      <div className="about-header">
        <img className="about-icon" src={ceilingIcon} alt="Ceiling" />
        <div className="about-title-block">
          <h2 className="about-title">{appInfo.name}</h2>
          <p className="about-version">
            {formatLocale(t("AboutVersion"), appInfo.version)}
            {appInfo.buildNumber !== "dev" && ` (${appInfo.buildNumber})`}
          </p>
          <p className="about-tagline">{appInfo.tagline}</p>
        </div>
      </div>

      <div className="about-links">
        {ABOUT_LINKS.map((link) => (
          <button
            key={link.url}
            type="button"
            className="about-link"
            onClick={() => openAboutLink(link.url)}
          >
            {link.label}
          </button>
        ))}
      </div>
      {linkError && (
        <p className="about-update-msg">
          {t("AboutLinkErrorPrefix")} {linkError}
        </p>
      )}

      <div className="about-divider" />

      <div className="about-update-controls">
        <Field
          label={t("AutoDownloadUpdates")}
          description={t("AutoDownloadUpdatesHelper")}
          leading
        >
          <Toggle
            checked={settings.autoDownloadUpdates}
            disabled={saving}
            onChange={(v) => set({ autoDownloadUpdates: v })}
          />
        </Field>

      </div>

      <div className="about-actions">
        <button
          className="credential-btn credential-btn--primary"
          disabled={isBusy}
          onClick={handleCheck}
        >
          {updateState.status === "checking"
            ? t("AboutChecking")
            : t("AboutCheckForUpdates")}
        </button>

        {updateState.status === "available" && (
          <div className="about-update-row">
            <span className="about-update-msg">
              {formatLocale(t("AboutUpdateAvailable"), updateState.version ?? "")}
            </span>
            {updateState.canDownload ? (
              <button
                className="credential-btn credential-btn--primary"
                onClick={download}
              >
                {t("BannerDownloadButton")}
              </button>
            ) : (
              <button className="credential-btn" onClick={openRelease}>
                {t("BannerViewRelease")}
              </button>
            )}
          </div>
        )}

        {updateState.status === "downloading" && (
          <span className="about-update-msg">
            {t("AboutDownloading")}
            {updateState.progress != null &&
              ` ${Math.round(updateState.progress * 100)}%`}
          </span>
        )}

        {updateState.status === "ready" && (
          <div className="about-update-row">
            <span className="about-update-msg">{t("AboutUpdateReady")}</span>
            {updateState.canApply ? (
              <button
                className="credential-btn credential-btn--primary"
                onClick={apply}
              >
                {t("BannerInstallRestart")}
              </button>
            ) : (
              <button className="credential-btn" onClick={openRelease}>
                {t("BannerViewRelease")}
              </button>
            )}
          </div>
        )}

        {updateState.status === "error" && (
          <span className="about-update-msg">
            {/*
              The backend sentence stands on its own, and it is not always a
              check failure: a hash mismatch or a rejected signature from
              Download or Install lands here too. Prefixing every one of them
              with "Could not check for updates." mislabelled those and read as
              a duplicate for the check failures that already say it.
            */}
            {updateState.error || t("AboutUpdateCheckFailed")}
          </span>
        )}

        {updateState.status === "idle" && hasChecked && (
          <span className="about-update-msg">{t("AboutUpToDate")}</span>
        )}
      </div>

      <p className="about-copyright">
        {t("AboutCopyrightPrefix")}{" "}
        <button
          type="button"
          className="about-link about-link--inline"
          onClick={() =>
            openAboutLink("https://github.com/Finesssee/Win-CodexBar")
          }
        >
          Win-CodexBar
        </button>
        {t("AboutCopyrightMid")}{" "}
        <button
          type="button"
          className="about-link about-link--inline"
          onClick={() => openAboutLink("https://github.com/steipete/CodexBar")}
        >
          CodexBar
        </button>
        {t("AboutCopyrightSuffix")}
      </p>
    </section>
  );
}
