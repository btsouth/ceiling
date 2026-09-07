import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getAppInfo: vi.fn(),
  openExternalUrl: vi.fn(),
}));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  ...tauriMocks,
}));

import { SETTLE_MS, readStarPromptState } from "../lib/starPrompt";
import { useStarPrompt } from "./useStarPrompt";
import type { ProviderUsageSnapshot } from "../types/bridge";

const VERSION = "1.5.34";

function reading(): ProviderUsageSnapshot {
  return {
    providerId: "claude",
    displayName: "Claude",
    primary: {
      usedPercent: 20,
      remainingPercent: 80,
      windowMinutes: null,
      resetsAt: null,
      resetDescription: null,
      isExhausted: false,
      reservePercent: null,
      reserveDescription: null,
    },
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "test",
    updatedAt: new Date().toISOString(),
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;
}

function Harness({ providers }: { providers: ProviderUsageSnapshot[] }) {
  const { reason, onStar, onDismiss } = useStarPrompt(providers);
  return (
    <div>
      <span data-testid="reason">{reason ?? "none"}</span>
      <button type="button" onClick={onStar}>
        star
      </button>
      <button type="button" onClick={onDismiss}>
        dismiss
      </button>
    </div>
  );
}

function currentReason(): string {
  return screen.getByTestId("reason").textContent ?? "";
}

/** Let the version promise resolve, then run past the settle delay. */
async function settle() {
  await act(async () => {
    await Promise.resolve();
  });
  await act(async () => {
    vi.advanceTimersByTime(SETTLE_MS + 5_000);
  });
}

describe("useStarPrompt", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    localStorage.clear();
    tauriMocks.getAppInfo.mockResolvedValue({
      name: "Ceiling",
      version: VERSION,
      buildNumber: "1",
      updateChannel: "stable",
      tagline: "",
    });
    tauriMocks.openExternalUrl.mockResolvedValue(undefined);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("stays quiet until the reading has been on screen a while", async () => {
    render(<Harness providers={[reading()]} />);
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      vi.advanceTimersByTime(SETTLE_MS - 1_000);
    });
    expect(currentReason()).toBe("none");
    expect(readStarPromptState().asked).toBe(0);
  });

  it("asks once the reading has settled", async () => {
    render(<Harness providers={[reading()]} />);
    await settle();
    expect(currentReason()).toBe("firstValue");
  });

  it("counts the ask when it appears, not when it is answered", async () => {
    // Otherwise closing the window without touching the card would leave the
    // count at zero and the same prompt would return on the next launch.
    const view = render(<Harness providers={[reading()]} />);
    await settle();
    expect(readStarPromptState().asked).toBe(1);
    expect(readStarPromptState().lastAskedVersion).toBe(VERSION);
    view.unmount();
    expect(readStarPromptState().asked).toBe(1);
  });

  it("does not come back on the next launch of the same version", async () => {
    const first = render(<Harness providers={[reading()]} />);
    await settle();
    expect(currentReason()).toBe("firstValue");
    first.unmount();

    render(<Harness providers={[reading()]} />);
    await settle();
    expect(currentReason()).toBe("none");
    expect(readStarPromptState().asked).toBe(1);
  });

  it("opens the repo and stops asking for good when starred", async () => {
    render(<Harness providers={[reading()]} />);
    await settle();
    await act(async () => {
      screen.getByText("star").click();
    });
    expect(currentReason()).toBe("none");
    expect(tauriMocks.openExternalUrl).toHaveBeenCalledWith(
      "https://github.com/btsouth/ceiling",
    );
    expect(readStarPromptState().starred).toBe(true);
  });

  it("does not treat Later as interest", async () => {
    render(<Harness providers={[reading()]} />);
    await settle();
    await act(async () => {
      screen.getByText("dismiss").click();
    });
    expect(currentReason()).toBe("none");
    expect(tauriMocks.openExternalUrl).not.toHaveBeenCalled();
    expect(readStarPromptState().starred).toBe(false);
  });

  it("stays put when the reading goes away underneath it", async () => {
    // A refresh failing mid-click must not yank the card out from under the
    // pointer; the ask is already counted, so hiding it would waste it too.
    const view = render(<Harness providers={[reading()]} />);
    await settle();
    expect(currentReason()).toBe("firstValue");

    view.rerender(<Harness providers={[]} />);
    await act(async () => {
      vi.advanceTimersByTime(10_000);
    });
    expect(currentReason()).toBe("firstValue");
  });

  it("never asks with nothing on screen to have earned it", async () => {
    render(<Harness providers={[]} />);
    await settle();
    expect(currentReason()).toBe("none");
    expect(readStarPromptState().asked).toBe(0);
  });

  it("restarts the wait when the reading disappears part-way through it", async () => {
    // A failed refresh must not bank credit toward the settle period: the next
    // reading starts its twenty seconds over.
    const view = render(<Harness providers={[reading()]} />);
    await act(async () => {
      await Promise.resolve();
    });
    await act(async () => {
      vi.advanceTimersByTime(SETTLE_MS - 2_000);
    });

    view.rerender(<Harness providers={[]} />);
    await act(async () => {
      vi.advanceTimersByTime(5_000);
    });
    view.rerender(<Harness providers={[reading()]} />);

    // The old partial wait plus this one would already be past SETTLE_MS.
    await act(async () => {
      vi.advanceTimersByTime(SETTLE_MS - 5_000);
    });
    expect(currentReason()).toBe("none");

    await act(async () => {
      vi.advanceTimersByTime(10_000);
    });
    expect(currentReason()).toBe("firstValue");
  });

  it("does not show an ask it cannot record", async () => {
    // Storage that will not hold the record means no cap, so no prompt at all
    // rather than one on every launch.
    const setItem = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new Error("storage disabled");
      });
    try {
      render(<Harness providers={[reading()]} />);
      await settle();
      expect(currentReason()).toBe("none");
    } finally {
      setItem.mockRestore();
    }
  });
});
