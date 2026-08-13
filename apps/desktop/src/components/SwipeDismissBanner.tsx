import { X } from "lucide-react";
import {
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

import type { BannerNotification } from "../lib/contracts";
import { messages } from "../lib/i18n";
import { BrandMark } from "./BrandMark";

type SwipeState = {
  axis: "pending" | "horizontal";
  pointerId: number;
  startX: number;
  startY: number;
};

type BannerStyle = CSSProperties & {
  "--aizu-swipe-offset": string;
};

const AXIS_THRESHOLD = 8;
const DISMISS_THRESHOLD = 72;
const EXIT_DURATION = 160;

export function SwipeDismissBanner({
  banner,
  onDismiss,
}: {
  banner: BannerNotification;
  onDismiss: (id: number) => Promise<boolean>;
}) {
  const swipe = useRef<SwipeState | null>(null);
  const offset = useRef(0);
  const dismissTimer = useRef<number | null>(null);
  const dismissStarted = useRef(false);
  const [renderedOffset, setRenderedOffset] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [exitDirection, setExitDirection] = useState<-1 | 1 | null>(null);

  const reset = useCallback(() => {
    swipe.current = null;
    offset.current = 0;
    setRenderedOffset(0);
    setDragging(false);
    setExitDirection(null);
    dismissStarted.current = false;
  }, []);

  useEffect(() => () => {
    if (dismissTimer.current !== null) window.clearTimeout(dismissTimer.current);
  }, []);

  const finish = useCallback((event: ReactPointerEvent<HTMLElement>, cancelled = false) => {
    const active = swipe.current;
    if (active?.pointerId !== event.pointerId) return;
    swipe.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    setDragging(false);
    if (cancelled || active.axis !== "horizontal" || Math.abs(offset.current) < DISMISS_THRESHOLD) {
      offset.current = 0;
      setRenderedOffset(0);
      return;
    }

    const direction = offset.current < 0 ? -1 : 1;
    setExitDirection(direction);
    dismissStarted.current = true;
    const delay = window.matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : EXIT_DURATION;
    dismissTimer.current = window.setTimeout(() => {
      dismissTimer.current = null;
      void onDismiss(banner.id).then((dismissed) => {
        if (!dismissed) reset();
      });
    }, delay);
  }, [banner.id, onDismiss, reset]);

  const handlePointerDown = (event: ReactPointerEvent<HTMLElement>) => {
    if (!event.isPrimary || event.button !== 0 || exitDirection !== null) return;
    if (event.target instanceof Element && event.target.closest("button, .aizu-banner__body")) return;
    swipe.current = {
      axis: "pending",
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
    };
    offset.current = 0;
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const handlePointerMove = (event: ReactPointerEvent<HTMLElement>) => {
    const active = swipe.current;
    if (active?.pointerId !== event.pointerId || exitDirection !== null) return;
    const deltaX = event.clientX - active.startX;
    const deltaY = event.clientY - active.startY;
    if (active.axis === "pending") {
      if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < AXIS_THRESHOLD) return;
      if (Math.abs(deltaX) <= Math.abs(deltaY)) {
        swipe.current = null;
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
        return;
      }
      active.axis = "horizontal";
      window.getSelection()?.removeAllRanges();
      setDragging(true);
    }
    event.preventDefault();
    const width = Math.max(event.currentTarget.getBoundingClientRect().width, DISMISS_THRESHOLD);
    offset.current = Math.max(-width, Math.min(width, deltaX));
    setRenderedOffset(offset.current);
  };

  const style: BannerStyle = {
    "--aizu-swipe-offset": `${String(renderedOffset)}px`,
  };
  const classes = [
    "aizu-banner",
    dragging ? "aizu-banner--dragging" : "",
    exitDirection === -1 ? "aizu-banner--dismiss-left" : "",
    exitDirection === 1 ? "aizu-banner--dismiss-right" : "",
  ].filter(Boolean).join(" ");
  const dismissImmediately = () => {
    if (dismissStarted.current) return;
    dismissStarted.current = true;
    void onDismiss(banner.id).then((dismissed) => {
      if (!dismissed) reset();
    });
  };

  return (
    <article
      className={classes}
      data-banner-id={banner.id}
      lang={banner.language === "system" ? undefined : banner.language}
      onPointerCancel={(event) => finish(event, true)}
      onPointerDown={handlePointerDown}
      onLostPointerCapture={() => {
        if (swipe.current) reset();
      }}
      onPointerMove={handlePointerMove}
      onPointerUp={(event) => finish(event)}
      style={style}
    >
      <div className="aizu-banner__content">
        <span className="aizu-banner__mark"><BrandMark small /></span>
        <span className="aizu-banner__copy">
          <strong>{banner.title}</strong>
          {banner.body ? <span className="aizu-banner__body">{banner.body}</span> : null}
        </span>
      </div>
      <button
        aria-label={messages(banner.language).dismissNotification}
        className="aizu-banner__dismiss"
        onClick={dismissImmediately}
        title={messages(banner.language).dismissNotification}
        type="button"
      >
        <X aria-hidden="true" size={16} />
      </button>
    </article>
  );
}
