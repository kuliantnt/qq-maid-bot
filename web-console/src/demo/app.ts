import { icons } from "./icons.js";
import { routeStatuses, themeById, themes, type DemoTheme, type DemoView } from "./data.js";

const demoRoot = document.querySelector<HTMLElement>("#demo-root");
if (!demoRoot) throw new Error("Demo root is missing");
const root: HTMLElement = demoRoot;

let currentTheme: DemoTheme = "night-shift";
let currentView: DemoView = "signal";

const viewMeta: Readonly<Record<DemoView, { readonly label: string; readonly kicker: string }>> = {
  signal: { label: "Signal", kicker: "LIVE OVERVIEW" },
  routes: { label: "Routes", kicker: "CHANNEL HEALTH" },
  settings: { label: "Settings", kicker: "SURFACE CONTROL" },
  archive: { label: "Archive", kicker: "RECENT EVENTS" },
};

function render(): void {
  const theme = themeById(currentTheme);
  root.dataset.theme = theme.id;
  root.innerHTML = `
    <div class="demo-atmosphere" aria-hidden="true"></div>
    <header class="demo-topbar">
      <a class="demo-brand" href="#signal" data-view="signal" aria-label="Return to signal overview">
        <span class="demo-brand-mark">${icons.spark}</span>
        <span><strong>MAID / SIGNAL</strong><small>visual approval console</small></span>
      </a>
      <div class="demo-topbar-right">
        <div class="demo-topbar-meta">
          <span class="demo-live"><i></i> local simulation</span>
          <span class="demo-time">09:41:26 <small>UTC+08</small></span>
        </div>
        <button class="demo-avatar" type="button" aria-label="Demo operator profile">MK</button>
      </div>
    </header>
    <main class="demo-shell">
      <section class="demo-hero" aria-labelledby="demo-page-title">
        <div>
          <p class="demo-overline">${viewMeta[currentView].kicker} <span>/ 03 OCT 2026</span></p>
          <h1 id="demo-page-title">${viewMeta[currentView].label}<br><em>in a clear state.</em></h1>
          <p class="demo-lede">A local-first command surface for watching the quiet signals behind every conversation.</p>
        </div>
        <div class="demo-hero-side">
          <span class="demo-hero-icon">${icons.signal}</span>
          <span class="demo-mono">SYSTEM / NOMINAL</span>
          <strong>99.98<span>%</span></strong>
          <small>signal confidence</small>
        </div>
      </section>
      ${renderView()}
    </main>
    <aside class="demo-theme-switcher" aria-label="Theme presets">
      <p class="demo-overline">COLOR PRESETS</p>
      <div class="demo-theme-options">${themes.map((option) => `
        <button class="demo-theme-button" type="button" data-theme-choice="${option.id}" aria-pressed="${option.id === currentTheme}">
          <span class="demo-theme-swatch" style="--swatch-dark:${option.dark};--swatch-light:${option.light};--swatch-contrast:${option.contrast}"></span>
          <span><strong>${option.name}</strong><small>${option.note}</small></span>
        </button>`).join("")}</div>
    </aside>
    <nav class="demo-bottom-nav" aria-label="Demo views">
      ${Object.entries(viewMeta).map(([id, meta]) => `<button type="button" data-view="${id}" aria-current="${id === currentView ? "page" : "false"}"><span>${icons[id as DemoView] ?? icons.signal}</span><small>${meta.label}</small></button>`).join("")}
    </nav>
  `;
  bindEvents();
}

function renderView(): string {
  switch (currentView) {
    case "signal": return renderSignal();
    case "routes": return renderRoutes();
    case "settings": return renderSettings();
    case "archive": return renderArchive();
  }
}

function frame(title: string, eyebrow: string, content: string, className = ""): string {
  return `<article class="demo-frame ${className}"><div class="demo-frame-head"><div><p class="demo-overline">${eyebrow}</p><h2>${title}</h2></div><span class="demo-frame-code">${icons.arrow}</span></div>${content}</article>`;
}

