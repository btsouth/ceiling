import { useEffect, type ReactNode } from "react";
import { revealReadyWindow } from "../lib/tauri";

/** Let the browser paint the mounted surface before releasing its native frame. */
export default function RevealAfterPaint({
  children,
  reveal = revealReadyWindow,
}: {
  children: ReactNode;
  reveal?: () => Promise<void>;
}) {
  useEffect(() => {
    let nextFrame: number | null = null;
    const firstFrame = window.requestAnimationFrame(() => {
      nextFrame = window.requestAnimationFrame(() => {
        void reveal().catch(() => {});
      });
    });
    return () => {
      window.cancelAnimationFrame(firstFrame);
      if (nextFrame !== null) window.cancelAnimationFrame(nextFrame);
    };
  }, [reveal]);

  return <>{children}</>;
}
