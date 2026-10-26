import { describe, expect, it } from "vitest";
import tauriConfigSource from "../../src-tauri/tauri.conf.json?raw";
import settingsWindowSource from "../../src-tauri/src/shell/settings_window.rs?raw";
import flyoutWindowSource from "../../src-tauri/src/shell/flyout_window.rs?raw";
import dwmSource from "../../src-tauri/src/shell/dwm.rs?raw";
import mainCapabilitySource from "../../src-tauri/capabilities/main.json?raw";
import flyoutCapabilitySource from "../../src-tauri/capabilities/flyout.json?raw";
import floatbarCapabilitySource from "../../src-tauri/capabilities/floatbar.json?raw";

function parseJson(source: string) {
  return JSON.parse(source) as Record<string, unknown>;
}

describe("native theme configuration", () => {
  it("does not force dark on startup-created windows", () => {
    const config = parseJson(tauriConfigSource) as {
      app: { windows: Array<Record<string, unknown>> };
    };

    expect(config.app.windows.length).toBeGreaterThan(0);
    for (const window of config.app.windows) {
      expect(Object.prototype.hasOwnProperty.call(window, "theme")).toBe(false);
    }
  });

  it("does not force dark on dynamically-created settings and flyout windows", () => {
    const forcedDarkTheme = /\.theme\s*\(\s*Some\s*\(\s*tauri::Theme::Dark\s*\)\s*\)/;

    expect(settingsWindowSource).not.toMatch(forcedDarkTheme);
    expect(flyoutWindowSource).not.toMatch(forcedDarkTheme);
  });

  it("does not force dark DWM colors on borderless windows", () => {
    expect(dwmSource).not.toMatch(/DWMWA_USE_IMMERSIVE_DARK_MODE|DWMWA_CAPTION_COLOR/);
  });

  it("grants set-theme only to the app's existing Tauri window groups", () => {
    const capabilitySources = [
      mainCapabilitySource,
      flyoutCapabilitySource,
      floatbarCapabilitySource,
    ];
    const themedWindows = capabilitySources.flatMap((source) => {
      const capability = parseJson(source) as {
        permissions: string[];
        windows: string[];
      };
      return capability.permissions.includes("core:window:allow-set-theme")
        ? capability.windows
        : [];
    });

    expect(new Set(themedWindows)).toEqual(new Set(["main", "settings", "flyout", "floatbar"]));
  });
});
