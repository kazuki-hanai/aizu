import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { SwipeDismissBanner } from "./components/SwipeDismissBanner";
import { bannerBackend, type BannerClient } from "./lib/backend";
import type { BannerNotification } from "./lib/contracts";

type BannerAppProps = {
  client?: BannerClient;
};

const BANNER_RECONCILIATION_INTERVAL_MS = 500;

export function BannerApp({ client = bannerBackend }: BannerAppProps) {
  const [banners, setBanners] = useState<BannerNotification[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const stackRef = useRef<HTMLElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const refreshGeneration = useRef(0);
  const refreshInFlight = useRef(false);
  const refreshRequestVersion = useRef(0);
  const presentedBanners = useMemo(() => {
    const approval = banners.find((banner) => banner.approval !== null);
    return approval ? [approval] : banners;
  }, [banners]);
  const approvalMode = presentedBanners.some((banner) => banner.approval !== null);

  const refresh = useCallback(async () => {
    refreshRequestVersion.current += 1;
    if (refreshInFlight.current) {
      return;
    }
    refreshInFlight.current = true;
    try {
      let handledRequestVersion: number;
      do {
        handledRequestVersion = refreshRequestVersion.current;
        const generation = refreshGeneration.current;
        try {
          const nextBanners = await client.getBanners();
          if (refreshGeneration.current !== generation) continue;
          setBanners(nextBanners);
          setUnavailable(false);
        } catch {
          if (refreshGeneration.current !== generation) continue;
          setUnavailable(true);
        }
      } while (handledRequestVersion !== refreshRequestVersion.current);
    } finally {
      refreshInFlight.current = false;
    }
  }, [client]);

  const runAction = useCallback(async (action: () => Promise<void>) => {
    try {
      await action();
      setUnavailable(false);
    } catch {
      setUnavailable(true);
    }
  }, []);

  const dismiss = useCallback(async (id: number) => {
    try {
      await client.dismiss(id);
      refreshGeneration.current += 1;
      setBanners((current) => current.filter((banner) => banner.id !== id));
      setUnavailable(false);
      await refresh();
      return true;
    } catch {
      setUnavailable(true);
      return false;
    }
  }, [client, refresh]);

  const activate = useCallback(async (id: number) => {
    try {
      await client.activate(id);
      refreshGeneration.current += 1;
      setBanners((current) => current.filter((banner) => banner.id !== id));
      setUnavailable(false);
      await refresh();
      return true;
    } catch {
      setUnavailable(true);
      return false;
    }
  }, [client, refresh]);

  const acknowledgeApproval = useCallback(async (id: number) => {
    try {
      await client.acknowledgeApproval(id);
      setUnavailable(false);
      return true;
    } catch {
      // A stale render acknowledgement can race an explicit terminal fallback.
      // Decision controls remain disabled, while terminal fallback stays available.
      return false;
    }
  }, [client]);

  const decideApproval = useCallback(async (id: number, decision: "allowOnce" | "deny") => {
    try {
      await client.decideApproval(id, decision);
      refreshGeneration.current += 1;
      setBanners((current) => current.filter((banner) => banner.id !== id));
      setUnavailable(false);
      await refresh();
      return true;
    } catch {
      setUnavailable(true);
      return false;
    }
  }, [client, refresh]);

  const answerQuestion = useCallback(async (id: number, optionIndex: number) => {
    try {
      await client.answerQuestion(id, optionIndex);
      refreshGeneration.current += 1;
      setBanners((current) => current.filter((banner) => banner.id !== id));
      setUnavailable(false);
      await refresh();
      return true;
    } catch {
      setUnavailable(true);
      return false;
    }
  }, [client, refresh]);

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;
    const wake = () => {
      if (active) void refresh();
    };
    window.addEventListener("aizu-banner-refresh", wake);
    void client.subscribe((nextBanners) => {
      if (!active) return;
      refreshGeneration.current += 1;
      setBanners(nextBanners);
      setUnavailable(false);
    }).then((stop) => {
      if (active) {
        unsubscribe = stop;
        void refresh();
      }
      else stop();
    }).catch(() => {
      if (active) setUnavailable(true);
    });
    queueMicrotask(() => {
      if (active) void refresh();
    });
    const reconciliation = window.setInterval(() => {
      if (active) void refresh();
    }, BANNER_RECONCILIATION_INTERVAL_MS);
    return () => {
      active = false;
      refreshGeneration.current += 1;
      window.clearInterval(reconciliation);
      window.removeEventListener("aizu-banner-refresh", wake);
      unsubscribe?.();
    };
  }, [client, refresh]);

  useEffect(() => {
    const stack = stackRef.current;
    const content = contentRef.current;
    if (!stack || !content) return;
    const resize = () => {
      const style = window.getComputedStyle(stack);
      const padding = Number.parseFloat(style.paddingTop) + Number.parseFloat(style.paddingBottom);
      const requestedHeight = content.scrollHeight + (Number.isFinite(padding) ? padding : 0);
      void runAction(() => client.resize(requestedHeight));
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(content);
    return () => observer.disconnect();
  }, [banners, client, runAction, unavailable]);

  return (
    <main
      aria-label="Aizu notifications"
      className={`banner-stack${approvalMode ? " banner-stack--approval" : ""}`}
      data-presentation={approvalMode ? "approval" : "passive"}
      ref={stackRef}
    >
      <div className="banner-stack__content" ref={contentRef}>
        {unavailable ? <p className="banner-error">Aizu Banner is unavailable.</p> : null}
        {presentedBanners.map((banner) => (
          <SwipeDismissBanner
            banner={banner}
            key={banner.id}
            onAcknowledgeApproval={acknowledgeApproval}
            onActivate={activate}
            onAnswerQuestion={answerQuestion}
            onDecideApproval={decideApproval}
            onDismiss={dismiss}
          />
        ))}
      </div>
    </main>
  );
}
