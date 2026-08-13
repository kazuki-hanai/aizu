import { AlertTriangle, RefreshCw, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";

import { AppShell } from "./components/AppShell";
import { Onboarding } from "./components/Onboarding";
import { defaultBackend, type BackendClient } from "./lib/backend";
import type { AppView, Preferences } from "./lib/contracts";
import { messages, resolveLanguage } from "./lib/i18n";

type AppProps = {
  backend?: BackendClient;
};

export function App({ backend = defaultBackend }: AppProps) {
  const [view, setView] = useState<AppView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setError(null);
    try {
      setView(await backend.getView());
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : messages("system").appLoadError);
    }
  }, [backend]);

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;
    void backend.getView().then((initialView) => {
      if (active) setView(initialView);
    }).catch((caught: unknown) => {
      if (active) {
        setError(caught instanceof Error ? caught.message : messages("system").appLoadError);
      }
    });
    void backend.subscribe((nextView) => {
      if (active) setView(nextView);
    }).then((unlisten) => {
      if (active) unsubscribe = unlisten;
      else unlisten();
    }).catch(() => undefined);

    return () => {
      active = false;
      unsubscribe?.();
    };
  }, [backend]);

  useEffect(() => {
    if (error === null || view === null) return undefined;
    const timeout = window.setTimeout(() => setError(null), 6000);
    return () => window.clearTimeout(timeout);
  }, [error, view]);

  useEffect(() => {
    if (view === null) return;
    document.documentElement.lang = resolveLanguage(view.preferences.language);
  }, [view]);

  const runActionResult = useCallback(async (action: () => Promise<AppView>) => {
    setBusy(true);
    setError(null);
    try {
      setView(await action());
      return true;
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : messages(view?.preferences.language ?? "system").actionError);
      return false;
    } finally {
      setBusy(false);
    }
  }, [view?.preferences.language]);
  const runAction = useCallback(async (action: () => Promise<AppView>) => {
    await runActionResult(action);
  }, [runActionResult]);
  const t = messages(view?.preferences.language ?? "system");

  if (error !== null && view === null) {
    return (
      <main className="fatal-state" role="alert">
        <AlertTriangle aria-hidden="true" size={30} />
        <h1>{t.appStartError}</h1>
        <p>{error}</p>
        <button className="button button--secondary" onClick={() => void load()} type="button">
          <RefreshCw aria-hidden="true" size={16} />
          {t.retry}
        </button>
      </main>
    );
  }

  if (view === null) {
    return (
      <main aria-busy="true" aria-label={t.loading} className="loading-state">
        <span className="loading-mark" />
        <span>{t.loading}</span>
      </main>
    );
  }

  const actionError = error === null ? null : (
    <div className="action-error" role="alert">
      <AlertTriangle aria-hidden="true" size={16} />
      <span>{error}</span>
      <button aria-label={t.dismissError} className="action-error__close" onClick={() => setError(null)} type="button">
        <X aria-hidden="true" size={15} />
      </button>
    </div>
  );

  if (!view.onboardingComplete) {
    return (
      <>
        {actionError}
        <Onboarding
          busy={busy}
          onComplete={async (launchAtLogin) =>
            runAction(() => backend.completeOnboarding({ launchAtLogin }))
          }
          onConfigureAgents={async () => runAction(() => backend.configureAgents())}
          onConfirmCodexTrust={async () => runAction(() => backend.confirmCodexHookTrust())}
          onLanguageChange={async (language) =>
            runAction(() => backend.updatePreferences({ ...view.preferences, language }))
          }
          onRequestPermission={async () =>
            runAction(() => backend.requestNotificationPermission())
          }
          view={view}
        />
      </>
    );
  }

  return (
    <>
      {actionError}
      <AppShell
        onAddRemoteSource={async (hostAlias, localLabel) =>
          runActionResult(() => backend.addRemoteSource(hostAlias, localLabel))
        }
        onTestRemoteConnection={(hostAlias) => backend.testSshConnection(hostAlias)}
        onConfirmRemoteIdentity={async (hostAlias) =>
          runAction(() => backend.confirmRemoteIdentity(hostAlias))
        }
        onInstallCli={async () => runAction(() => backend.installCli())}
        onConfigureAgents={async () => runAction(() => backend.configureAgents())}
        onConfirmCodexTrust={async () => runAction(() => backend.confirmCodexHookTrust())}
        onClearHistory={async () => runAction(() => backend.clearHistory())}
        busy={busy}
        onPauseChange={async (paused) =>
          runAction(() => backend.setNotificationsPaused(paused))
        }
        onReconnectRemoteSource={async (hostAlias) =>
          runAction(() => backend.reconnectRemoteSource(hostAlias))
        }
        onRemoveRemoteSource={async (hostAlias) =>
          runAction(() => backend.removeRemoteSource(hostAlias))
        }
        onSendTest={async () => runAction(async () => {
          const permissionView = view.notificationPermission === "granted"
            ? view
            : await backend.requestNotificationPermission();
          if (permissionView.notificationPermission !== "granted") {
            throw new Error(t.notificationBannersDisabled);
          }
          return backend.sendTestNotification();
        })}
        onUpdatePreferences={async (preferences: Preferences) =>
          runAction(() => backend.updatePreferences(preferences))
        }
        view={view}
      />
    </>
  );
}
