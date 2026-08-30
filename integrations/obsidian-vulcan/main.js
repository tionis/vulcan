"use strict";
var __getOwnPropNames = Object.getOwnPropertyNames;
var __commonJS = (cb, mod) => function __require() {
  try {
    return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
  } catch (e) {
    throw mod = 0, e;
  }
};

// protocol.js
var require_protocol = __commonJS({
  "protocol.js"(exports2, module2) {
    "use strict";
    var PROTOCOL_VERSION = 1;
    var VulcanProtocolError = class extends Error {
      constructor(kind, detail2, status) {
        super(detail2);
        this.name = "VulcanProtocolError";
        this.kind = kind;
        this.status = status;
      }
    };
    function normalizeBaseUrl(value) {
      let url;
      try {
        url = new URL(String(value).trim());
      } catch (_) {
        throw new Error("Vulcan endpoint must be an absolute loopback HTTP URL");
      }
      if (url.protocol !== "http:" || !isLoopbackHost(url.hostname)) {
        throw new Error("Vulcan endpoint must use HTTP on a loopback host");
      }
      if (url.username || url.password || url.pathname !== "/" || url.search || url.hash) {
        throw new Error("Vulcan endpoint must contain only a loopback origin and optional port");
      }
      return url.origin;
    }
    function isLoopbackHost(hostname) {
      if (hostname === "localhost" || hostname === "[::1]" || hostname === "::1") {
        return true;
      }
      const octets = hostname.split(".");
      return octets.length === 4 && octets[0] === "127" && octets.every((octet) => /^\d{1,3}$/.test(octet) && Number(octet) <= 255);
    }
    function defaultIdempotencyKey() {
      if (globalThis.crypto && typeof globalThis.crypto.randomUUID === "function") {
        return `obsidian-${globalThis.crypto.randomUUID()}`;
      }
      throw new Error("secure random UUID generation is unavailable");
    }
    var VulcanCompanionClient2 = class {
      constructor({ baseUrl, token, request, WebSocketCtor, idempotencyKey = defaultIdempotencyKey }) {
        this.baseUrl = normalizeBaseUrl(baseUrl);
        this.token = String(token || "");
        if (this.token.length < 40 || /[\x00-\x1f\x7f]/.test(this.token)) {
          throw new Error("Vulcan companion token is missing or malformed");
        }
        if (typeof request !== "function") {
          throw new Error("a companion HTTP transport is required");
        }
        this.transport = request;
        this.WebSocketCtor = WebSocketCtor;
        this.idempotencyKey = idempotencyKey;
      }
      capabilities() {
        return this.request("GET", "/capabilities");
      }
      listWikis(group) {
        const query = group ? `?group=${encodeURIComponent(group)}` : "";
        return this.request("GET", `/vaults${query}`);
      }
      status(wiki) {
        return this.request("GET", `/${segment(wiki)}/sync/status`);
      }
      sync(wiki) {
        return this.request("POST", `/${segment(wiki)}/sync`, void 0, {
          "Idempotency-Key": this.idempotencyKey()
        });
      }
      pause(wiki) {
        return this.request("POST", `/${segment(wiki)}/sync/pause`, {});
      }
      resume(wiki) {
        return this.request("POST", `/${segment(wiki)}/sync/resume`, {}, {
          "Idempotency-Key": this.idempotencyKey()
        });
      }
      conflicts(wiki) {
        return this.request("GET", `/${segment(wiki)}/sync/conflicts`);
      }
      conflict(wiki, conflict) {
        return this.request(
          "GET",
          `/${segment(wiki)}/sync/conflicts/${segment(conflict)}`
        );
      }
      resolveConflict(wiki, conflict, side, dryRun) {
        if (!["base", "local", "remote"].includes(side)) {
          throw new Error("conflict side must be base, local, or remote");
        }
        return this.request(
          "POST",
          `/${segment(wiki)}/sync/conflicts/${segment(conflict)}/resolve`,
          { side, dry_run: Boolean(dryRun) }
        );
      }
      connectEvents({ onSnapshot, onError, onClose }) {
        if (typeof this.WebSocketCtor !== "function") {
          throw new Error("WebSocket event transport is unavailable");
        }
        const url = new URL(this.baseUrl);
        url.protocol = "ws:";
        url.pathname = "/events";
        const socket = new this.WebSocketCtor(url.toString(), [
          "vulcan.v1",
          `vulcan.bearer.${this.token}`
        ]);
        socket.onmessage = (event) => {
          try {
            const value = JSON.parse(event.data);
            if (value.version !== PROTOCOL_VERSION || value.event !== "state_snapshot") {
              throw new Error("unsupported Vulcan event payload");
            }
            onSnapshot(value);
          } catch (error) {
            if (onError) onError(error);
          }
        };
        socket.onerror = (event) => {
          if (onError) onError(event);
        };
        socket.onclose = (event) => {
          if (onClose) onClose(event);
        };
        return socket;
      }
      async request(method, path, body, extraHeaders = {}) {
        const headers = {
          Authorization: `Bearer ${this.token}`,
          "Vulcan-Protocol-Version": String(PROTOCOL_VERSION),
          ...extraHeaders
        };
        if (body !== void 0) headers["Content-Type"] = "application/json";
        let response;
        try {
          response = await this.transport({
            url: `${this.baseUrl}${path}`,
            method,
            headers,
            body: body === void 0 ? void 0 : JSON.stringify(body)
          });
        } catch (error) {
          throw new VulcanProtocolError("offline", error.message || String(error), 0);
        }
        const status = Number(response.status);
        let value = response.json;
        if (value === void 0 && response.text) {
          try {
            value = JSON.parse(response.text);
          } catch (_) {
            value = void 0;
          }
        }
        if (status < 200 || status >= 300) {
          throw new VulcanProtocolError(
            value && value.kind ? value.kind : "transport",
            value && value.detail ? value.detail : `Vulcan request failed with HTTP ${status}`,
            status
          );
        }
        if (value === void 0) {
          throw new VulcanProtocolError("transport", "Vulcan returned no JSON body", status);
        }
        return value;
      }
    };
    function segment(value) {
      const text = String(value || "");
      if (!text) throw new Error("Vulcan resource identifier is required");
      return encodeURIComponent(text);
    }
    module2.exports = {
      PROTOCOL_VERSION,
      VulcanCompanionClient: VulcanCompanionClient2,
      VulcanProtocolError,
      isLoopbackHost,
      normalizeBaseUrl
    };
  }
});

