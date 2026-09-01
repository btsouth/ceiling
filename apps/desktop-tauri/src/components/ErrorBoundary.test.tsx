import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  type MockInstance,
  vi,
} from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import ErrorBoundary, { describeThrown } from "./ErrorBoundary";
import { expectNoAccessibilityViolations } from "../test/accessibility";

const webviewWindowMocks = vi.hoisted(() => ({ label: "main" }));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ label: webviewWindowMocks.label }),
}));

const tauriMocks = vi.hoisted(() => ({
  revealTrayPanelWindow: vi.fn(() => Promise.resolve()),
}));

vi.mock("../lib/tauri", () => tauriMocks);

function Thrower({ value }: { value: unknown }): never {
  throw value;
}

/**
 * Throws while `broken.current` is set, renders normally otherwise — the
 * "one bad snapshot, fixed by the next tick" case. Module state rather than
 * component state because a boundary reset remounts its children.
 */
const broken = { current: true };
function ThrowsWhileBroken() {
  if (broken.current) {
    throw new Error("transient");
  }
  return <div data-testid="recovered">recovered</div>;
}

describe("ErrorBoundary", () => {
  let consoleError: MockInstance<typeof console.error>;

  beforeEach(() => {
    webviewWindowMocks.label = "main";
    tauriMocks.revealTrayPanelWindow.mockClear();
    // React logs the caught error and its stack on its own; keep test output
    // readable while still asserting the boundary's own report below.
    consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    consoleError.mockRestore();
  });

  it("renders its children when nothing throws", () => {
    render(
      <ErrorBoundary>
        <div data-testid="child">fine</div>
      </ErrorBoundary>,
    );

    expect(screen.getByTestId("child")).toHaveTextContent("fine");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("replaces a throwing tree with an opaque alert naming the error", () => {
    render(
      <StrictMode>
        <ErrorBoundary>
          <Thrower
            value={new TypeError("Cannot read properties of undefined (reading 'resetsAt')")}
          />
        </ErrorBoundary>
      </StrictMode>,
    );

    const alert = screen.getByRole("alert");
    expect(alert).toHaveClass("error-boundary");
    expect(alert).toHaveTextContent("Something went wrong");
    expect(alert).toHaveTextContent(
      "TypeError: Cannot read properties of undefined (reading 'resetsAt')",
    );
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
  });

  it.each([
    ["null", null, "null"],
    ["undefined", undefined, "undefined"],
    ["a string", "plain string", "plain string"],
    ["an object", { code: 7 }, "[object Object]"],
  ])("still catches when a child throws %s", (_label, value, expected) => {
    render(
      <ErrorBoundary>
        <Thrower value={value} />
      </ErrorBoundary>,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(expected);
  });

  it("describes errors by name and message and other values by String()", () => {
    expect(describeThrown(new RangeError("out of range"))).toBe(
      "RangeError: out of range",
    );
    expect(describeThrown(new Error(""))).toBe("Error");
    expect(describeThrown(42)).toBe("42");
  });

  it("survives a value that String() cannot convert", () => {
    // `String()` throws for these; the description runs inside the fallback
    // render, so a throw here would unmount the root and blank the window.
    expect(describeThrown(Object.create(null))).toBe("[object Object]");
    const hostile = {
      [Symbol.toPrimitive]() {
        throw new Error("no primitive for you");
      },
    };
    expect(describeThrown(hostile)).toBe("[object Object]");

    render(
      <ErrorBoundary>
        <Thrower value={Object.create(null)} />
      </ErrorBoundary>,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("[object Object]");
  });

  it("reports the error and component stack to the console", () => {
    render(
      <ErrorBoundary>
        <Thrower value={new Error("boom")} />
      </ErrorBoundary>,
    );

    const report = consoleError.mock.calls.find(
      (call) => call[0] === "[ErrorBoundary] uncaught render error",
    );
    expect(report).toBeDefined();
    expect(report?.[1]).toBeInstanceOf(Error);
    expect(String(report?.[2])).toContain("Thrower");
  });

  it("re-renders the children from Try again", () => {
    broken.current = true;
    render(
      <ErrorBoundary>
        <ThrowsWhileBroken />
      </ErrorBoundary>,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("transient");

    broken.current = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(screen.getByTestId("recovered")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows the fallback again if Try again still throws", () => {
    broken.current = true;
    render(
      <ErrorBoundary>
        <ThrowsWhileBroken />
      </ErrorBoundary>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(screen.getByRole("alert")).toHaveTextContent("transient");
  });

  it("reloads the page from Reload", () => {
    const reload = vi.fn();
    render(
      <ErrorBoundary reload={reload}>
        <Thrower value={new Error("boom")} />
      </ErrorBoundary>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Reload" }));

    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("asks the shell to reveal the flyout so the fallback is not painted into a hidden window", () => {
    webviewWindowMocks.label = "flyout";

    render(
      <ErrorBoundary>
        <Thrower value={new Error("boom")} />
      </ErrorBoundary>,
    );

    expect(tauriMocks.revealTrayPanelWindow).toHaveBeenCalledTimes(1);
  });

  it("leaves other windows alone — their shells show them from Rust", () => {
    webviewWindowMocks.label = "settings";

    render(
      <ErrorBoundary>
        <Thrower value={new Error("boom")} />
      </ErrorBoundary>,
    );

    expect(tauriMocks.revealTrayPanelWindow).not.toHaveBeenCalled();
  });

  it("has no accessibility violations in its fallback", async () => {
    const { container } = render(
      <ErrorBoundary>
        <Thrower value={new Error("boom")} />
      </ErrorBoundary>,
    );

    await expectNoAccessibilityViolations(container);
  });
});
