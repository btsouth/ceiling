import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ThemePreference } from "../types/bridge";

const tauriMocks = vi.hoisted(() => ({
  isTauri: vi.fn(() => false),
  setTheme: vi.fn((_theme: "light" | "dark" | null) => Promise.resolve()),
}));

vi.mock("@tauri-apps/api/core", () => ({ isTauri: tauriMocks.isTauri }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTheme: tauriMocks.setTheme }),
}));

import { resolveTheme, useTheme } from "./useTheme";

const LIGHT_MEDIA_QUERY = "(prefers-color-scheme: light)";

function stubMatchMedia(initialMatches: boolean) {
  let matches = initialMatches;
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  const addEventListener = vi.fn(
    (_type: string, listener: (event: MediaQueryListEvent) => void) => listeners.add(listener),
  );
  const removeEventListener = vi.fn(
    (_type: string, listener: (event: MediaQueryListEvent) => void) => listeners.delete(listener),
  );
  const mediaQueryList = {
    get matches() {
      return matches;
    },
    media: LIGHT_MEDIA_QUERY,
    onchange: null,
    addEventListener,
    removeEventListener,
    addListener: vi.fn(),
    removeListener: vi.fn(),
  } as unknown as MediaQueryList;

  vi.stubGlobal("matchMedia", vi.fn(() => mediaQueryList));

  return {
    addEventListener,
    removeEventListener,
    change(nextMatches: boolean) {
      matches = nextMatches;
      const event = Object.assign(new Event("change"), {
        matches: nextMatches,
        media: LIGHT_MEDIA_QUERY,
      }) as MediaQueryListEvent;
      for (const listener of [...listeners]) listener(event);
    },
  };
}

beforeEach(() => {
  tauriMocks.isTauri.mockReset();
  tauriMocks.isTauri.mockReturnValue(false);
  tauriMocks.setTheme.mockClear();
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.style.removeProperty("color-scheme");
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("resolveTheme", () => {
  it("uses the OS light preference in auto mode", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query === "(prefers-color-scheme: light)",
    }));

    expect(resolveTheme("auto")).toBe("light");

    vi.unstubAllGlobals();
  });

  it("falls back to dark in auto mode when light is not preferred", () => {
    vi.stubGlobal("matchMedia", () => ({ matches: false }));

    expect(resolveTheme("auto")).toBe("dark");

    vi.unstubAllGlobals();
  });
});

describe("useTheme", () => {
  it("lets the native window follow the OS in auto mode and updates the page", () => {
    tauriMocks.isTauri.mockReturnValue(true);
    const media = stubMatchMedia(true);

    const { unmount } = renderHook(() => useTheme("auto"));

    expect(tauriMocks.setTheme).toHaveBeenCalledExactlyOnceWith(null);
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");

    act(() => media.change(false));

    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    expect(tauriMocks.setTheme).toHaveBeenCalledExactlyOnceWith(null);

    unmount();
    expect(media.removeEventListener).toHaveBeenCalledWith("change", expect.any(Function));

    act(() => media.change(true));
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("applies explicit light and dark overrides without subscribing to OS changes", () => {
    tauriMocks.isTauri.mockReturnValue(true);
    const media = stubMatchMedia(true);

    const { rerender } = renderHook(({ preference }) => useTheme(preference), {
      initialProps: { preference: "light" as ThemePreference },
    });

    expect(tauriMocks.setTheme).toHaveBeenLastCalledWith("light");
    expect(document.documentElement.dataset.theme).toBe("light");

    act(() => media.change(false));
    expect(document.documentElement.dataset.theme).toBe("light");

    rerender({ preference: "dark" });

    expect(tauriMocks.setTheme).toHaveBeenLastCalledWith("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(media.addEventListener).not.toHaveBeenCalled();
    expect(media.removeEventListener).not.toHaveBeenCalled();

    act(() => media.change(true));
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("removes the OS listener when auto changes to an explicit preference", () => {
    tauriMocks.isTauri.mockReturnValue(true);
    const media = stubMatchMedia(false);

    const { rerender } = renderHook(({ preference }) => useTheme(preference), {
      initialProps: { preference: "auto" as ThemePreference },
    });
    const listener = media.addEventListener.mock.calls[0]?.[1];

    rerender({ preference: "light" });

    expect(media.removeEventListener).toHaveBeenCalledWith("change", listener);
    expect(tauriMocks.setTheme).toHaveBeenLastCalledWith("light");
    act(() => media.change(true));
    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("does not call native window APIs when running in a browser", () => {
    const media = stubMatchMedia(false);

    renderHook(() => useTheme("auto"));

    expect(tauriMocks.setTheme).not.toHaveBeenCalled();
    act(() => media.change(true));
    expect(document.documentElement.dataset.theme).toBe("light");
  });
});
