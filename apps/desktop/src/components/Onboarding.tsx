import { Bell, Check, ChevronRight, Languages, LockKeyhole, Radio, Wrench } from "lucide-react";
import { useState } from "react";

import type { AppView, Preferences } from "../lib/contracts";
import { messages } from "../lib/i18n";
import { BrandMark } from "./BrandMark";

type OnboardingProps = {
  view: AppView;
  busy: boolean;
  onRequestPermission: () => Promise<void>;
  onComplete: (launchAtLogin: boolean) => Promise<void>;
  onConfigureAgents: () => Promise<void>;
  onConfirmCodexTrust: () => Promise<void>;
  onLanguageChange: (language: Preferences["language"]) => Promise<void>;
};

export function Onboarding({
  view,
  busy,
  onRequestPermission,
  onComplete,
  onConfigureAgents,
  onConfirmCodexTrust,
  onLanguageChange,
}: OnboardingProps) {
  const [launchAtLogin, setLaunchAtLogin] = useState(true);
  const permissionGranted = view.notificationPermission === "granted";
  const permissionDenied = view.notificationPermission === "denied"
    || view.notificationPermission === "alertsDisabled";
  const agentsConfigured = view.cliStatus === "installed"
    && view.agentMonitors.every((agent) => agent.hookStatus === "configured");
  const codexApprovalRequired = view.agentMonitors.some((agent) => agent.hookStatus === "approvalRequired");
  const t = messages(view.preferences.language);

  return (
    <main className="onboarding-shell">
      <div className="onboarding-brand" aria-label="Aizu">
        <BrandMark />
        <strong>Aizu</strong>
      </div>

      <section className="onboarding-content" aria-labelledby="onboarding-title">
        <label className="onboarding-language">
          <Languages aria-hidden="true" size={16} />
          <span>{t.language}</span>
          <select
            aria-label={t.language}
            disabled={busy}
            onChange={(event) => {
              const language = event.target.value;
              if (language === "system" || language === "ja" || language === "en") {
                void onLanguageChange(language);
              }
            }}
            value={view.preferences.language}
          >
            <option value="system">{t.languageSystem}</option>
            <option value="ja">{t.languageJapanese}</option>
            <option value="en">{t.languageEnglish}</option>
          </select>
        </label>
        <div className="eyebrow">{t.firstSetup}</div>
        <h1 id="onboarding-title">{t.onboardingTitle}</h1>
        <p className="onboarding-lead">{t.onboardingLead}</p>

        <ol className="setup-list">
          <li className={permissionGranted ? "setup-step is-complete" : "setup-step"}>
            <div className="setup-step__icon">
              {permissionGranted ? (
                <Check aria-hidden="true" size={19} />
              ) : (
                <Bell aria-hidden="true" size={19} />
              )}
            </div>
            <div className="setup-step__body">
              <strong>{view.preferences.notificationDelivery === "aizuBanner" ? t.notificationStyle : t.allowNotifications}</strong>
              <span>
                {permissionGranted
                  ? view.preferences.notificationDelivery === "aizuBanner"
                    ? t.aizuBannerReady
                    : t.notificationsReady
                  : permissionDenied
                    ? t.notificationsDenied
                    : t.notificationsExplicit}
              </span>
            </div>
            {!permissionGranted && !permissionDenied ? (
              <button
                className="button button--secondary"
                disabled={busy}
                onClick={() => void onRequestPermission()}
                type="button"
              >
                {t.allow}
              </button>
            ) : null}
          </li>

          <li className="setup-step">
            <div className="setup-step__icon">
              {agentsConfigured ? <Check aria-hidden="true" size={19} /> : <Wrench aria-hidden="true" size={19} />}
            </div>
            <div className="setup-step__body">
              <strong>{t.connectAgents}</strong>
              <span>{codexApprovalRequired ? t.hooksApproval : agentsConfigured ? t.hooksReady : t.hooksInstall}</span>
            </div>
            {codexApprovalRequired ? (
              <button className="button button--secondary" disabled={busy} onClick={() => void onConfirmCodexTrust()} type="button">
                {t.confirmApproval}
              </button>
            ) : !agentsConfigured ? (
              <button className="button button--secondary" disabled={busy} onClick={() => void onConfigureAgents()} type="button">
                {t.setUp}
              </button>
            ) : null}
          </li>

          <li className="setup-step">
            <div className="setup-step__icon">
              <Radio aria-hidden="true" size={19} />
            </div>
            <label className="setup-step__body" htmlFor="launch-at-login">
              <strong>{t.launchAtLogin}</strong>
              <span>{t.launchHelp}</span>
            </label>
            <button
              aria-checked={launchAtLogin}
              aria-label={t.launchAria}
              className="switch"
              id="launch-at-login"
              onClick={() => setLaunchAtLogin((enabled) => !enabled)}
              role="switch"
              type="button"
            >
              <span />
            </button>
          </li>

          <li className="setup-step is-passive">
            <div className="setup-step__icon">
              <LockKeyhole aria-hidden="true" size={19} />
            </div>
            <div className="setup-step__body">
              <strong>{t.privateDefault}</strong>
              <span>{t.privateHelp}</span>
            </div>
          </li>
        </ol>

        {permissionDenied ? (
          <div className="inline-alert" role="alert">
            {t.deniedHelp}
          </div>
        ) : null}

        <div className="onboarding-actions">
          <span>{t.localReady}</span>
          <button
            className="button button--primary"
            disabled={busy || !permissionGranted || !agentsConfigured}
            onClick={() => void onComplete(launchAtLogin)}
            type="button"
          >
            {t.openAizu}
            <ChevronRight aria-hidden="true" size={17} />
          </button>
        </div>
      </section>
    </main>
  );
}
