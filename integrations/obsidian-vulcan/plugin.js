"use strict";

// Bundled into main.js for Obsidian desktop and mobile.

const {
  Modal,
  Notice,
  Plugin,
  PluginSettingTab,
  Setting,
  requestUrl,
} = require("obsidian");
const { VulcanCompanionClient } = require("./protocol");
const {
  DEFAULT_SETTINGS,
  SaveSyncCoordinator,
  sanitizeSettings,
  statusPresentation,
} = require("./core");

const TOKEN_SECRET_ID = "vulcan-companion-token";
const EVENT_RECONNECT_MS = 5000;

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
      callback: () => void this.syncNow(),
    });
    this.addCommand({
      id: "show-status",
      name: "Show synchronization status",
      callback: () => void this.showStatus(),
    });
    this.addCommand({
      id: "review-conflicts",
      name: "Review synchronization conflicts",
      callback: () => void this.reviewConflicts(),
    });
    this.addCommand({
      id: "pause-sync",
      name: "Pause automatic synchronization",
      callback: () => void this.pauseSync(),
    });
    this.addCommand({
      id: "resume-sync",
      name: "Resume automatic synchronization",
      callback: () => void this.resumeSync(),
    });
    this.addSettingTab(new VulcanSettingTab(this.app, this));

    this.rebuildSaveCoordinator();
    this.registerEvent(this.app.vault.on("modify", () => {
      if (this.settings.syncOnSave) this.saveCoordinator.notifySave();
    }));
    this.registerInterval(window.setInterval(() => void this.refreshStatus(false), 30_000));
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
          throw: false,
        });
        return { status: response.status, json: response.json, text: response.text };
      },
      WebSocketCtor: globalThis.WebSocket,
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
      onError: (error) => this.handleError(error, false),
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
        onError: () => {},
        onClose: () => {
          this.eventSocket = null;
          this.scheduleEventReconnect();
        },
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

class VulcanSettingTab extends PluginSettingTab {
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
      text: "Only non-secret preferences are stored in plugin data. The bearer token uses Obsidian's device-local SecretStorage.",
    });

    new Setting(containerEl)
      .setName("Daemon endpoint")
      .setDesc("Loopback origin returned by `vulcan daemon companion --output json`.")
      .addText((text) => text
        .setPlaceholder(DEFAULT_SETTINGS.baseUrl)
        .setValue(this.plugin.settings.baseUrl)
        .onChange(async (value) => {
          this.plugin.settings.baseUrl = value.trim();
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName("Wiki ID")
      .setDesc("The device-local registered wiki ID to control.")
      .addText((text) => text
        .setPlaceholder("personal")
        .setValue(this.plugin.settings.wikiId)
        .onChange(async (value) => {
          this.plugin.settings.wikiId = value.trim();
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName("Companion token")
      .setDesc("Paste the token from `vulcan daemon companion --reveal-token --output json`.")
      .addText((text) => {
        text.inputEl.type = "password";
        text.setPlaceholder(this.app.secretStorage.getSecret(TOKEN_SECRET_ID) ? "Token stored" : "Paste token");
        text.onChange((value) => { this.pendingToken = value; });
      })
      .addButton((button) => button.setButtonText("Store on device").onClick(async () => {
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
      }))
      .addButton((button) => button.setButtonText("Clear").setWarning().onClick(() => {
        this.app.secretStorage.setSecret(TOKEN_SECRET_ID, "");
        this.pendingToken = "";
        this.plugin.closeEvents();
        new Notice("Vulcan token cleared");
        this.display();
      }));

    new Setting(containerEl)
      .setName("Synchronize after saves")
      .setDesc("Debounce completed vault writes and enqueue the daemon's ordinary finite sync transaction.")
      .addToggle((toggle) => toggle
        .setValue(this.plugin.settings.syncOnSave)
        .onChange(async (value) => {
          this.plugin.settings.syncOnSave = value;
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName("Save debounce (milliseconds)")
      .setDesc("Wait between 250 and 60000 ms after the last saved vault change.")
      .addText((text) => text
        .setValue(String(this.plugin.settings.saveDebounceMs))
        .onChange(async (value) => {
          this.plugin.settings.saveDebounceMs = Number(value);
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName("Live status events")
      .setDesc("Use the authenticated Vulcan WebSocket snapshot stream; periodic status refresh remains enabled.")
      .addToggle((toggle) => toggle
        .setValue(this.plugin.settings.eventStream)
        .onChange(async (value) => {
          this.plugin.settings.eventStream = value;
          await this.plugin.saveSettings();
        }));

    new Setting(containerEl)
      .setName("Test connection")
      .addButton((button) => button.setButtonText("Test").onClick(async () => {
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
}

class StatusModal extends Modal {
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

  onClose() { this.contentEl.empty(); }
}

class ConflictListModal extends Modal {
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
      new Setting(this.contentEl)
        .setName(conflict.paths.join(", "))
        .setDesc(`Conflict ${conflict.id}`)
        .addButton((button) => button.setButtonText("Review").onClick(async () => {
          try {
            const detailReport = await this.client.conflict(this.plugin.settings.wikiId, conflict.id);
            new ConflictDetailModal(this.app, this.plugin, this.client, detailReport).open();
          } catch (error) {
            this.plugin.handleError(error, true);
          }
        }));
    }
  }

  onClose() { this.contentEl.empty(); }
}

class ConflictDetailModal extends Modal {
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
      cls: "mod-warning",
    });
    for (const side of ["base", "local", "remote"]) {
      new Setting(this.contentEl)
        .setName(`Use ${side} side`)
        .addButton((button) => button.setButtonText("Preview").onClick(() => void this.preview(side)));
    }
  }

  async preview(side) {
    try {
      const record = this.report.record || this.report.conflict || this.report;
      const preview = await this.client.resolveConflict(
        this.plugin.settings.wikiId,
        record.id,
        side,
        true,
      );
      new ResolutionPreviewModal(this.app, this.plugin, this.client, record.id, side, preview).open();
    } catch (error) {
      this.plugin.handleError(error, true);
    }
  }

  onClose() { this.contentEl.empty(); }
}

class ResolutionPreviewModal extends Modal {
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
      cls: "vulcan-companion-preview",
    });
    new Setting(this.contentEl)
      .setName("Apply this exact reviewed side")
      .setDesc("Vulcan will rerun stale-input, lease, recovery, whole-tree, and worktree checks.")
      .addButton((button) => button
        .setButtonText("Apply resolution")
        .setWarning()
        .onClick(async () => {
          button.setDisabled(true);
          try {
            await this.client.resolveConflict(
              this.plugin.settings.wikiId,
              this.conflictId,
              this.side,
              false,
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

  onClose() { this.contentEl.empty(); }
}

function detail(container, name, value) {
  if (value === undefined || value === null) return;
  container.createEl("dt", { text: name });
  container.createEl("dd", { text: String(value) });
}

function jobSuffix(report) {
  const id = report && report.job && report.job.id;
  return id ? ` (${id})` : "";
}