function renderSignal(): string {
  const metrics: readonly (readonly [string, string, string])[] = [
    ["ACTIVE SESSIONS", "24", "+08.4%"], ["AVG RESPONSE", "1.28s", "−12ms"], ["ROUTE HEALTH", "03 / 04", "one quiet"], ["UNREAD SIGNALS", "07", "review soon"],
  ];
  return `<section class="demo-grid" aria-label="Signal metrics">
    ${frame("The room is quiet", "CURRENT READ", `<div class="demo-hero-metric"><span class="demo-status-dot healthy"></span><strong>All primary routes are listening.</strong><p>Nothing is asking for attention. The latest edge pulse arrived 12 seconds ago.</p></div><div class="demo-metric-footer"><span>LAST PULSE</span><strong>12s ago</strong><span>WINDOW</span><strong>24 hours</strong></div>`, "demo-frame-main")}
    ${metrics.map(([label, value, note], index) => frame(label, "METRIC / 0" + (index + 1), `<div class="demo-stat"><strong>${value}</strong><span>${note}</span></div>`, "demo-frame-metric")).join("")}
    ${frame("Listening posts", "STATUS MAP", `<div class="demo-status-list">${routeStatuses.map((route) => `<div class="demo-status-item"><span class="demo-status-dot ${route.state}"></span><span><strong>${route.name}</strong><small>${route.channel}</small></span><b>${route.latency}</b></div>`).join("")}</div>`, "demo-frame-wide")}
    ${frame("Signal texture", "24H ACTIVITY", `<div class="demo-bars" aria-label="Sample activity chart">${Array.from({ length: 32 }, (_, index) => `<i style="--bar:${30 + ((index * 19) % 65)}%"></i>`).join("")}</div><div class="demo-chart-labels"><span>00:00</span><span>06:00</span><span>12:00</span><span>18:00</span><span>NOW</span></div>`, "demo-frame-chart")}
  </section>`;
}

function renderRoutes(): string {
  return `<section class="demo-grid demo-grid-routes" aria-label="Route health">
    ${frame("Three channels, one rhythm", "ROUTE TOPOLOGY", `<p class="demo-copy">Each route carries its own state without hiding the shared pulse. Use this view to spot a quiet failure before it becomes a conversation gap.</p><div class="demo-route-stack">${routeStatuses.map((route, index) => `<div class="demo-route-row"><span class="demo-route-index">0${index + 1}</span><span class="demo-status-dot ${route.state}"></span><span class="demo-route-name"><strong>${route.name}</strong><small>${route.channel}</small></span><b>${route.state.toUpperCase()}</b><span class="demo-route-detail">${route.detail}</span></div>`).join("")}</div>`, "demo-frame-main")}
    ${frame("Edge latency", "LAST 10 MINUTES", `<div class="demo-ring"><strong>84</strong><span>ms<br>median</span></div><div class="demo-inline-note"><span class="demo-status-dot healthy"></span> within local target <strong>120ms</strong></div>`, "demo-frame-side")}
  </section>`;
}

function renderSettings(): string {
  return `<section class="demo-grid demo-grid-settings" aria-label="Demo settings">
    ${frame("Tune the surface", "LOCAL PREFERENCES", `<div class="demo-setting-list"><label><span><strong>Ambient glow</strong><small>Keep the chromatic field alive.</small></span><input type="checkbox" checked><i></i></label><label><span><strong>Dense telemetry</strong><small>Show more context in each frame.</small></span><input type="checkbox" checked><i></i></label><label><span><strong>Quiet hours</strong><small>Pause non-critical signals at night.</small></span><input type="checkbox"><i></i></label></div>`, "demo-frame-main")}
    ${frame("Rendering profile", "DISPLAY", `<div class="demo-profile"><span>GLASS / 02</span><strong>Contrast-forward</strong><p>Light text, sharp accents, no soft corners. Built for a screen that stays calm under load.</p><button class="demo-small-button" type="button">Preview profile ${icons.arrow}</button></div>`, "demo-frame-side")}
  </section>`;
}

function renderArchive(): string {
  const events = ["QQ Official heartbeat acknowledged", "Configuration preview opened", "OneBot route entered pending", "Signal confidence crossed 99.9%", "Local simulation initialized"];
  return `<section class="demo-grid demo-grid-archive" aria-label="Recent events">
    ${frame("A short memory", "EVENT ARCHIVE", `<div class="demo-event-list">${events.map((event, index) => `<div class="demo-event"><span class="demo-event-time">${String(9 - index).padStart(2, "0")}:4${index}</span><span class="demo-status-dot ${index === 2 ? "attention" : "healthy"}"></span><span>${event}</span></div>`).join("")}</div>`, "demo-frame-main")}
    ${frame("Archive status", "LOCAL ONLY", `<div class="demo-archive-stamp">${icons.archive}<strong>SAFE</strong><span>5 events<br>retained</span></div>`, "demo-frame-side")}
  </section>`;
}

function bindEvents(): void {
  root.querySelectorAll<HTMLButtonElement>("[data-theme-choice]").forEach((button) => button.addEventListener("click", () => {
    const next = button.dataset.themeChoice;
    if (next === "night-shift" || next === "ember-grid" || next === "tide-signal") { currentTheme = next; render(); }
  }));
  root.querySelectorAll<HTMLElement>("[data-view]").forEach((element) => element.addEventListener("click", (event) => {
    event.preventDefault();
    const next = element.dataset.view;
    if (next === "signal" || next === "routes" || next === "settings" || next === "archive") { currentView = next; render(); }
  }));
}

render();
