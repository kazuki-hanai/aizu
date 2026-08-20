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
import { SafeMarkdown } from "./SafeMarkdown";

type SwipeState = {
  axis: "pending" | "horizontal";
  input: "mouse" | "pointer";
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
  onAcknowledgeApproval,
  onActivate,
  onDecideApproval,
  onDismiss,
}: {
  banner: BannerNotification;
  onAcknowledgeApproval: (id: number) => Promise<boolean>;
  onActivate: (id: number) => Promise<boolean>;
  onDecideApproval: (id: number, decision: "allowOnce" | "deny") => Promise<boolean>;
  onDismiss: (id: number) => Promise<boolean>;
}) {
  const bannerRef = useRef<HTMLElement>(null);
  const swipe = useRef<SwipeState | null>(null);
  const offset = useRef(0);
  const dismissTimer = useRef<number | null>(null);
  const dismissStarted = useRef(false);
  const activationStarted = useRef(false);
  const approvalStarted = useRef(false);
  const suppressActivation = useRef(false);
  const suppressionTimer = useRef<number | null>(null);
  const [renderedOffset, setRenderedOffset] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [exitDirection, setExitDirection] = useState<-1 | 1 | null>(null);
  const [approvalPending, setApprovalPending] = useState(false);
  const requiresApproval = banner.approval !== null;
  const [acknowledgedApprovalId, setAcknowledgedApprovalId] = useState<number | null>(null);
  const approvalReady = !requiresApproval || acknowledgedApprovalId === banner.id;

  useEffect(() => {
    if (!requiresApproval) return;
    let active = true;
    void onAcknowledgeApproval(banner.id).then((acknowledged) => {
      if (active && acknowledged) setAcknowledgedApprovalId(banner.id);
    });
    return () => {
      active = false;
    };
  }, [banner.id, onAcknowledgeApproval, requiresApproval]);

  const reset = useCallback(() => {
    swipe.current = null;
    offset.current = 0;
    setRenderedOffset(0);
    setDragging(false);
    setExitDirection(null);
    dismissStarted.current = false;
    activationStarted.current = false;
    approvalStarted.current = false;
    setApprovalPending(false);
  }, []);

  useEffect(() => () => {
    if (dismissTimer.current !== null) window.clearTimeout(dismissTimer.current);
    if (suppressionTimer.current !== null) window.clearTimeout(suppressionTimer.current);
  }, []);

  const suppressSyntheticClick = useCallback(() => {
    suppressActivation.current = true;
    if (suppressionTimer.current !== null) window.clearTimeout(suppressionTimer.current);
    suppressionTimer.current = window.setTimeout(() => {
      suppressionTimer.current = null;
      suppressActivation.current = false;
    }, 0);
  }, []);

  const finish = useCallback((input: SwipeState["input"], pointerId: number, cancelled = false) => {
    const active = swipe.current;
    if (active?.input !== input || active.pointerId !== pointerId) return;
    swipe.current = null;
    setDragging(false);
    if (dismissStarted.current) {
      offset.current = 0;
      setRenderedOffset(0);
      return;
    }
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

  const begin = (
    input: SwipeState["input"],
    pointerId: number,
    clientX: number,
    clientY: number,
    target: EventTarget,
  ) => {
    if (
      swipe.current ||
      dismissStarted.current ||
      activationStarted.current ||
      approvalStarted.current ||
      exitDirection !== null
    ) return false;
    if (target instanceof Element && target.closest("button")) return false;
    swipe.current = {
      axis: "pending",
      input,
      pointerId,
      startX: clientX,
      startY: clientY,
    };
    offset.current = 0;
    return true;
  };

  const move = useCallback((
    input: SwipeState["input"],
    pointerId: number,
    clientX: number,
    clientY: number,
    preventDefault: () => void,
  ) => {
    const active = swipe.current;
    if (
      active?.input !== input ||
      active.pointerId !== pointerId ||
      dismissStarted.current ||
      exitDirection !== null
    ) return;
    const deltaX = clientX - active.startX;
    const deltaY = clientY - active.startY;
    if (active.axis === "pending") {
      if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < AXIS_THRESHOLD) return;
      if (Math.abs(deltaX) <= Math.abs(deltaY)) {
        suppressSyntheticClick();
        swipe.current = null;
        return;
      }
      active.axis = "horizontal";
      suppressSyntheticClick();
      window.getSelection()?.removeAllRanges();
      setDragging(true);
    }
    preventDefault();
    const width = Math.max(bannerRef.current?.getBoundingClientRect().width ?? 0, DISMISS_THRESHOLD);
    offset.current = Math.max(-width, Math.min(width, deltaX));
    setRenderedOffset(offset.current);
  }, [exitDirection, suppressSyntheticClick]);

  useEffect(() => {
    const handleMouseMove = (event: MouseEvent) => {
      move("mouse", 0, event.clientX, event.clientY, () => event.preventDefault());
    };
    const handleMouseUp = () => finish("mouse", 0);
    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, [finish, move]);

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
    if (dismissStarted.current || activationStarted.current || approvalStarted.current) return;
    dismissStarted.current = true;
    void onDismiss(banner.id).then((dismissed) => {
      if (!dismissed) reset();
    });
  };
  const activateImmediately = () => {
    if (!banner.canActivateTerminal || activationStarted.current || dismissStarted.current) return;
    if (suppressActivation.current) {
      suppressActivation.current = false;
      if (suppressionTimer.current !== null) {
        window.clearTimeout(suppressionTimer.current);
        suppressionTimer.current = null;
      }
      return;
    }
    const selection = window.getSelection();
    if (selection && !selection.isCollapsed) return;
    activationStarted.current = true;
    void onActivate(banner.id).then((activated) => {
      if (!activated) reset();
    });
  };
  const decideApproval = (decision: "allowOnce" | "deny") => {
    if (
      !banner.approval ||
      !approvalReady ||
      approvalStarted.current ||
      dismissStarted.current
    ) return;
    approvalStarted.current = true;
    setApprovalPending(true);
    void onDecideApproval(banner.id, decision).then((decided) => {
      if (!decided) reset();
    });
  };
  return (
    <article
      className={classes}
      data-banner-id={banner.id}
      data-text-size={banner.textSize}
      data-terminal-activation={banner.canActivateTerminal ? "available" : "unavailable"}
      data-command-approval={banner.approval ? "available" : "unavailable"}
      lang={banner.language === "system" ? undefined : banner.language}
      onMouseDown={(event) => {
        if (event.button === 0) begin("mouse", 0, event.clientX, event.clientY, event.target);
      }}
      onPointerCancel={(event) => finish("pointer", event.pointerId, true)}
      onPointerDown={(event) => {
        if (
          event.isPrimary &&
          event.button === 0 &&
          begin("pointer", event.pointerId, event.clientX, event.clientY, event.target)
        ) event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onLostPointerCapture={() => {
        if (swipe.current?.input === "pointer") reset();
      }}
      onPointerMove={(event) => {
        move("pointer", event.pointerId, event.clientX, event.clientY, () => event.preventDefault());
      }}
      onPointerUp={(event: ReactPointerEvent<HTMLElement>) => {
        const captured = event.currentTarget.hasPointerCapture(event.pointerId);
        finish("pointer", event.pointerId);
        if (captured) event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      ref={bannerRef}
      style={style}
    >
      <div
        className="aizu-banner__content"
        onClick={activateImmediately}
        onKeyDown={(event) => {
          if (banner.canActivateTerminal && (event.key === "Enter" || event.key === " ")) {
            event.preventDefault();
            activateImmediately();
          }
        }}
        role={banner.canActivateTerminal ? "button" : undefined}
        tabIndex={banner.canActivateTerminal ? 0 : undefined}
        title={banner.canActivateTerminal ? messages(banner.language).returnToTerminal : undefined}
      >
        <span className="aizu-banner__mark"><BrandMark small /></span>
        <div className="aizu-banner__copy">
          <strong>{banner.title}</strong>
          {banner.body ? <SafeMarkdown>{banner.body}</SafeMarkdown> : null}
          {banner.approval ? (
            <>
              <span className="aizu-banner__approval-tool">{banner.approval.toolName}</span>
              <pre className="aizu-banner__command"><code>{banner.approval.command}</code></pre>
              <div className="aizu-banner__actions">
                <button
                  className="aizu-banner__action aizu-banner__action--deny"
                  disabled={!approvalReady || approvalPending}
                  onClick={() => decideApproval("deny")}
                  type="button"
                >
                  {messages(banner.language).deny}
                </button>
                <button
                  className="aizu-banner__action aizu-banner__action--allow"
                  disabled={!approvalReady || approvalPending}
                  onClick={() => decideApproval("allowOnce")}
                  type="button"
                >
                  {messages(banner.language).allowOnce}
                </button>
              </div>
            </>
          ) : null}
        </div>
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
