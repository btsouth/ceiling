import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getCachedProviders: vi.fn(),
  refreshProviders: vi.fn(),
  refreshProvidersIfStale: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
  listeners: new Map<string, Array<(event: { payload: unknown }) => void>>(),
}));

vi.mock("../lib/tauri", () => tauriMocks);

vi.mock("@tauri-apps/api/event", () => eventMocks);

import { useProviders } from "./useProviders";
import type { ProviderUsageSnapshot } from "../types/bridge";

function provider(id: string, usedPercent = 20): ProviderUsageSnapshot {
  return {
    providerId: id,
    displayName: id,
    primary: {
      usedPercent,
      remainingPercent: 100 - usedPercent,
      windowMinutes: null,
      resetsAt: null,
      resetDescription: null,
      isExhausted: false,
      reservePercent: null,
      reserveDescription: null,
    },
    primaryLabel: "Session",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "CLI",
    updatedAt: new Date().toISOString(),
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
    fetchDurationMs: null,
  };
}

function emitProviderEvent(event: string, payload: unknown) {
  for (const listener of eventMocks.listeners.get(event) ?? []) {
    listener({ payload });
  }
}

describe("useProviders", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventMocks.listeners.clear();
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.refreshProvidersIfStale.mockResolvedValue(undefined);
    eventMocks.listen.mockImplementation(
      (event: string, handler: (event: { payload: unknown }) => void) => {
        const listeners = eventMocks.listeners.get(event) ?? [];
        listeners.push(handler);
        eventMocks.listeners.set(event, listeners);
        return Promise.resolve(() => {});
      },
    );
  });

  it("keeps both accounts of one provider instead of the last one winning", async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useProviders());

    const personal = { ...provider("codex", 12), accountId: "acct-personal" };
    const work = { ...provider("codex", 88), accountId: "acct-work" };

    // Both land inside the same 80ms flush window, which is what happens on a
    // real refresh: the fan-out fetches every account concurrently.
    act(() => {
      emitProviderEvent("provider-updated", personal);
      emitProviderEvent("provider-updated", work);
      vi.advanceTimersByTime(100);
    });

    const ids = result.current.providers.map((p) => p.accountId).sort();
    expect(ids).toEqual(["acct-personal", "acct-work"]);
    vi.useRealTimers();
  });

  it("still replaces a reading of the same account rather than duplicating it", async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useProviders());

    act(() => {
      emitProviderEvent("provider-updated", {
        ...provider("codex", 12),
        accountId: "acct-work",
      });
      vi.advanceTimersByTime(100);
    });
    act(() => {
      emitProviderEvent("provider-updated", {
        ...provider("codex", 44),
        accountId: "acct-work",
      });
      vi.advanceTimersByTime(100);
    });

    expect(result.current.providers).toHaveLength(1);
    expect(result.current.providers[0].primary?.usedPercent).toBe(44);
    vi.useRealTimers();
  });

  it("drops a removed account instead of leaving a ghost card", async () => {
    vi.useFakeTimers();
    const personal = { ...provider("codex", 12), accountId: "acct-personal" };
    const work = { ...provider("codex", 88), accountId: "acct-work" };
    tauriMocks.getCachedProviders.mockResolvedValue([personal, work]);

    const { result } = renderHook(() => useProviders());
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(result.current.providers).toHaveLength(2);

    // Deleting an account evicts its rows in the backend and emits
    // settings-changed, which re-reads the whole cache.
    tauriMocks.getCachedProviders.mockResolvedValue([personal]);
    await act(async () => {
      emitProviderEvent("settings-changed", undefined);
      await vi.runOnlyPendingTimersAsync();
    });

    const ids = result.current.providers.map((p) => p.accountId);
    expect(ids).toEqual(["acct-personal"]);
    vi.useRealTimers();
  });

  it("uses stale-aware refresh on mount", async () => {
    renderHook(() => useProviders());

    await waitFor(() => {
      expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
    });
    expect(tauriMocks.refreshProviders).not.toHaveBeenCalled();
  });

  it("can defer the stale-aware refresh on mount", async () => {
    vi.useFakeTimers();
    try {
      renderHook(() => useProviders({ initialRefreshDelayMs: 250 }));

      expect(tauriMocks.refreshProvidersIfStale).not.toHaveBeenCalled();

      await act(async () => {
        vi.advanceTimersByTime(249);
      });
      expect(tauriMocks.refreshProvidersIfStale).not.toHaveBeenCalled();

      await act(async () => {
        vi.advanceTimersByTime(1);
      });
      expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("can subscribe to cached data and events without refreshing on mount", async () => {
    vi.useFakeTimers();
    try {
      tauriMocks.getCachedProviders.mockResolvedValue([provider("cached", 15)]);

      const { result } = renderHook(() => useProviders({ refreshOnMount: false }));

      await act(async () => {
        await Promise.resolve();
      });
      expect(result.current.providers.map((snapshot) => snapshot.providerId)).toEqual([
        "cached",
      ]);
      expect(tauriMocks.refreshProvidersIfStale).not.toHaveBeenCalled();

      act(() => {
        emitProviderEvent("provider-updated", provider("live", 30));
        vi.advanceTimersByTime(100);
      });

      expect(result.current.providers.map((snapshot) => snapshot.providerId)).toEqual([
        "cached",
        "live",
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("resyncs authoritatively after refresh-complete so pruned ghosts leave the UI", async () => {
    const ambient = { ...provider("codex", 0), accountId: null };
    const registered = {
      ...provider("codex", 0),
      accountId: "11111111-2222-3333-4444-555555555555",
    };
    // Initial paint: both the ambient ghost and the registered seat.
    tauriMocks.getCachedProviders.mockResolvedValueOnce([ambient, registered]);

    const { result } = renderHook(() => useProviders({ refreshOnMount: false }));
    await waitFor(() => expect(result.current.providers).toHaveLength(2));

    // Backend pruned ambient; refresh-complete re-reads the authoritative cache.
    tauriMocks.getCachedProviders.mockResolvedValueOnce([registered]);
    await act(async () => {
      emitProviderEvent("refresh-complete", {
        providerCount: 1,
        errorCount: 0,
      });
    });

    await waitFor(() => {
      expect(result.current.providers).toHaveLength(1);
      expect(result.current.providers[0]?.accountId).toBe(
        "11111111-2222-3333-4444-555555555555",
      );
    });
  });

  it("reloads cached provider presentation when settings change", async () => {
    const visible = provider("codex");
    visible.extraRateWindows = [
      {
        id: "codex-spark",
        title: "Spark",
        window: visible.primary,
      },
    ];
    tauriMocks.getCachedProviders.mockResolvedValueOnce([visible]);

    const { result } = renderHook(() => useProviders({ refreshOnMount: false }));
    await waitFor(() => expect(result.current.providers[0]?.extraRateWindows).toHaveLength(1));

    tauriMocks.getCachedProviders.mockResolvedValueOnce([provider("codex")]);
    act(() => emitProviderEvent("settings-changed", undefined));

    await waitFor(() => expect(result.current.providers[0]?.extraRateWindows).toHaveLength(0));
    expect(tauriMocks.getCachedProviders).toHaveBeenCalledTimes(2);
  });

  it("discards queued pre-change snapshots before reloading settings presentation", async () => {
    vi.useFakeTimers();
    try {
      tauriMocks.getCachedProviders.mockResolvedValueOnce([provider("codex")]);
      const { result } = renderHook(() => useProviders({ refreshOnMount: false }));
      await act(async () => {});

      const visible = provider("codex", 30);
      visible.extraRateWindows = [
        { id: "codex-spark", title: "Spark", window: visible.primary },
      ];
      tauriMocks.getCachedProviders.mockResolvedValueOnce([provider("codex", 40)]);

      await act(async () => {
        emitProviderEvent("provider-updated", visible);
        emitProviderEvent("settings-changed", undefined);
      });
      await act(async () => {
        vi.advanceTimersByTime(80);
      });

      expect(result.current.providers[0]?.primary.usedPercent).toBe(40);
      expect(result.current.providers[0]?.extraRateWindows).toHaveLength(0);

      act(() => emitProviderEvent("provider-updated", provider("codex", 50)));
      act(() => vi.advanceTimersByTime(80));
      expect(result.current.providers[0]?.primary.usedPercent).toBe(50);
    } finally {
      vi.useRealTimers();
    }
  });

  it("applies post-change events after a pending settings cache reload", async () => {
    let resolveReload!: (snapshots: ProviderUsageSnapshot[]) => void;
    tauriMocks.getCachedProviders
      .mockResolvedValueOnce([provider("codex", 20)])
      .mockImplementationOnce(
        () =>
          new Promise<ProviderUsageSnapshot[]>((resolve) => {
            resolveReload = resolve;
          }),
      );
    const { result } = renderHook(() => useProviders({ refreshOnMount: false }));
    await waitFor(() => expect(result.current.providers[0]?.primary.usedPercent).toBe(20));

    act(() => {
      emitProviderEvent("settings-changed", undefined);
      emitProviderEvent("provider-updated", provider("codex", 50));
    });
    await act(async () => {
      resolveReload([provider("codex", 40)]);
    });

    expect(result.current.providers[0]?.primary.usedPercent).toBe(50);
  });

  it("ignores an initial cache response superseded by a settings reload", async () => {
    let resolveInitial!: (snapshots: ProviderUsageSnapshot[]) => void;
    tauriMocks.getCachedProviders
      .mockImplementationOnce(
        () =>
          new Promise<ProviderUsageSnapshot[]>((resolve) => {
            resolveInitial = resolve;
          }),
      )
      .mockResolvedValueOnce([provider("codex", 40)]);
    const { result } = renderHook(() => useProviders({ refreshOnMount: false }));
    await waitFor(() =>
      expect(eventMocks.listeners.get("settings-changed")).toHaveLength(1),
    );

    await act(async () => {
      emitProviderEvent("settings-changed", undefined);
      emitProviderEvent("provider-updated", provider("codex", 50));
    });
    await act(async () => {
      resolveInitial([provider("codex", 20)]);
    });

    expect(result.current.providers[0]?.primary.usedPercent).toBe(50);
  });

  it("does not leave event batching paused when an effect restarts during reload", async () => {
    vi.useFakeTimers();
    try {
      tauriMocks.getCachedProviders
        .mockResolvedValueOnce([provider("codex", 20)])
        .mockImplementationOnce(() => new Promise<ProviderUsageSnapshot[]>(() => {}))
        .mockResolvedValueOnce([provider("codex", 30)]);
      const { result, rerender } = renderHook(
        ({ delay }) =>
          useProviders({
            refreshOnMount: false,
            initialRefreshDelayMs: delay,
          }),
        { initialProps: { delay: 0 } },
      );
      await act(async () => {});

      act(() => emitProviderEvent("settings-changed", undefined));
      rerender({ delay: 1 });
      await act(async () => {});
      act(() => emitProviderEvent("provider-updated", provider("codex", 50)));
      act(() => vi.advanceTimersByTime(80));

      expect(result.current.providers[0]?.primary.usedPercent).toBe(50);
    } finally {
      vi.useRealTimers();
    }
  });

  it("manual refresh uses forced refresh", async () => {
    const { result } = renderHook(() => useProviders());

    await waitFor(() => {
      expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
    });

    act(() => {
      result.current.refresh();
    });

    expect(tauriMocks.refreshProviders).toHaveBeenCalledTimes(1);
  });

  it("re-arms manual refresh once the invoke settles without a refresh-complete", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => {
      expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
    });

    // The backend skipped this cycle (another one owned it), so no
    // refresh-complete reaches the hook; the resolved invoke must unlatch it.
    await act(async () => {
      result.current.refresh();
    });
    await act(async () => {
      result.current.refresh();
    });

    expect(tauriMocks.refreshProviders).toHaveBeenCalledTimes(2);
  });

  it("does not leave an unhandled rejection when the initial cache read fails", async () => {
    tauriMocks.getCachedProviders.mockRejectedValueOnce(new Error("ipc down"));
    const { result } = renderHook(() => useProviders());

    await waitFor(() => expect(result.current.hasLoadedCache).toBe(true));
    expect(result.current.providers).toEqual([]);
  });

  it("reports cached data when cached providers are loaded", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      {
        providerId: "codex",
        displayName: "Codex",
        primary: {
          usedPercent: 25,
          remainingPercent: 75,
          windowMinutes: null,
          resetsAt: null,
          resetDescription: null,
          isExhausted: false,
          reservePercent: null,
          reserveDescription: null,
        },
        primaryLabel: "Session",
        secondary: null,
        secondaryLabel: null,
        modelSpecific: null,
        tertiary: null,
        extraRateWindows: [],
        cost: null,
        planName: null,
        accountEmail: null,
        sourceLabel: "CLI",
        updatedAt: new Date().toISOString(),
        error: null,
        pace: null,
        accountOrganization: null,
        trayStatusLabel: "25%",
        fetchDurationMs: null,
      },
    ]);

    const { result } = renderHook(() => useProviders());

    await waitFor(() => {
      expect(result.current.hasCachedData).toBe(true);
    });
    expect(result.current.hasLoadedCache).toBe(true);
  });

  it("reports cache readiness even when no providers are cached", async () => {
    const { result } = renderHook(() => useProviders());

    expect(result.current.hasLoadedCache).toBe(false);
    await waitFor(() => {
      expect(result.current.hasLoadedCache).toBe(true);
    });
    expect(result.current.hasCachedData).toBe(false);
  });

  it("coalesces streamed provider updates into one state change", async () => {
    vi.useFakeTimers();
    try {
      const { result } = renderHook(() => useProviders());

      act(() => {
        emitProviderEvent("provider-updated", provider("codex", 10));
        emitProviderEvent("provider-updated", provider("claude", 30));
        emitProviderEvent("provider-updated", provider("codex", 45));
      });

      expect(result.current.providers).toHaveLength(0);

      await act(async () => {
        vi.advanceTimersByTime(79);
      });
      expect(result.current.providers).toHaveLength(0);

      await act(async () => {
        vi.advanceTimersByTime(1);
      });

      expect(result.current.providers.map((snapshot) => snapshot.providerId)).toEqual([
        "codex",
        "claude",
      ]);
      expect(result.current.providers[0].primary.usedPercent).toBe(45);
    } finally {
      vi.useRealTimers();
    }
  });

  it("flushes pending provider updates when refresh completes", async () => {
    const { result } = renderHook(() => useProviders());
    await waitFor(() => {
      expect(result.current.hasLoadedCache).toBe(true);
    });

    // Authoritative resync after complete should match the backend cache.
    tauriMocks.getCachedProviders.mockResolvedValue([provider("codex", 10)]);

    vi.useFakeTimers();
    try {
      await act(async () => {
        emitProviderEvent("refresh-started", { providerIds: ["codex"] });
        emitProviderEvent("provider-updated", provider("codex", 10));
        emitProviderEvent("refresh-complete", {
          providerCount: 1,
          errorCount: 0,
        });
      });

      expect(result.current.providers.map((snapshot) => snapshot.providerId)).toEqual([
        "codex",
      ]);
      expect(result.current.isRefreshing).toBe(false);
      expect(result.current.lastRefresh).toEqual({
        providerCount: 1,
        errorCount: 0,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears each provider from refresh state as it completes", async () => {
    const { result } = renderHook(() =>
      useProviders({ refreshOnMount: false }),
    );
    await waitFor(() => expect(result.current.hasLoadedCache).toBe(true));

    act(() =>
      emitProviderEvent("refresh-started", {
        providerIds: ["codex", "claude"],
      }),
    );
    expect([...result.current.refreshingProviderIds]).toEqual(["codex", "claude"]);

    act(() => emitProviderEvent("provider-updated", provider("codex", 10)));
    expect([...result.current.refreshingProviderIds]).toEqual(["claude"]);
    expect(result.current.isRefreshing).toBe(true);

    act(() =>
      emitProviderEvent("refresh-complete", {
        providerCount: 2,
        errorCount: 0,
      }),
    );
    expect(result.current.refreshingProviderIds.size).toBe(0);
    expect(result.current.isRefreshing).toBe(false);
  });
});