// core.js
var require_core = __commonJS({
  "core.js"(exports2, module2) {
    "use strict";
    var DEFAULT_SETTINGS2 = Object.freeze({
      baseUrl: "http://127.0.0.1:3210",
      wikiId: "",
      syncOnSave: false,
      saveDebounceMs: 1500,
      eventStream: true
    });
    var BUSY_STATES = /* @__PURE__ */ new Set([
      "capture_pending",
      "capturing",
      "captured_unpushed",
      "fetching",
      "fetched",
      "merging",
      "pushing",
      "applying"
    ]);
    function sanitizeSettings2(value) {
      const input = value && typeof value === "object" ? value : {};
      const debounce = Number(input.saveDebounceMs);
      return {
        baseUrl: typeof input.baseUrl === "string" ? input.baseUrl : DEFAULT_SETTINGS2.baseUrl,
        wikiId: typeof input.wikiId === "string" ? input.wikiId : "",
        syncOnSave: input.syncOnSave === true,
        saveDebounceMs: Number.isFinite(debounce) ? Math.min(6e4, Math.max(250, Math.round(debounce))) : DEFAULT_SETTINGS2.saveDebounceMs,
        eventStream: input.eventStream !== false
      };
    }
    function statusPresentation2(status) {
      const state = status && typeof status.state === "string" ? status.state : "unknown";
      const conflicts = Number(status && status.unresolved_conflicts) || 0;
      const labels = {
        clean: "Vulcan: synced",
        dirty: "Vulcan: changes pending",
        paused: "Vulcan: paused",
        offline: "Vulcan: offline",
        conflicted: `Vulcan: ${conflicts || ""} conflict${conflicts === 1 ? "" : "s"}`.replace("  ", " "),
        error: "Vulcan: error",
        unknown: "Vulcan: not configured"
      };
      return {
        state,
        busy: BUSY_STATES.has(state),
        label: labels[state] || `Vulcan: ${state.replaceAll("_", " ")}`
      };
    }
    var SaveSyncCoordinator2 = class {
      constructor({ delayMs, status, sync, onError, setTimer = setTimeout, clearTimer = clearTimeout }) {
        this.delayMs = delayMs;
        this.status = status;
        this.sync = sync;
        this.onError = onError || (() => {
        });
        this.setTimer = setTimer;
        this.clearTimer = clearTimer;
        this.timer = null;
        this.running = false;
        this.pending = false;
        this.disposed = false;
      }
      notifySave() {
        if (this.disposed) return;
        this.pending = true;
        if (this.timer !== null) this.clearTimer(this.timer);
        this.timer = this.setTimer(() => {
          this.timer = null;
          void this.flush();
        }, this.delayMs);
      }
      async flush() {
        if (this.disposed || this.running || !this.pending) return false;
        this.pending = false;
        this.running = true;
        try {
          const status = await this.status();
          const state = statusPresentation2(status).state;
          if (BUSY_STATES.has(state) || state === "paused" || state === "conflicted") {
            return false;
          }
          await this.sync();
          return true;
        } catch (error) {
          this.onError(error);
          return false;
        } finally {
          this.running = false;
          if (this.pending && !this.disposed) this.notifySave();
        }
      }
      dispose() {
        this.disposed = true;
        this.pending = false;
        if (this.timer !== null) this.clearTimer(this.timer);
        this.timer = null;
      }
    };
    module2.exports = {
      BUSY_STATES,
      DEFAULT_SETTINGS: DEFAULT_SETTINGS2,
      SaveSyncCoordinator: SaveSyncCoordinator2,
      sanitizeSettings: sanitizeSettings2,
      statusPresentation: statusPresentation2
    };
  }
});

