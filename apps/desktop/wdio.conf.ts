import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import "@wdio/tauri-service";
import type { TauriCapabilities } from "@wdio/tauri-service";

const desktopDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(desktopDirectory, "../..");
const e2eStateRoot = process.env.AIZU_STATE_DIR ?? path.join(
  os.tmpdir(),
  `aizu-desktop-e2e-${String(process.pid)}`,
);
process.env.AIZU_STATE_DIR = e2eStateRoot;
process.env.XDG_CONFIG_HOME = path.join(e2eStateRoot, "config");
const capability: TauriCapabilities & { browserName: "tauri" } = {
  browserName: "tauri",
  "tauri:options": {
    application: path.join(repositoryRoot, "target/debug/aizu-desktop"),
  },
};

export const config: WebdriverIO.Config = {
  runner: "local",
  specs: [path.join(repositoryRoot, "tests/e2e/**/*.spec.ts")],
  maxInstances: 1,
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded",
    embeddedPort: 4445,
    startTimeout: 60_000,
    statusPollTimeout: 5_000,
    captureBackendLogs: false,
    captureFrontendLogs: false,
    logLevel: "warn",
  }]],
  capabilities: [capability],
  logLevel: "warn",
  waitforTimeout: 10_000,
  connectionRetryTimeout: 90_000,
  connectionRetryCount: 1,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    ui: "bdd",
    timeout: 60_000,
  },
};
