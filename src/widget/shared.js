// Shared host plumbing and card rendering for the Tandoor MCP App widgets.
// Inlined into each widget's <script> by the server (see widget_html in
// server.rs) at the `@include shared.js` marker, ahead of the widget code.
//
// A widget calls createGallery(config) and supplies:
//   toolName, appName, appVersion  identify the widget to the host
//   copy                           status texts shown while loading/failing
//   fetchInput(toolInput)          tool arguments for a direct fetch, or null
//   refreshInput(data)             tool arguments to refresh expired URLs
//   render(result)                 builds the DOM; returns the merged data
// and gets back the helpers listed at the bottom of createGallery.
const MCP_APPS_PROTOCOL_VERSION = "2026-01-26";

function createGallery(config) {
  let rpcId = 0;
  const initializeIds = new Set();
  const pendingToolCalls = new Map();
  let initialized = false;
  let current = null;
  let toolInput = null;
  let fetching = false;
  let fetchAttempts = 0;
  let displayModeDecided = false;

  // --- host plumbing ---------------------------------------------------
  function send(method, params) {
    const id = ++rpcId;
    window.parent.postMessage({ jsonrpc: "2.0", id, method, params }, "*");
    return id;
  }
  // Ask the host to show the gallery inline. ChatGPT opens widgets that
  // declare `fullscreen` in its side pane; requesting `inline` keeps the
  // gallery in the chat while the user can still expand it. Only the first
  // display mode the host reports is considered, so a later manual expand
  // (which arrives as another mode change) is never undone. The host may
  // refuse the request.
  function preferInline(mode) {
    if (displayModeDecided || typeof mode !== "string") return;
    displayModeDecided = true;
    if (mode === "inline") return;
    send("ui/request-display-mode", { mode: "inline" });
    const request = window.openai?.requestDisplayMode;
    if (typeof request !== "function") return;
    try { Promise.resolve(request.call(window.openai, { mode: "inline" })).catch(() => {}); } catch (_) {}
  }
  // Report the content height through every channel the host may honour.
  // ChatGPT's bridge has been observed to ignore the MCP Apps size-changed
  // notification and to expose `notifyIntrinsicWidth` (not `...Height`);
  // without that call the inline iframe stayed 0x0 until a window resize.
  // The value is clamped to the host's maximum so it is never rejected.
  function notifyHeight() {
    const content = Math.ceil(document.documentElement.scrollHeight);
    const maxHeight = window.openai?.maxHeight || 0;
    const height = maxHeight ? Math.min(content, maxHeight) : content;
    window.parent.postMessage({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height } }, "*");
    for (const name of ["notifyIntrinsicHeight", "notifyIntrinsicWidth"]) {
      const report = window.openai?.[name];
      if (typeof report !== "function") continue;
      try { Promise.resolve(report.call(window.openai, height)).catch(() => {}); } catch (_) {}
    }
  }
  // Loading-state copy doubles as a diagnostic: it names the phase the
  // widget is stuck in if the host never hands over the tool result.
  function setStatus(text) {
    const status = document.getElementById("status");
    status.hidden = false;
    status.textContent = text;
    notifyHeight();
  }
  function applyTheme(theme) {
    if (theme === "dark" || theme === "light") document.documentElement.style.colorScheme = theme;
  }
  // Call a server tool through the host: ChatGPT's bridge when present,
  // otherwise the MCP Apps `tools/call` request forwarded by the host.
  function callTool(name, args) {
    const call = window.openai?.callTool
      ? window.openai.callTool(name, args)
      : new Promise((resolve, reject) => {
          const id = send("tools/call", { name, arguments: args });
          pendingToolCalls.set(id, { resolve, reject });
        });
    return Promise.race([
      call,
      new Promise((_, reject) => setTimeout(() => reject(new Error(`${name} timed out`)), 20000))
    ]);
  }
  // Hand a tool result to the widget's render() and remember what it showed.
  function deliver(result) {
    const data = config.render(result);
    if (data) current = data;
    refreshExpired();
  }
  // Last resort when the host never delivers the tool result (observed in
  // ChatGPT): fetch the gallery directly with the original tool arguments.
  async function fetchDirectly() {
    if (current || fetching || fetchAttempts >= 2) return;
    if (!window.openai?.callTool && !initialized) {
      setStatus(config.copy.cannotConnect);
      return;
    }
    const args = config.fetchInput(toolInput ?? window.openai?.toolInput);
    if (!args) {
      setStatus(config.copy.failed);
      return;
    }
    fetching = true;
    fetchAttempts += 1;
    setStatus(config.copy.loading);
    try {
      deliver(await callTool(config.toolName, args));
      if (!current) setStatus(config.copy.failed);
    } catch (error) {
      console.error("Direct gallery fetch failed", error);
      if (!current) setStatus(fetchAttempts < 2 ? "Loading is taking longer than expected, retrying…" : config.copy.failed);
    } finally {
      fetching = false;
      if (!current && fetchAttempts < 2) setTimeout(fetchDirectly, 1500);
    }
  }
  // Presigned thumbnail URLs expire; re-run the tool to get fresh ones.
  async function refreshExpired() {
    if (!current?.urls_expire_at || Date.now() < current.urls_expire_at * 1000 || fetching) return;
    fetching = true;
    try {
      const data = config.render(await callTool(config.toolName, config.refreshInput(current)));
      if (data) current = data;
    } catch (error) {
      console.error("Thumbnail refresh failed", error);
    } finally {
      fetching = false;
    }
  }
  function renderOpenAiGlobals(globals = window.openai) {
    const toolOutput = globals?.toolOutput ?? window.openai?.toolOutput;
    if (!toolOutput) return;
    deliver({
      structuredContent: toolOutput,
      toolResponseMetadata: globals?.toolResponseMetadata ?? window.openai?.toolResponseMetadata
    });
  }
  function initialize() {
    if (initialized) return;
    initializeIds.add(send("ui/initialize", {
      protocolVersion: MCP_APPS_PROTOCOL_VERSION,
      appInfo: { name: config.appName, version: config.appVersion },
      appCapabilities: { availableDisplayModes: ["inline", "fullscreen"] }
    }));
  }
  function onMessage(event) {
    if (event.source !== window.parent || event.data?.jsonrpc !== "2.0") return;
    const message = event.data;
    if (initializeIds.has(message.id) && message.result && !initialized) {
      initialized = true;
      window.parent.postMessage({ jsonrpc: "2.0", method: "ui/notifications/initialized" }, "*");
      applyTheme(message.result.hostContext?.theme);
      preferInline(message.result.hostContext?.displayMode ?? window.openai?.displayMode);
      if (!current) setStatus(config.copy.waiting);
      renderOpenAiGlobals();
      setTimeout(fetchDirectly, 2000);
    }
    if (initializeIds.has(message.id) && message.error) {
      console.error("MCP Apps initialization failed", message.error);
      setStatus(config.copy.retrying);
    }
    const pending = pendingToolCalls.get(message.id);
    if (pending) {
      pendingToolCalls.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message || "tool call failed"));
      else pending.resolve(message.result);
    }
    if (message.method === "ui/notifications/tool-input" && message.params?.arguments) {
      toolInput = message.params.arguments;
    }
    if (message.method === "ui/notifications/host-context-changed") applyTheme(message.params?.theme);
    if (message.method === "ui/notifications/tool-result") deliver(message.params);
  }
  function onSetGlobals(event) {
    const globals = event.detail?.globals;
    if (globals?.toolInput) toolInput = globals.toolInput;
    if (globals?.theme) applyTheme(globals.theme);
    preferInline(globals?.displayMode);
    if (!globals || globals.toolOutput !== undefined || globals.toolResponseMetadata !== undefined) {
      renderOpenAiGlobals(globals);
    }
  }
  // Wire the host events and kick off the (deliberately redundant) startup
  // paths: ChatGPT globals, the MCP Apps handshake, and a direct fetch.
  function start() {
    window.addEventListener("message", onMessage);
    window.addEventListener("openai:set_globals", onSetGlobals, { passive: true });
    setStatus(window.openai ? config.copy.waiting : "Connecting to ChatGPT…");
    applyTheme(window.openai?.theme);
    renderOpenAiGlobals();
    for (const delay of [250, 1000, 3000]) setTimeout(renderOpenAiGlobals, delay);
    for (const delay of [0, 500, 1500]) setTimeout(initialize, delay);
    setTimeout(fetchDirectly, window.openai?.callTool ? 3500 : 5000);
    setInterval(refreshExpired, 60000);
  }

  // --- rendering helpers -----------------------------------------------
  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = text;
    return node;
  }
  // Stable per-keyword colour so the same tag looks alike on every card.
  function hue(text) {
    let value = 0;
    for (const char of text.toLowerCase()) value = (value * 31 + char.codePointAt(0)) % 360;
    return value;
  }
  function formatMinutes(minutes) {
    const total = Number(minutes) || 0;
    const hours = Math.floor(total / 60);
    const rest = total % 60;
    if (!hours) return `${rest} min`;
    return rest ? `${hours} h ${rest} min` : `${hours} h`;
  }
  // Private thumbnail data travels in `_meta["tandoor/gallery"]`; hosts wrap
  // the tool result differently, so look in every known place.
  function galleryMetadata(source) {
    const candidates = [
      source?._meta,
      source?.mcp_tool_result?._meta,
      source?.call_tool_result?._meta,
      source?.toolResponseMetadata?._meta,
      source?.toolResponseMetadata?.mcp_tool_result?._meta,
      source?.toolResponseMetadata?.call_tool_result?._meta
    ];
    return candidates.find(candidate => candidate?.["tandoor/gallery"])?.["tandoor/gallery"];
  }
  // Join the model-visible items with their private thumbnail URLs by `key`.
  // Returns null when the result is not a gallery payload.
  function mergeGalleryData(result, key) {
    const structured = result?.structuredContent || result;
    if (!Array.isArray(structured?.items)) return null;
    const privateData = galleryMetadata(result);
    const privateItems = new Map((privateData?.items || []).map(item => [item[key], item]));
    return {
      ...structured,
      urls_expire_at: privateData?.urls_expire_at || 0,
      items: structured.items.map(item => ({ ...item, ...privateItems.get(item[key]) }))
    };
  }
  function openRecipe(event, url) {
    if (!window.openai?.openExternal) return;
    event.preventDefault();
    window.openai.openExternal({ href: url });
  }
  function photo(item) {
    const media = element("div", "media");
    if (item.image_status !== "attached" || !item.thumbnail_url) {
      media.append(element("div", "image placeholder", item.image_status === "not_set" ? "No photo" : "Photo unavailable"));
      return media;
    }
    const image = element("img", "image");
    image.src = item.thumbnail_url;
    image.alt = `Photo of ${item.display_name ?? item.name}`;
    image.loading = "lazy";
    image.referrerPolicy = "no-referrer";
    image.addEventListener("load", notifyHeight, { once: true });
    image.addEventListener("error", () => {
      image.replaceWith(element("div", "image placeholder", "Photo unavailable"));
      notifyHeight();
    }, { once: true });
    media.append(image);
    return media;
  }
  function nameLink(item) {
    const name = item.display_name ?? item.name;
    if (!item.recipe_url) return element("span", "name", name);
    const link = element("a", "name", name);
    link.href = item.recipe_url;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.addEventListener("click", event => openRecipe(event, item.recipe_url));
    return link;
  }
  // Keyword tags in server order (source keyword first); `limit` collapses
  // the rest into a "+N" pill.
  function tags(keywords, limit = Infinity) {
    if (!Array.isArray(keywords) || !keywords.length) return null;
    const list = element("div", "tags");
    const visible = keywords.slice(0, limit);
    for (const keyword of visible) {
      const tag = element("span", "tag", keyword);
      tag.style.setProperty("--hue", hue(keyword));
      list.append(tag);
    }
    if (keywords.length > visible.length) list.append(element("span", "tag more", `+${keywords.length - visible.length}`));
    return list;
  }
  function metaRow(item) {
    const facts = [];
    const servings = Number(item.servings);
    if (servings) facts.push([String(servings), servings === 1 ? "serving" : "servings"]);
    if (item.working_time) facts.push([formatMinutes(item.working_time), "active"]);
    if (item.waiting_time) facts.push([formatMinutes(item.waiting_time), "waiting"]);
    if (!item.working_time && !item.waiting_time && item.total_time) facts.push([formatMinutes(item.total_time), "total"]);
    if (!facts.length) return null;
    const row = element("p", "meta");
    facts.forEach(([value, label], index) => {
      if (index) row.append(element("span", "dot", "·"));
      const fact = element("span");
      fact.append(element("strong", null, value), ` ${label}`);
      row.append(fact);
    });
    return row;
  }

  return { start, notifyHeight, setStatus, element, formatMinutes, mergeGalleryData, photo, nameLink, tags, metaRow };
}
