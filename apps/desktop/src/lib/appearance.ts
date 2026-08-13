import type { Preferences } from "./contracts";
import { resolveLanguage } from "./i18n";

export function applyDocumentPreferences(
  preferences: Pick<Preferences, "language" | "textSize">,
): void {
  document.documentElement.lang = resolveLanguage(preferences.language);
  document.documentElement.dataset.textSize = preferences.textSize;
}