// plugin.js
var {
  Modal,
  Notice,
  Plugin,
  PluginSettingTab,
  Setting,
  requestUrl
} = require("obsidian");
var { VulcanCompanionClient } = require_protocol();
var {
  DEFAULT_SETTINGS,
  SaveSyncCoordinator,
  sanitizeSettings,
  statusPresentation
} = require_core();
var TOKEN_SECRET_ID = "vulcan-companion-token";
var EVENT_RECONNECT_MS = 5e3;
module.exports = class VulcanCompanionPlugin extends Plugin {
  async onload() {
    this.settings = sanitizeSettings(await this.loadData());
    this.status = null;
    this.eventSocket = null;
    this.eventReconnectTimer = null;
    this.statusEl = this.addStatusBarItem();
    this.statusEl.addClass("vulcan-companion-status");
    this.statusEl.onClickEvent(() => void this.showStatus());
    this.renderStatus();
    this.addRibbonIcon("refresh-cw", "Synchronize with Vulcan", () => void this.syncNow());
    this.addCommand({
      id: "sync-now",
      name: "Synchronize now",
      callback: () => void this.syncNow()
    });
    this.addCommand({
      id: "show-status",
      name: "Show synchronization status",
      callback: () => void this.showStatus()
    });
    this.addCommand({
      id: "review-conflicts",
      name: "Review synchronization conflicts",
      callback: () => void this.reviewConflicts()
    });
    this.addCommand({
      id: "pause-sync",
      name: "Pause automatic synchronization",
      callback: () => void this.pauseSync()
    });
    this.addCommand({
      id: "resume-sync",
      name: "Resume automatic synchronization",
      callback: () => void this.resumeSync()
    });
    this.addSettingTab(new VulcanSettingTab(this.app, this));
    this.rebuildSaveCoordinator();
    this.registerEvent(this.app.vault.on("modify", () => {
      if (this.settings.syncOnSave) this.saveCoordinator.notifySave();
    }));
    this.registerInterval(window.setInterval(() => void this.refreshStatus(false), 3e4));
    this.app.workspace.onLayoutReady(() => {
      void this.refreshStatus(false);
      this.reconnectEvents();
    });
  }
  onunload() {
    if (this.saveCoordinator) this.saveCoordinator.dispose();
    this.closeEvents();
  }
  async saveSettings() {
    this.settings = sanitizeSettings(this.settings);
    await this.saveData(this.settings);
    this.rebuildSaveCoordinator();
    this.closeEvents();
    this.reconnectEvents();
    await this.refreshStatus(false);
  }
  async client() {
    const token = this.app.secretStorage.getSecret(TOKEN_SECRET_ID);
    if (!token) {
      throw new Error("Configure the device-local Vulcan companion token first");
    }
    if (!this.settings.wikiId) {
      throw new Error("Configure the registered Vulcan wiki ID first");
    }
    return new VulcanCompanionClient({
      baseUrl: this.settings.baseUrl,
      token,
      request: async (request) => {
        const response = await requestUrl({
          url: request.url,
          method: request.method,
          headers: request.headers,
          body: request.body,
          throw: false
        });
        return { status: response.status, json: response.json, text: response.text };
      },
      WebSocketCtor: globalThis.WebSocket
    });
  }
  rebuildSaveCoordinator() {
    if (this.saveCoordinator) this.saveCoordinator.dispose();
    this.saveCoordinator = new SaveSyncCoordinator({
      delayMs: this.settings.saveDebounceMs,
      status: async () => {
        const client = await this.client();
        return client.status(this.settings.wikiId);
      },
      sync: async () => {
        const client = await this.client();
        await client.sync(this.settings.wikiId);
        await this.refreshStatus(false);
      },
      onError: (error) => this.handleError(error, false)
    });
  }
  async requestEditorSave() {
    this.app.commands.executeCommandById("editor:save-file");
    await new Promise((resolve) => window.setTimeout(resolve, 75));
  }
  async syncNow() {
    try {
      await this.requestEditorSave();
      const client = await this.client();
      const report = await client.sync(this.settings.wikiId);
      new Notice(`Vulcan synchronization queued${jobSuffix(report)}`);
      await this.refreshStatus(false);
    } catch (error) {
      this.handleError(error, true);
    }
  }
  async pauseSync() {
    try {
      const client = await this.client();
      await client.pause(this.settings.wikiId);
      new Notice("Vulcan automatic synchronization paused");
      await this.refreshStatus(false);
    } catch (error) {
      this.handleError(error, true);
    }
  }
  async resumeSync() {
    try {
      const client = await this.client();
      const report = await client.resume(this.settings.wikiId);
      new Notice(`Vulcan synchronization resumed${jobSuffix(report)}`);
      await this.refreshStatus(false);
    } catch (error) {
      this.handleError(error, true);
    }
  }
  async refreshStatus(notify) {
    try {
      const client = await this.client();
      this.status = await client.status(this.settings.wikiId);
      this.renderStatus();
      if (notify) new StatusModal(this.app, this.status).open();
      return this.status;
    } catch (error) {
      this.status = { state: "offline", detail: error.message || String(error) };
      this.renderStatus();
      if (notify) this.handleError(error, true);
      return null;
    }
  }
  async showStatus() {
    await this.refreshStatus(true);
  }
  async reviewConflicts() {
    try {
      const client = await this.client();
      const report = await client.conflicts(this.settings.wikiId);
      new ConflictListModal(this.app, this, client, report).open();
    } catch (error) {
      this.handleError(error, true);
    }
  }
  renderStatus() {
    const presentation = statusPresentation(this.status);
    this.statusEl.setText(presentation.label);
    this.statusEl.setAttr("data-state", presentation.state);
    this.statusEl.setAttr("aria-label", "Open Vulcan synchronization status");
  }
  reconnectEvents() {
    if (!this.settings.eventStream || this.eventSocket || this.eventReconnectTimer) return;
    void this.client().then((client) => {
      if (!this.settings.eventStream || this.eventSocket) return;
      this.eventSocket = client.connectEvents({
        onSnapshot: (snapshot) => {
          const status = snapshot.statuses.find((entry) => entry.wiki_id === this.settings.wikiId);
          if (status) {
            this.status = status;
            this.renderStatus();
          }
        },
        onError: () => {
        },
        onClose: () => {
          this.eventSocket = null;
          this.scheduleEventReconnect();
        }
      });
    }).catch(() => this.scheduleEventReconnect());
  }
  scheduleEventReconnect() {
    if (!this.settings.eventStream || this.eventReconnectTimer) return;
    this.eventReconnectTimer = window.setTimeout(() => {
      this.eventReconnectTimer = null;
      this.reconnectEvents();
    }, EVENT_RECONNECT_MS);
  }
  closeEvents() {
    if (this.eventReconnectTimer) window.clearTimeout(this.eventReconnectTimer);
    this.eventReconnectTimer = null;
    const socket = this.eventSocket;
    this.eventSocket = null;
    if (socket) {
      socket.onclose = null;
      socket.close();
    }
  }
  handleError(error, notify) {
    console.error("Vulcan companion operation failed", error);
    if (notify) new Notice(`Vulcan: ${error.message || String(error)}`);
  }
};
var VulcanSettingTab = class extends PluginSettingTab {
  constructor(app, plugin) {
    super(app, plugin);
    this.plugin = plugin;
    this.pendingToken = "";
  }
  display() {
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl("h2", { text: "Vulcan companion" });
    containerEl.createEl("p", {
      text: "Only non-secret preferences are stored in plugin data. The bearer token uses Obsidian's device-local SecretStorage."
    });
    new Setting(containerEl).setName("Daemon endpoint").setDesc("Loopback origin returned by `vulcan daemon companion --output json`.").addText((text) => text.setPlaceholder(DEFAULT_SETTINGS.baseUrl).setValue(this.plugin.settings.baseUrl).onChange(async (value) => {
      this.plugin.settings.baseUrl = value.trim();
      await this.plugin.saveSettings();
    }));
    new Setting(containerEl).setName("Wiki ID").setDesc("The device-local registered wiki ID to control.").addText((text) => text.setPlaceholder("personal").setValue(this.plugin.settings.wikiId).onChange(async (value) => {
      this.plugin.settings.wikiId = value.trim();
      await this.plugin.saveSettings();
    }));
    new Setting(containerEl).setName("Companion token").setDesc("Paste the token from `vulcan daemon companion --reveal-token --output json`.").addText((text) => {
      text.inputEl.type = "password";
      text.setPlaceholder(this.app.secretStorage.getSecret(TOKEN_SECRET_ID) ? "Token stored" : "Paste token");
      text.onChange((value) => {
        this.pendingToken = value;
      });
    }).addButton((button) => button.setButtonText("Store on device").onClick(async () => {
      if (!this.pendingToken) {
        new Notice("Paste a token before storing it");
        return;
      }
      this.app.secretStorage.setSecret(TOKEN_SECRET_ID, this.pendingToken.trim());
      this.pendingToken = "";
      new Notice("Vulcan token stored in Obsidian SecretStorage");
      this.plugin.closeEvents();
      this.plugin.reconnectEvents();
      this.display();
    })).addButton((button) => button.setButtonText("Clear").setWarning().onClick(() => {
      this.app.secretStorage.setSecret(TOKEN_SECRET_ID, "");
      this.pendingToken = "";
      this.plugin.closeEvents();
      new Notice("Vulcan token cleared");
      this.display();
    }));
    new Setting(containerEl).setName("Synchronize after saves").setDesc("Debounce completed vault writes and enqueue the daemon's ordinary finite sync transaction.").addToggle((toggle) => toggle.setValue(this.plugin.settings.syncOnSave).onChange(async (value) => {
      this.plugin.settings.syncOnSave = value;
      await this.plugin.saveSettings();
    }));
    new Setting(containerEl).setName("Save debounce (milliseconds)").setDesc("Wait between 250 and 60000 ms after the last saved vault change.").addText((text) => text.setValue(String(this.plugin.settings.saveDebounceMs)).onChange(async (value) => {
      this.plugin.settings.saveDebounceMs = Number(value);
      await this.plugin.saveSettings();
    }));
    new Setting(containerEl).setName("Live status events").setDesc("Use the authenticated Vulcan WebSocket snapshot stream; periodic status refresh remains enabled.").addToggle((toggle) => toggle.setValue(this.plugin.settings.eventStream).onChange(async (value) => {
      this.plugin.settings.eventStream = value;
      await this.plugin.saveSettings();
    }));
    new Setting(containerEl).setName("Test connection").addButton((button) => button.setButtonText("Test").onClick(async () => {
      try {
        const client = await this.plugin.client();
        const capabilities = await client.capabilities();
        if (capabilities.version !== 1) throw new Error("unsupported companion protocol version");
        await this.plugin.refreshStatus(false);
        new Notice("Connected to the Vulcan daemon");
      } catch (error) {
        this.plugin.handleError(error, true);
      }
    }));
  }
};
var StatusModal = class extends Modal {
  constructor(app, status) {
    super(app);
    this.status = status;
  }
  onOpen() {
    this.contentEl.createEl("h2", { text: "Vulcan synchronization" });
    const presentation = statusPresentation(this.status);
    this.contentEl.createEl("p", { text: presentation.label });
    const details = this.contentEl.createEl("dl", { cls: "vulcan-companion-details" });
    detail(details, "Wiki", this.status.wiki_id);
    detail(details, "Backend", this.status.backend);
    detail(details, "Source", this.status.source);
    detail(details, "Conflicts", String(this.status.unresolved_conflicts || 0));
    if (this.status.detail) detail(details, "Detail", this.status.detail);
  }
  onClose() {
    this.contentEl.empty();
  }
};
var ConflictListModal = class extends Modal {
  constructor(app, plugin, client, report) {
    super(app);
    this.plugin = plugin;
    this.client = client;
    this.report = report;
  }
  onOpen() {
    this.contentEl.createEl("h2", { text: "Vulcan conflicts" });
    const conflicts = Array.isArray(this.report.conflicts) ? this.report.conflicts : [];
    if (conflicts.length === 0) {
      this.contentEl.createEl("p", { text: "No unresolved conflicts." });
      return;
    }
    for (const conflict of conflicts) {
      new Setting(this.contentEl).setName(conflict.paths.join(", ")).setDesc(`Conflict ${conflict.id}`).addButton((button) => button.setButtonText("Review").onClick(async () => {
        try {
          const detailReport = await this.client.conflict(this.plugin.settings.wikiId, conflict.id);
          new ConflictDetailModal(this.app, this.plugin, this.client, detailReport).open();
        } catch (error) {
          this.plugin.handleError(error, true);
        }
      }));
    }
  }
  onClose() {
    this.contentEl.empty();
  }
};
var ConflictDetailModal = class extends Modal {
  constructor(app, plugin, client, report) {
    super(app);
    this.plugin = plugin;
    this.client = client;
    this.report = report;
  }
  onOpen() {
    this.contentEl.createEl("h2", { text: "Review preserved conflict" });
    const record = this.report.record || this.report.conflict || this.report;
    this.contentEl.createEl("p", { text: `Conflict ${record.id}` });
    const paths = Array.isArray(record.paths) ? record.paths : [];
    const list = this.contentEl.createEl("ul");
    for (const entry of paths) {
      list.createEl("li", { text: typeof entry === "string" ? entry : entry.path });
    }
    this.contentEl.createEl("p", {
      text: "Choosing a preserved side is potentially lossy. Preview is mandatory; applying requires a second explicit action.",
      cls: "mod-warning"
    });
    for (const side of ["base", "local", "remote"]) {
      new Setting(this.contentEl).setName(`Use ${side} side`).addButton((button) => button.setButtonText("Preview").onClick(() => void this.preview(side)));
    }
  }
  async preview(side) {
    try {
      const record = this.report.record || this.report.conflict || this.report;
      const preview = await this.client.resolveConflict(
        this.plugin.settings.wikiId,
        record.id,
        side,
        true
      );
      new ResolutionPreviewModal(this.app, this.plugin, this.client, record.id, side, preview).open();
    } catch (error) {
      this.plugin.handleError(error, true);
    }
  }
  onClose() {
    this.contentEl.empty();
  }
};
var ResolutionPreviewModal = class extends Modal {
  constructor(app, plugin, client, conflictId, side, preview) {
    super(app);
    this.plugin = plugin;
    this.client = client;
    this.conflictId = conflictId;
    this.side = side;
    this.preview = preview;
  }
  onOpen() {
    this.contentEl.createEl("h2", { text: `Resolution preview: ${this.side}` });
    this.contentEl.createEl("pre", {
      text: JSON.stringify(this.preview, null, 2),
      cls: "vulcan-companion-preview"
    });
    new Setting(this.contentEl).setName("Apply this exact reviewed side").setDesc("Vulcan will rerun stale-input, lease, recovery, whole-tree, and worktree checks.").addButton((button) => button.setButtonText("Apply resolution").setWarning().onClick(async () => {
      button.setDisabled(true);
      try {
        await this.client.resolveConflict(
          this.plugin.settings.wikiId,
          this.conflictId,
          this.side,
          false
        );
        new Notice("Vulcan conflict resolution applied");
        this.close();
        await this.plugin.refreshStatus(false);
      } catch (error) {
        button.setDisabled(false);
        this.plugin.handleError(error, true);
      }
    }));
  }
  onClose() {
    this.contentEl.empty();
  }
};
function detail(container, name, value) {
  if (value === void 0 || value === null) return;
  container.createEl("dt", { text: name });
  container.createEl("dd", { text: String(value) });
}
function jobSuffix(report) {
  const id = report && report.job && report.job.id;
  return id ? ` (${id})` : "";
}
