import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

const DeepSeekHarnessUsage = (await import("./DeepSeekHarnessUsage")).default;

describe("DSH recorded statistics", () => {
  it("shows exact labels and values without inventing quota percentages", () => {
    const { container } = render(<DeepSeekHarnessUsage rows={[
      { id: "dsh-balance-CNY", title: "Account balance", window: { resetDescription: "¥42.12" } },
      { id: "dsh-today", title: "Tokens today", window: { resetDescription: "0" } },
      { id: "dsh-month", title: "Tokens this month", window: { resetDescription: "1,234,567" } },
    ]} />);
    expect(screen.getByText("¥42.12")).toBeTruthy();
    expect(screen.getByText("1,234,567")).toBeTruthy();
    expect(screen.getByText("0")).toBeTruthy();
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
    expect(container.textContent).not.toMatch(/100%|0%/);
  });
  it("keeps unavailable balances visibly separate from a successful zero", () => {
    render(<DeepSeekHarnessUsage rows={[
      { id: "dsh-balance-status", title: "Account balance", window: { resetDescription: "Balance unavailable (HTTP 401). Check that you are signed in to DeepSeek Harness" } },
      { id: "dsh-today", title: "Tokens today", window: { resetDescription: "0" } },
    ]} />);
    expect(screen.getByText(/Balance unavailable/)).toBeTruthy();
    expect(screen.getByText("0")).toBeTruthy();
  });
});
