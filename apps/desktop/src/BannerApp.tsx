import { X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { BrandMark } from "./components/BrandMark";
import { bannerBackend, type BannerClient } from "./lib/backend";
import type { BannerNotification } from "./lib/contracts";
import { messages } from "./lib/i18n";

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
        <article className="aizu-banner" key={banner.id} lang={banner.language === "system" ? undefined : banner.language}>
          <button
            aria-label={messages(banner.language).openAizu}
            className="aizu-banner__content"
            onClick={() => void runAction(() => client.open(banner.id))}
            type="button"
          >
            <span className="aizu-banner__mark"><BrandMark small /></span>
            <span className="aizu-banner__copy">
              <strong>{banner.title}</strong>
              {banner.body ? <span className="aizu-banner__body">{banner.body}</span> : null}
            </span>
          </button>
          <button
            aria-label={messages(banner.language).dismissNotification}
            className="aizu-banner__dismiss"
            onClick={() => void runAction(() => client.dismiss(banner.id))}
            title={messages(banner.language).dismissNotification}
            type="button"
          >
            <X aria-hidden="true" size={16} />
          </button>
        </article>
      ))}
    </main>
  );
}
