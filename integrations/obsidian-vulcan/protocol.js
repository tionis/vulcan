"use strict";

const PROTOCOL_VERSION = 1;

class VulcanProtocolError extends Error {
  constructor(kind, detail, status) {
    super(detail);
    this.name = "VulcanProtocolError";
    this.kind = kind;
    this.status = status;
  }
}

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
  return octets.length === 4
    && octets[0] === "127"
    && octets.every((octet) => /^\d{1,3}$/.test(octet) && Number(octet) <= 255);
}

function defaultIdempotencyKey() {
  if (globalThis.crypto && typeof globalThis.crypto.randomUUID === "function") {
    return `obsidian-${globalThis.crypto.randomUUID()}`;
  }
  throw new Error("secure random UUID generation is unavailable");
}

class VulcanCompanionClient {
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
    return this.request("POST", `/${segment(wiki)}/sync`, undefined, {
      "Idempotency-Key": this.idempotencyKey(),
    });
  }

  pause(wiki) {
    return this.request("POST", `/${segment(wiki)}/sync/pause`, {});
  }

  resume(wiki) {
    return this.request("POST", `/${segment(wiki)}/sync/resume`, {}, {
      "Idempotency-Key": this.idempotencyKey(),
    });
  }

  conflicts(wiki) {
    return this.request("GET", `/${segment(wiki)}/sync/conflicts`);
  }

  conflict(wiki, conflict) {
    return this.request(
      "GET",
      `/${segment(wiki)}/sync/conflicts/${segment(conflict)}`,
    );
  }

  resolveConflict(wiki, conflict, side, dryRun) {
    if (!["base", "local", "remote"].includes(side)) {
      throw new Error("conflict side must be base, local, or remote");
    }
    return this.request(
      "POST",
      `/${segment(wiki)}/sync/conflicts/${segment(conflict)}/resolve`,
      { side, dry_run: Boolean(dryRun) },
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
      `vulcan.bearer.${this.token}`,
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
      ...extraHeaders,
    };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    let response;
    try {
      response = await this.transport({
        url: `${this.baseUrl}${path}`,
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (error) {
      throw new VulcanProtocolError("offline", error.message || String(error), 0);
    }
    const status = Number(response.status);
    let value = response.json;
    if (value === undefined && response.text) {
      try {
        value = JSON.parse(response.text);
      } catch (_) {
        value = undefined;
      }
    }
    if (status < 200 || status >= 300) {
      throw new VulcanProtocolError(
        value && value.kind ? value.kind : "transport",
        value && value.detail ? value.detail : `Vulcan request failed with HTTP ${status}`,
        status,
      );
    }
    if (value === undefined) {
      throw new VulcanProtocolError("transport", "Vulcan returned no JSON body", status);
    }
    return value;
  }
}

function segment(value) {
  const text = String(value || "");
  if (!text) throw new Error("Vulcan resource identifier is required");
  return encodeURIComponent(text);
}

module.exports = {
  PROTOCOL_VERSION,
  VulcanCompanionClient,
  VulcanProtocolError,
  isLoopbackHost,
  normalizeBaseUrl,
};
