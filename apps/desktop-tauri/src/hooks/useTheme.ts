import { useEffect } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { ThemePreference } from "../types/bridge";

export type ResolvedTheme = "light" | "dark";

const MEDIA_QUERY = "(prefers-color-scheme: light)";

function prefersLight(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) {
    return false;
  }
  return window.matchMedia(MEDIA_QUERY).matches;
}

export function resolveTheme(preference: ThemePreference): ResolvedTheme {
  if (preference === "light") return "light";
  if (preference === "dark") return "dark";
  return prefersLight() ? "light" : "dark";
}

function apply(theme: ResolvedTheme) {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
}

export function useTheme(preference: ThemePreference): void {
  useEffect(() => {
    apply(resolveTheme(preference));

    if (preference !== "auto" || typeof window === "undefined") {
      return;
    }

    let active = true;
    let unlisten: (() => void) | undefined;
    const mq = window.matchMedia?.(MEDIA_QUERY);
    const handle = (event: MediaQueryListEvent) => {
      if (active) apply(event.matches ? "light" : "dark");
    };
    const refresh = () => {
      if (active) apply(resolveTheme("auto"));
    };

    if (typeof mq?.addEventListener === "function") {
      mq.addEventListener("change", handle);
    } else {
      mq?.addListener(handle);
    }
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);

    try {
      void getCurrentWebviewWindow().onThemeChanged(({ payload }) => {
        if (active && (payload === "light" || payload === "dark")) apply(payload);
      }).then((stop) => {
        if (active) unlisten = stop;
        else stop();
      }).catch(() => {});
    } catch {}

    return () => {
      active = false;
      unlisten?.();
      if (typeof mq?.addEventListener === "function") {
        mq.removeEventListener("change", handle);
      } else {
        mq?.removeListener(handle);
      }
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [preference]);
}
