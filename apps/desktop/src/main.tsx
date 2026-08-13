import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import "./styles.css";

if (import.meta.env.VITE_DESKTOP_E2E === "1") {
  await import("@wdio/tauri-plugin");
  const internals = window.__TAURI_INTERNALS__;
  const invoke = internals?.invoke;
  if (invoke !== undefined) {
    window.__TAURI__ = {
      core: {
        invoke: (command, args) => invoke(command, args),
      },
    };
  }
}

const root = document.getElementById("root");

if (!root) {
  throw new Error("Aizu application root is missing");
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
