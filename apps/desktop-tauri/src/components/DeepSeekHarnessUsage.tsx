import { useLocale } from "../hooks/useLocale";

type StatisticRow = {
  id: string;
  title: string;
  window: { resetDescription?: string | null };
};

export function harnessOverviewRows(rows: readonly StatisticRow[]) {
  return rows.filter(({ id }) =>
    id.startsWith("dsh-balance-") ||
    id === "dsh-today" ||
    id === "dsh-month" ||
    id === "dsh-log-status"
  );
}

/** Recorded counts and wallet balances have no meaningful quota percentage. */
export default function DeepSeekHarnessUsage({ rows, compact = false }: {
  rows: readonly StatisticRow[];
  compact?: boolean;
}) {
  const { t } = useLocale();
  const visibleRows = compact
    ? harnessOverviewRows(rows)
    : rows.filter(({ id }) => id.startsWith("dsh-"));
  return (
    <section className="menu-card__group dsh-usage" aria-label={t("DshUsageLabel")}>
      <dl className="dsh-usage__rows">
        {visibleRows.length === 0 && <dt>{t("DshUsageLoading")}</dt>}
        {visibleRows.map((row) => (
          <div className="dsh-usage__row" key={row.id}>
            <dt>{row.title}</dt>
            <dd>{row.window.resetDescription ?? t("DshNotReported")}</dd>
          </div>
        ))}
      </dl>
      {!compact && <p className="dsh-usage__note">{t("DshUsageNote")}</p>}
    </section>
  );
}
