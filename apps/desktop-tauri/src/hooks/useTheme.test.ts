import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { resolveTheme, useTheme } from "./useTheme";

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: vi.fn(),
}));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

describe("useTheme", () => {
  it.each(["throw", "reject"])("handles Tauri registration failure: %s", async (failure) => {
    vi.mocked(getCurrentWebviewWindow).mockImplementation(() => {
      if (failure === "throw") throw new Error("No Tauri internals");
      return {
        onThemeChanged: () => Promise.reject(new Error("No Tauri internals")),
      } as unknown as ReturnType<typeof getCurrentWebviewWindow>;
    });
    let matches = false;
    vi.stubGlobal("matchMedia", () => ({
      matches,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));
    const { unmount } = renderHook(() => useTheme("auto"));
    await act(async () => {});

    matches = true;
    act(() => window.dispatchEvent(new Event("focus")));
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    matches = false;
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    unmount();
  });

  it("unlistens when registration completes after cleanup", async () => {
    const unlisten = vi.fn();
    let finish!: (stop: () => void) => void;
    vi.mocked(getCurrentWebviewWindow).mockReturnValue({
      onThemeChanged: () => new Promise<() => void>((resolve) => { finish = resolve; }),
    } as unknown as ReturnType<typeof getCurrentWebviewWindow>);
    const { unmount } = renderHook(() => useTheme("auto"));
    unmount();
    await act(async () => finish(unlisten));
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("applies native theme events and ignores them after cleanup", async () => {
    const unlisten = vi.fn();
    let onTheme: (event: { payload: "light" | "dark" }) => void = () => {};
    const onThemeChanged = vi.fn((handler: typeof onTheme) => {
      onTheme = handler;
      return Promise.resolve(unlisten);
    });
    vi.mocked(getCurrentWebviewWindow).mockReturnValue({
      onThemeChanged,
    } as unknown as ReturnType<typeof getCurrentWebviewWindow>);
    vi.stubGlobal("matchMedia", () => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));

    const { unmount } = renderHook(() => useTheme("auto"));
    await act(async () => {});
    expect(onThemeChanged).toHaveBeenCalledOnce();
    expect(document.documentElement.dataset.theme).toBe("dark");

    act(() => onTheme({ payload: "light" }));
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");

    act(() => onTheme({ payload: "dark" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");

    unmount();
    expect(unlisten).toHaveBeenCalledOnce();
    act(() => onTheme({ payload: "light" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
  });
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
