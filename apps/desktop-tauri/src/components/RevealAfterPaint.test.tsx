import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import RevealAfterPaint from "./RevealAfterPaint";

const tauriMocks = vi.hoisted(() => ({
  revealReadyWindow: vi.fn(() => Promise.resolve()),
}));
vi.mock("../lib/tauri", () => tauriMocks);

let frames: Map<number, FrameRequestCallback>;
let nextFrame: number;

beforeEach(() => {
  frames = new Map();
  nextFrame = 1;
  tauriMocks.revealReadyWindow.mockClear();
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    const id = nextFrame++;
    frames.set(id, callback);
    return id;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
    frames.delete(id);
  });
});

afterEach(() => vi.restoreAllMocks());

function paintFrame() {
  const pending = [...frames.values()];
  frames.clear();
  act(() => pending.forEach((callback) => callback(0)));
}

it("reveals only after a committed surface has had a paint frame", () => {
  const { getByText } = render(<RevealAfterPaint><div>Settings content</div></RevealAfterPaint>);
  expect(getByText("Settings content")).toBeInTheDocument();
  expect(tauriMocks.revealReadyWindow).not.toHaveBeenCalled();

  paintFrame();
  expect(tauriMocks.revealReadyWindow).not.toHaveBeenCalled();
  paintFrame();
  expect(tauriMocks.revealReadyWindow).toHaveBeenCalledTimes(1);
});

it("does not reveal a surface that was dismissed before its paint", () => {
  const view = render(<RevealAfterPaint><div>Dashboard</div></RevealAfterPaint>);
  paintFrame();
  view.unmount();
  paintFrame();

  expect(tauriMocks.revealReadyWindow).not.toHaveBeenCalled();
});
