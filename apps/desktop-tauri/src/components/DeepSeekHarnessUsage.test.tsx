import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import DeepSeekHarnessUsage from "./DeepSeekHarnessUsage";

describe("DSH recorded statistics", () => {
  it("shows exact labels and values without inventing quota percentages", () => {
    const { container } = render(<DeepSeekHarnessUsage rows={[
      { id: "dsh-balance-CNY", title: "账户余额", window: { resetDescription: "¥42.12" } },
      { id: "dsh-today", title: "今日 Token", window: { resetDescription: "0" } },
      { id: "dsh-month", title: "本月 Token", window: { resetDescription: "1,234,567" } },
    ]} />);
    expect(screen.getByText("¥42.12")).toBeTruthy();
    expect(screen.getByText("1,234,567")).toBeTruthy();
    expect(screen.getByText("0")).toBeTruthy();
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
    expect(container.textContent).not.toMatch(/100%|0%/);
  });
  it("keeps unavailable balances visibly separate from a successful zero", () => {
    render(<DeepSeekHarnessUsage rows={[
      { id: "dsh-balance-status", title: "账户余额", window: { resetDescription: "暂时无法读取，请在 DSH 中确认登录" } },
      { id: "dsh-today", title: "今日 Token", window: { resetDescription: "0" } },
    ]} />);
    expect(screen.getByText(/暂时无法读取/)).toBeTruthy();
    expect(screen.getByText("0")).toBeTruthy();
  });
});
