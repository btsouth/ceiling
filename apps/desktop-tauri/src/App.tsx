import { Suspense, lazy, useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  checkForUpdates,
  downloadUpdate,
  getBootstrapState,
  getSettingsSnapshot,
  setSurfaceMode,
} from "./lib/tauri";
import { useSurfaceSnapshot } from "./hooks/useSurfaceSnapshot";
import { useTheme } from "./hooks/useTheme";
import TrayPanel from "./surfaces/TrayPanel";
import TaskbarFlyout from "./surfaces/TaskbarFlyout";
import RevealAfterPaint from "./components/RevealAfterPaint";
import { FLOATBAR_WINDOW_LABEL } from "./floatbar/api";
import { LocaleProvider } from "./i18n/LocaleProvider";
import type { BootstrapState, ThemePreference } from "./types/bridge";
import type { SurfaceSnapshot } from "./hooks/useSurfaceSnapshot";

const Settings = lazy(() => import("./surfaces/Settings"));
const PopOutPanel = lazy(() => import("./surfaces/PopOutPanel"));
const FloatBar = lazy(() => import("./floatbar/FloatBar"));

function SurfaceFallback() {
  return null;
}

/** True when running inside the detached Settings window. */
function isSettingsWindow(): boolean {
  return getCurrentWebviewWindow().label === "settings";
}

/** True when running inside the detached FloatBar window. */
function isFloatBarWindow(): boolean {
  return getCurrentWebviewWindow().label === FLOATBAR_WINDOW_LABEL;
}

/** True when running inside the detached flyout ("Pop Out Dashboard") window. */
function isFlyoutWindow(): boolean {
  return getCurrentWebviewWindow().label === "flyout";
}

/** Parse the initial Settings tab from the URL query string. */
function initialSettingsTab(): string {
  const params = new URLSearchParams(window.location.search);
  return params.get("tab") || "general";
}

export default function App() {
  return (
    <LocaleProvider>
      <AppInner />
    </LocaleProvider>
  );
}

function AppInner() {
  const auxiliaryWindow = isFloatBarWindow() || isFlyoutWindow();
  const surface = useSurfaceSnapshot();
  const [state, setState] = useState<BootstrapState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [themePreference, setThemePreference] = useState<ThemePreference>("dark");

  useTheme(themePreference);

  const reloadBootstrapState = useCallback(
    () => getBootstrapState(),
    [],
  );

  useEffect(() => {
    let cancelled = false;

    reloadBootstrapState()
      .then((bootstrap) => {
        if (cancelled) {
          return;
        }
        setState(bootstrap);
        setThemePreference(bootstrap.settings.theme);
        setError(null);
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setError(cause instanceof Error ? cause.message : String(cause));
        }
      });

    // Fire-and-forget update checks after the first paint so startup/tray open
    // is not competing with network work.
    const updateTimer = auxiliaryWindow
      ? null
      : window.setTimeout(() => {
          Promise.all([checkForUpdates(), getSettingsSnapshot()])
            .then(([update, settings]) => {
              if (settings.autoDownloadUpdates && update.canDownload) {
                void downloadUpdate().catch(() => {});
              }
            })
            .catch(() => {});
        }, 2_000);

    const unlistenSettingsChangePromise = isSettingsWindow()
      ? listen<string>("settings-change-tab", () => {
          void reloadBootstrapState()
            .then((bootstrap) => {
              setState(bootstrap);
              setThemePreference(bootstrap.settings.theme);
              setError(null);
            })
            .catch(() => {});
        })
      : Promise.resolve(null);

    // Keep the theme in sync when mutations happen inside other surfaces
    // (e.g., Settings → Appearance). `useSettings` dispatches this event
    // after every successful `updateSettings` call.
    const onSettingsUpdated = (evt: Event) => {
      const detail = (evt as CustomEvent<BootstrapState["settings"]>).detail;
      if (detail) {
        setThemePreference(detail.theme);
      } else {
        getSettingsSnapshot()
          .then((fresh) => setThemePreference(fresh.theme))
          .catch(() => {});
      }
    };
    window.addEventListener("codexbar:settings-updated", onSettingsUpdated);

    return () => {
      cancelled = true;
      void unlistenSettingsChangePromise
        .then((unlisten) => unlisten?.())
        .catch(() => {});
      if (updateTimer !== null) window.clearTimeout(updateTimer);
      window.removeEventListener("codexbar:settings-updated", onSettingsUpdated);
    };
  }, [auxiliaryWindow, reloadBootstrapState]);

  if (error) {
    const content = (
      <main className="shell">
        <section className="panel error">
          <h2>Bootstrap failed</h2>
          <p>{error}</p>
        </section>
      </main>
    );
    return isSettingsWindow() || surface.mode !== "hidden"
      ? <RevealAfterPaint>{content}</RevealAfterPaint>
      : content;
  }

  if (!state) {
    return (
      <main className="shell">
        <section className="panel">
          <h2>Loading shell contract…</h2>
          <p>Waiting for the Rust bridge to describe providers, surfaces, and settings.</p>
        </section>
      </main>
    );
  }

  // Detached settings window — render Settings directly, skip SurfaceRouter.
  if (isSettingsWindow()) {
    return <DetachedSettingsApp state={state} />;
  }

  // Detached floating-bar window — render the FloatBar surface directly.
  if (isFloatBarWindow()) {
    return (
      <Suspense fallback={<SurfaceFallback />}>
        <FloatBar state={state} />
      </Suspense>
    );
  }

  // The native taskbar widget gets a purpose-built glance flyout. Keeping it
  // separate from TrayPanel prevents dashboard controls and remembered sizing
  // from making this quick interaction feel like a second app window.
  if (isFlyoutWindow()) {
    return <TaskbarFlyout state={state} />;
  }

  return <SurfaceRouter surface={surface} state={state} />;
}

function SurfaceRouter({
  surface,
  state,
}: {
  surface: SurfaceSnapshot;
  state: BootstrapState;
}) {
  switch (surface.mode) {
    case "hidden":
      return null;
    case "trayPanel":
      return <RevealAfterPaint><TrayPanel state={state} /></RevealAfterPaint>;
    case "popOut": {
      const providerId =
        surface.target.kind === "provider"
          ? surface.target.providerId
          : undefined;
      return (
        <Suspense fallback={<SurfaceFallback />}>
          <RevealAfterPaint><PopOutPanel state={state} providerId={providerId} /></RevealAfterPaint>
        </Suspense>
      );
    }
    case "settings":
      return (
        <Suspense fallback={<SurfaceFallback />}>
          <RevealAfterPaint><SettingsLayout state={state} /></RevealAfterPaint>
        </Suspense>
      );
    default:
      return <RevealAfterPaint><TrayPanel state={state} /></RevealAfterPaint>;
  }
}

function SettingsLayout({ state }: { state: BootstrapState }) {
  return (
    <main className="settings-surface settings-surface--full">
      <Settings state={state} />
    </main>
  );
}

function DetachedSettingsApp({ state }: { state: BootstrapState }) {
  const [tab, setTab] = useState(initialSettingsTab);

  useEffect(() => {
    // Listen for tab-change events from Rust (when the window is re-focused
    // with a different tab request).
    const unlisten = listen<string>("settings-change-tab", (event) => {
      setTab(event.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <Suspense fallback={<SurfaceFallback />}>
      <RevealAfterPaint>
        <main className="settings-surface settings-surface--full">
          <Settings state={state} initialTab={tab} />
        </main>
      </RevealAfterPaint>
    </Suspense>
  );
}
