const root = document.querySelector("#app");
const systemTheme = window.matchMedia("(prefers-color-scheme: light)");

let state = null;
let config = null;
let activePage = "general";
let activeGeneralTab = "appearance";
let saving = false;
let saveId = 0;
let statusText = "";
let appIconUrl = "";
let appIconDarkUrl = "";

const numberSpecs = {
  non_expanded_scale: { min: 0.5, max: 5, step: 0.05, precision: 2 },
  expanded_scale: { min: 0.5, max: 5, step: 0.05, precision: 2 },
  base_width: { min: 40, max: Number.POSITIVE_INFINITY, step: 5, precision: 0 },
  base_height: { min: 15, max: Number.POSITIVE_INFINITY, step: 2, precision: 0 },
  expanded_width: { min: 200, max: Number.POSITIVE_INFINITY, step: 10, precision: 0 },
  expanded_height: { min: 100, max: Number.POSITIVE_INFINITY, step: 10, precision: 0 },
  position_x_offset: { min: Number.NEGATIVE_INFINITY, max: Number.POSITIVE_INFINITY, step: 5, precision: 0 },
  position_y_offset: { min: Number.NEGATIVE_INFINITY, max: Number.POSITIVE_INFINITY, step: 5, precision: 0 },
  font_size: { min: 0, max: 30, step: 1, precision: 0 },
  auto_hide_delay: { min: 1, max: 60, step: 1, precision: 0 },
  hover_to_hide_distance: { min: 50, max: 300, step: 10, precision: 0 },
  hover_to_hide_delay: { min: 0.2, max: 3, step: 0.1, precision: 1 },
  update_check_interval: { min: 1, max: 24, step: 1, precision: 0 },
  lyrics_delay: { min: -10, max: 10, step: 0.1, precision: 1 },
  lyrics_scroll_max_width: { min: 100, max: 500, step: 10, precision: 0 },
};

