import { useCallback, useEffect, useRef, useState } from "react";

import { SwipeDismissBanner } from "./components/SwipeDismissBanner";
import { bannerBackend, type BannerClient } from "./lib/backend";
import type { BannerNotification } from "./lib/contracts";

type BannerAppProps = {
  client?: BannerClient;
};

export function BannerApp({ client = bannerBackend }: BannerAppProps) {
  const [banners, setBanners] = useState<BannerNotification[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const stackRef = useRef<HTMLElement>(null);
  const refreshGeneration = useRef(0);

  const refresh = useCallback(async () => {
    const generation = ++refreshGeneration.current;
    try {
      const nextBanners = await client.getBanners();
      if (refreshGeneration.current !== generation) return;
      setBanners(nextBanners);
      setUnavailable(false);
    } catch {
      if (refreshGeneration.current !== generation) return;
      setUnavailable(true);
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

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;
    void client.subscribe(() => {
      if (active) void refresh();
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
    return () => {
      active = false;
      refreshGeneration.current += 1;
      unsubscribe?.();
    };
  }, [client, refresh]);

  useEffect(() => {
    const stack = stackRef.current;
    if (!stack) return;
    const resize = () => void runAction(() => client.resize(stack.scrollHeight));
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(stack);
    return () => observer.disconnect();
  }, [banners, client, runAction, unavailable]);

  return (
    <main aria-label="Aizu notifications" className="banner-stack" ref={stackRef}>
      {unavailable ? <p className="banner-error">Aizu Banner is unavailable.</p> : null}
      {banners.map((banner) => (
        <SwipeDismissBanner banner={banner} key={banner.id} onDismiss={dismiss} />
      ))}
    </main>
  );
}
