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
  const visibleRows = compact
    ? harnessOverviewRows(rows)
    : rows.filter(({ id }) => id.startsWith("dsh-"));
  return (
    <section className="menu-card__group dsh-usage" aria-label="DeepSeek Harness 用量统计">
      <dl className="dsh-usage__rows">
        {visibleRows.length === 0 && <dt>正在读取用量…</dt>}
        {visibleRows.map((row) => (
          <div className="dsh-usage__row" key={row.id}>
            <dt>{row.title}</dt>
            <dd>{row.window.resetDescription ?? "未报告"}</dd>
          </div>
        ))}
      </dl>
      {!compact && <p className="dsh-usage__note">Token 来自本机 DSH 记录，余额来自官方账号。套餐上限和重置时间未提供。</p>}
    </section>
  );
}
