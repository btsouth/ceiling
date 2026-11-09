import type { ProviderDetail } from "../../../../types/bridge";
import type { LocaleKey } from "../../../../i18n/keys";
import { openExternalUrl } from "../../../../lib/tauri";
import { providerProvenanceKey } from "../../../../lib/providerProvenance";

const DATA_SOURCES_DOC_URL =
  "https://github.com/btsouth/ceiling/blob/main/docs/DATA_SOURCES.md";

interface Props {
  provider: ProviderDetail;
  t: (key: LocaleKey) => string;
}

/**
 * "How Ceiling gets this data" — a short, provider-specific provenance line
 * plus a precise privacy note (SOU-179). The copy is verified against the real
 * fetch paths documented in `docs/DATA_SOURCES.md`; providers without dedicated
 * copy fall back to a precise generic statement. Storage/DPAPI detail lives in
 * the separate CredentialStorageSection, so this block stays about *where the
 * data comes from* and *what is (and isn't) sent over the network*.
 */
export function DataSourceSection({ provider, t }: Props) {
  return (
    <section className="provider-detail-section provider-detail-datasource">
      <h4>{t("DataSourceSectionTitle")}</h4>
      <p className="provider-detail-datasource__source">
        {t(providerProvenanceKey(provider.id))}
      </p>
      <p className="provider-detail-datasource__privacy">
        {t("DataSourcePrivacyNote")}
      </p>
      <button
        type="button"
        className="provider-detail-datasource__link"
        onClick={() => void openExternalUrl(DATA_SOURCES_DOC_URL)}
      >
        {t("DataSourceLearnMore")}
      </button>
    </section>
  );
}