function invoke(command, args) {
  const tauri = window["__TAURI__"];
  if (!tauri?.core?.invoke) {
    return Promise.reject(new Error("Tauri API unavailable"));
  }
  return tauri.core.invoke(command, args);
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function h(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function blobUrlFromBytes(iconBytes) {
  if (!Array.isArray(iconBytes) || iconBytes.length === 0) {
    return "";
  }
  return URL.createObjectURL(new Blob([new Uint8Array(iconBytes)], { type: "image/png" }));
}

function updateAppIconUrls() {
  if (!appIconUrl) {
    appIconUrl = blobUrlFromBytes(state?.app?.icon_png);
  }
  if (!appIconDarkUrl) {
    appIconDarkUrl = blobUrlFromBytes(state?.app?.icon_dark_png);
  }
}

function applyState(nextState) {
  state = nextState;
  config = clone(state.config);
  updateAppIconUrls();
  applyTheme();
  render();
}

function appLogo(className, iconUrl) {
  if (!iconUrl) {
    return `<div class="${h(className)}">EM</div>`;
  }
  return `<img class="${h(className)}" src="${h(iconUrl)}" alt="EchoMusic" />`;
}

function themedAppIconUrl() {
  return resolvedTheme() === "light" ? appIconDarkUrl : appIconUrl;
}

function tr(key) {
  return state?.translations?.[key] ?? key;
}

function resolvedTheme() {
  if (config?.settings_theme === "light") {
    return "light";
  }
  if (config?.settings_theme === "dark") {
    return "dark";
  }
  return systemTheme.matches ? "light" : "dark";
}

function applyTheme() {
  document.documentElement.dataset.theme = resolvedTheme();
}

function setStatus(text) {
  statusText = text;
  const node = document.querySelector(".status");
  if (node) {
    node.textContent = statusText;
  }
}

function clampNumber(field, value) {
  const spec = numberSpecs[field];
  if (!spec) {
    return value;
  }
  const min = spec.min;
  const max = spec.max;
  const rounded = Math.round(value / spec.step) * spec.step;
  const clamped = Math.min(Math.max(rounded, min), max);
  if (spec.precision === 0) {
    return Math.round(clamped);
  }
  return Number(clamped.toFixed(spec.precision));
}

function formatNumber(field) {
  const spec = numberSpecs[field];
  const value = Number(config[field] ?? 0);
  if (!spec || spec.precision === 0) {
    return String(Math.round(value));
  }
  return value.toFixed(spec.precision);
}

async function saveConfig() {
  const currentSaveId = ++saveId;
  saving = true;
  setStatus("");
  const nextConfig = clone(config);
  try {
    const nextState = await invoke("save_settings_config", { config: nextConfig });
    if (currentSaveId !== saveId) {
      return;
    }
    applyState(nextState);
  } catch (error) {
    setStatus(String(error));
  } finally {
    if (currentSaveId === saveId) {
      saving = false;
    }
  }
}

function setField(field, value) {
  if (numberSpecs[field]) {
    value = clampNumber(field, Number(value));
  }
  config[field] = value;
  render();
  void saveConfig();
}

async function refreshFromCommand(command) {
  setStatus("");
  try {
    const nextState = await invoke(command);
    applyState(nextState);
  } catch (error) {
    setStatus(String(error));
  }
}

function navButton(page, label) {
  const active = activePage === page ? " active" : "";
  return `<button class="nav-button${active}" data-action="page" data-page="${h(page)}">${h(label)}</button>`;
}

function tabButton(tab, label) {
  const active = activeGeneralTab === tab ? " active" : "";
  return `<button class="tab-button${active}" data-action="tab" data-tab="${h(tab)}">${h(label)}</button>`;
}

function section(title, rows) {
  return `
    <section class="section">
      <h2>${h(title)}</h2>
      <div class="group">${rows.join("")}</div>
    </section>
  `;
}

function switchRow(label, field, enabled = true) {
  const checked = config[field] ? "checked" : "";
  const disabled = enabled ? "" : "disabled";
  const dim = enabled ? "" : " disabled";
  return `
    <label class="row switch-row${dim}">
      <span>${h(label)}</span>
      <input type="checkbox" data-field="${h(field)}" ${checked} ${disabled} />
      <span class="switch"></span>
    </label>
  `;
}

function selectRow(label, field, options, enabled = true) {
  const disabled = enabled ? "" : "disabled";
  const dim = enabled ? "" : " disabled";
  const value = field === "language" && config.language === "auto" ? state.current_lang : config[field];
  const optionHtml = options
    .map((option) => {
      const selected = String(option.value) === String(value) ? "selected" : "";
      return `<option value="${h(option.value)}" ${selected}>${h(option.label)}</option>`;
    })
    .join("");
  return `
    <label class="row${dim}">
      <span>${h(label)}</span>
      <select data-field="${h(field)}" ${disabled}>${optionHtml}</select>
    </label>
  `;
}

function numberRow(label, field, enabled = true) {
  const spec = numberSpecs[field];
  const disabled = enabled ? "" : "disabled";
  const dim = enabled ? "" : " disabled";
  return `
    <div class="row number-row${dim}">
      <span>${h(label)}</span>
      <div class="stepper">
        <button class="icon-button" data-action="step" data-field="${h(field)}" data-delta="${-spec.step}" ${disabled}>-</button>
        <input type="number" data-field="${h(field)}" value="${h(formatNumber(field))}" step="${h(spec.step)}" ${Number.isFinite(spec.min) ? `min="${h(spec.min)}"` : ""} ${Number.isFinite(spec.max) ? `max="${h(spec.max)}"` : ""} ${disabled} />
        <button class="icon-button" data-action="step" data-field="${h(field)}" data-delta="${spec.step}" ${disabled}>+</button>
      </div>
    </div>
  `;
}

function textRow(label, field, placeholder, enabled = true, invalid = false) {
  const disabled = enabled ? "" : "disabled";
  const dim = enabled ? "" : " disabled";
  const bad = invalid ? " invalid" : "";
  const error = invalid ? `<div class="field-error">${h(tr("lyrics_filter_invalid_regex"))}</div>` : "";
  return `
    <label class="row text-row${dim}${bad}">
      <span>${h(label)}</span>
      <div class="text-field">
        <input type="text" data-field="${h(field)}" value="${h(config[field])}" placeholder="${h(placeholder)}" ${disabled} />
        ${error}
      </div>
    </label>
  `;
}

function infoRow(text) {
  return `<div class="row info-row"><span>${h(text)}</span></div>`;
}

function monitorOptions() {
  return state.monitors.map((label, index) => ({ value: String(index), label }));
}

function dockOptions() {
  return [
    { value: "top_center", label: tr("dock_position_top_center") },
    { value: "top_left", label: tr("dock_position_top_left") },
    { value: "top_right", label: tr("dock_position_top_right") },
    { value: "bottom_center", label: tr("dock_position_bottom_center") },
    { value: "bottom_left", label: tr("dock_position_bottom_left") },
    { value: "bottom_right", label: tr("dock_position_bottom_right") },
  ];
}

function generalAppearance() {
  return [
    section(tr("section_appearance"), [
      numberRow(tr("non_expanded_scale"), "non_expanded_scale"),
      numberRow(tr("expanded_scale"), "expanded_scale"),
      numberRow(tr("base_width"), "base_width"),
      numberRow(tr("base_height"), "base_height"),
      numberRow(tr("expanded_width"), "expanded_width"),
      numberRow(tr("expanded_height"), "expanded_height"),
      numberRow(tr("position_x_offset"), "position_x_offset"),
      numberRow(tr("position_y_offset"), "position_y_offset"),
      numberRow(tr("font_size"), "font_size"),
      selectRow(tr("monitor"), "monitor_index", monitorOptions()),
      selectRow(tr("dock_position"), "dock_position", dockOptions()),
    ]),
  ].join("");
}

function generalEffects() {
  return [
    section(tr("section_effects"), [
      selectRow(tr("settings_theme"), "settings_theme", [
        { value: "system", label: tr("theme_system") },
        { value: "light", label: tr("theme_light") },
        { value: "dark", label: tr("theme_dark") },
      ]),
      selectRow(tr("mini_cover_shape"), "mini_cover_shape", [
        { value: "square", label: tr("shape_square") },
        { value: "circle", label: tr("shape_circle") },
      ]),
      selectRow(tr("expanded_cover_shape"), "expanded_cover_shape", [
        { value: "square", label: tr("shape_square") },
        { value: "circle", label: tr("shape_circle") },
      ]),
      switchRow(tr("adaptive_border"), "adaptive_border"),
      switchRow(tr("motion_blur"), "motion_blur"),
      switchRow(tr("cover_rotate"), "cover_rotate"),
      switchRow(tr("audio_gate"), "audio_gate"),
      switchRow(tr("auto_gate"), "auto_gate", config.audio_gate),
    ]),
    section(tr("section_experimental"), [
      switchRow(tr("mini_controls"), "mini_controls"),
      selectRow(tr("island_style"), "island_style", [
        { value: "default", label: tr("style_default") },
        { value: "glass", label: tr("style_glass") },
        { value: "mica", label: tr("style_mica") },
        { value: "dynamic", label: tr("style_dynamic") },
        { value: "liquid_glass", label: tr("style_liquid_glass") },
      ]),
      fontPicker(),
    ]),
  ].join("");
}

function generalBehavior() {
  const rows = [
    switchRow(tr("start_boot"), "auto_start"),
    switchRow(tr("auto_hide"), "auto_hide"),
  ];
  if (config.auto_hide) {
    rows.push(numberRow(tr("hide_delay"), "auto_hide_delay"));
  }
  rows.push(switchRow(tr("hover_to_hide"), "hover_to_hide"));
  if (config.hover_to_hide) {
    rows.push(numberRow(tr("hover_to_hide_distance"), "hover_to_hide_distance"));
    rows.push(numberRow(tr("hover_to_hide_delay"), "hover_to_hide_delay"));
  }
  rows.push(
    selectRow(tr("language"), "language", [
      { value: "en", label: "English" },
      { value: "zh", label: "中文" },
    ]),
  );

  const updateRows = [switchRow(tr("check_updates"), "check_for_updates")];
  if (config.check_for_updates) {
    updateRows.push(numberRow(tr("update_interval"), "update_check_interval"));
  }

  return [
    section(tr("section_behavior"), rows),
    section(tr("section_updates"), updateRows),
    `<div class="danger-zone"><button class="text-danger" data-action="reset-settings">${h(tr("reset_defaults"))}</button></div>`,
  ].join("");
}

function fontPicker() {
  const hasFont = Boolean(config.custom_font_path);
  const label = hasFont ? tr("font_preview_custom") : tr("font_preview_default");
  const path = hasFont ? `<div class="path-text">${h(config.custom_font_path)}</div>` : "";
  const reset = hasFont ? `<button class="secondary-button" data-action="reset-font">${h(tr("font_reset"))}</button>` : "";
  return `
    <div class="row font-row">
      <span>${h(tr("custom_font"))}</span>
      <div class="font-actions">
        <button class="primary-button" data-action="select-font">${h(tr("font_select"))}</button>
        ${reset}
      </div>
    </div>
    <div class="font-preview">
      <span>${h(label)}</span>
      <strong>${h(tr("font_preview_sample"))}</strong>
      ${path}
    </div>
  `;
}

function generalContent() {
  if (activeGeneralTab === "effects") {
    return generalEffects();
  }
  if (activeGeneralTab === "behavior") {
    return generalBehavior();
  }
  return generalAppearance();
}

function musicContent() {
  const lyricsOn = config.show_lyrics;
  const charColorEnabled = lyricsOn && config.lyrics_char_highlight;
  const regexEnabled = lyricsOn && config.lyrics_filter_scope !== "off";
  return [
    section(tr("section_lyrics"), [
      switchRow(tr("show_lyrics"), "show_lyrics"),
      infoRow(tr("lyrics_ws_source")),
      infoRow(tr("lyrics_ws_address")),
      numberRow(tr("lyrics_delay"), "lyrics_delay", lyricsOn),
      switchRow(tr("lyrics_scroll"), "lyrics_scroll", lyricsOn),
      numberRow(tr("lyrics_scroll_max_width"), "lyrics_scroll_max_width", lyricsOn && config.lyrics_scroll),
      selectRow(
        tr("lyrics_filter_scope"),
        "lyrics_filter_scope",
        [
          { value: "off", label: tr("lyrics_filter_off") },
          { value: "desktop", label: tr("lyrics_filter_desktop") },
          { value: "all", label: tr("lyrics_filter_all") },
        ],
        lyricsOn,
      ),
      textRow(
        tr("lyrics_filter_regex"),
        "lyrics_filter_regex",
        tr("lyrics_filter_regex_placeholder"),
        regexEnabled,
        regexEnabled && !state.lyrics_filter_regex_valid,
      ),
    ]),
    section(tr("lyrics_char_highlight"), [
      switchRow(tr("lyrics_char_highlight"), "lyrics_char_highlight", lyricsOn),
      textRow(tr("lyrics_char_color_unplayed"), "lyrics_char_color_unplayed", tr("lyrics_char_color_placeholder"), charColorEnabled),
      textRow(tr("lyrics_char_color_played"), "lyrics_char_color_played", tr("lyrics_char_color_placeholder"), charColorEnabled),
    ]),
  ].join("");
}

function aboutContent() {
  return `
    <section class="about-panel">
      ${appLogo("app-mark", themedAppIconUrl())}
      <h2>EchoMusic-Lyrics-WinIsland</h2>
      <p>Version ${h(state.app.version)}</p>
      <button class="primary-button wide" data-action="check-updates">${h(tr("check_updates_now"))}</button>
      <p>${h(tr("created_by"))} ${h(state.app.author)}</p>
      <button class="link-button" data-action="open-homepage">${h(tr("visit_homepage"))}</button>
    </section>
  `;
}

function pageTitle() {
  if (activePage === "music") {
    return tr("tab_music");
  }
  if (activePage === "about") {
    return tr("tab_about");
  }
  return tr("tab_general");
}

function pageContent() {
  if (activePage === "music") {
    return musicContent();
  }
  if (activePage === "about") {
    return aboutContent();
  }
  return `
    <div class="tabs">
      ${tabButton("appearance", tr("section_appearance"))}
      ${tabButton("effects", tr("section_effects"))}
      ${tabButton("behavior", tr("section_behavior"))}
    </div>
    ${generalContent()}
  `;
}

function render() {
  if (!root || !state || !config) {
    return;
  }
  root.innerHTML = `
    <div class="settings-shell">
      <aside class="sidebar">
        <div class="brand">
          ${appLogo("brand-icon", themedAppIconUrl())}
          <div>
            <strong>EchoMusic</strong>
            <span>${h(state.app.version)}</span>
          </div>
        </div>
        <nav>
          ${navButton("general", tr("tab_general"))}
          ${navButton("music", tr("tab_music"))}
          ${navButton("about", tr("tab_about"))}
        </nav>
      </aside>
      <main class="content">
        <header class="content-header">
          <h1>${h(pageTitle())}</h1>
          <div class="status">${h(statusText)}</div>
        </header>
        ${pageContent()}
      </main>
    </div>
  `;
}

function handleClick(event) {
  const button = event.target.closest("[data-action]");
  if (!button || !root.contains(button)) {
    return;
  }
  const action = button.dataset.action;
  if (action === "page") {
    activePage = button.dataset.page;
    render();
  } else if (action === "tab") {
    activeGeneralTab = button.dataset.tab;
    render();
  } else if (action === "step") {
    const field = button.dataset.field;
    const delta = Number(button.dataset.delta);
    setField(field, Number(config[field] ?? 0) + delta);
  } else if (action === "select-font") {
    void refreshFromCommand("select_font_file");
  } else if (action === "reset-font") {
    void refreshFromCommand("reset_font");
  } else if (action === "reset-settings") {
    void refreshFromCommand("reset_settings");
  } else if (action === "check-updates") {
    void invoke("check_updates_now").catch((error) => setStatus(String(error)));
  } else if (action === "open-homepage") {
    void invoke("open_homepage").catch((error) => setStatus(String(error)));
  }
}

function handleChange(event) {
  const target = event.target;
  const field = target?.dataset?.field;
  if (!field) {
    return;
  }
  if (target.type === "checkbox") {
    setField(field, target.checked);
  } else if (target.tagName === "SELECT") {
    const value = field === "monitor_index" ? Number(target.value) : target.value;
    setField(field, value);
  } else if (target.type === "number") {
    setField(field, Number(target.value));
  } else if (target.type === "text") {
    setField(field, target.value);
  }
}

function handleKeydown(event) {
  const target = event.target;
  if (event.key === "Enter" && target?.dataset?.field) {
    target.blur();
  }
}

async function load() {
  if (!root) {
    return;
  }
  root.innerHTML = `<div class="loading">EchoMusic-Lyrics-WinIsland</div>`;
  try {
    applyState(await invoke("get_settings_state"));
  } catch (error) {
    root.innerHTML = `<div class="loading error">${h(error)}</div>`;
  }
}

root?.addEventListener("click", handleClick);
root?.addEventListener("change", handleChange);
root?.addEventListener("keydown", handleKeydown);
systemTheme.addEventListener("change", () => {
  if (config?.settings_theme === "system") {
    applyTheme();
    render();
  }
});

void load();
