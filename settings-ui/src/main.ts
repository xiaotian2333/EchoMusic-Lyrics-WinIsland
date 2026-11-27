import { initColorPicker, isAutoColor, normalizeHex, openColorPicker, repositionColorPicker } from "./color-picker";
import type { AppConfig, ProcessDockRule, SettingsState } from "./types";

/* 配置字段以 Rust 侧 AppConfig 为契约，这里同时允许按字段名动态访问（表单按字段名驱动）。 */
type Config = AppConfig & Record<string, any>;
type NumberSpec = { min: number; max: number; step: number; precision: number };
type SelectOption = { value: string; label: string };

const SAVE_DEBOUNCE_MS = 300;

const root = document.querySelector<HTMLDivElement>("#app");
const systemTheme = window.matchMedia("(prefers-color-scheme: light)");

const EMPTY_STATE: SettingsState = {
  config: {} as AppConfig,
  current_lang: "en",
  translations: {},
  monitors: [],
  app: { version: "", author: "", homepage: "", icon_png: [], icon_dark_png: [] },
  lyrics_filter_regex_valid: true,
};

let state: SettingsState = EMPTY_STATE;
let config: Config = {} as Config;
let stateLoaded = false;
let activePage = "general";
let activeGeneralTab = "appearance";
let saving = false;
let saveId = 0;
let statusText = "";
let appIconUrl = "";
let appIconDarkUrl = "";
let customFontStyle: HTMLStyleElement | null = null;
let customFontUrl = "";
let customFontPath = "";
let saveTimer: number | null = null;
let pendingExternalState: SettingsState | null = null;

const COLOR_FIELDS = ["lyrics_char_color_unplayed", "lyrics_char_color_played"];

const numberSpecs: Record<string, NumberSpec> = {
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
  liquid_glass_blur: { min: 0, max: 30, step: 1, precision: 0 },
  liquid_glass_refraction: { min: 0, max: 400, step: 10, precision: 0 },
  liquid_glass_dispersion: { min: 0, max: 100, step: 5, precision: 0 },
  liquid_glass_tint: { min: 0, max: 200, step: 10, precision: 0 },
  glass_blur: { min: 0, max: 50, step: 1, precision: 0 },
};

function numberSpec(field: string): NumberSpec | null {
  const spec = numberSpecs[field];
  if (!spec) {
    return null;
  }
  if (field !== "lyrics_scroll_max_width") {
    return spec;
  }
  const min = Math.max(spec.min, Number(config.base_width ?? 0) + 35);
  return { ...spec, min, max: Math.max(spec.max, min) };
}

function invoke(command: string): Promise<any>;
function invoke(command: string, args: Record<string, unknown>): Promise<any>;
function invoke(command: string, args?: Record<string, unknown>): Promise<any> {
  const tauri = window.__TAURI__;
  if (!tauri?.core?.invoke) {
    return Promise.reject(new Error(tr("tauri_unavailable")));
  }
  return tauri.core.invoke(command, args);
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function h(value: unknown): string {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function blobUrlFromBytes(iconBytes: unknown): string {
  if (!Array.isArray(iconBytes) || iconBytes.length === 0) {
    return "";
  }
  return URL.createObjectURL(new Blob([new Uint8Array(iconBytes)], { type: "image/png" }));
}

function bytesFromBase64(b64: string): Uint8Array {
  const binaryStr = atob(b64);
  const bytes = new Uint8Array(binaryStr.length);
  for (let i = 0; i < binaryStr.length; i++) {
    bytes[i] = binaryStr.charCodeAt(i);
  }
  return bytes;
}

function updateAppIconUrls(): void {
  if (!appIconUrl) {
    appIconUrl = blobUrlFromBytes(state.app.icon_png);
  }
  if (!appIconDarkUrl) {
    appIconDarkUrl = blobUrlFromBytes(state.app.icon_dark_png);
  }
}

const colorPickerHooks = {
  tr: (key: string) => tr(key),
  getValue: (field: string) => config[field],
  onCommit: (field: string, value: unknown) => setField(field, value),
};

function applyState(nextState: SettingsState): void {
  state = nextState;
  config = clone(nextState.config) as Config;
  stateLoaded = true;
  updateAppIconUrls();
  applyTheme();
  // 面板 DOM 常驻，每次状态刷新都要按当前翻译与配置重建文案
  initColorPicker(colorPickerHooks);
  render();
  void loadCustomFont();
}

function appLogo(className: string, iconUrl: string): string {
  if (!iconUrl) {
    return `<div class="${h(className)}">EM</div>`;
  }
  return `<img class="${h(className)}" src="${h(iconUrl)}" alt="EchoMusic" />`;
}

function themedAppIconUrl(): string {
  return resolvedTheme() === "light" ? appIconDarkUrl : appIconUrl;
}

function tr(key: string): string {
  return state.translations[key] ?? key;
}

function resolvedTheme(): string {
  if (config.settings_theme === "light") {
    return "light";
  }
  if (config.settings_theme === "dark") {
    return "dark";
  }
  return systemTheme.matches ? "light" : "dark";
}

function applyTheme(): void {
  document.documentElement.dataset.theme = resolvedTheme();
}

function setStatus(text: string): void {
  statusText = text;
  const node = document.querySelector(".status");
  if (node) {
    node.textContent = statusText;
  }
}

function clampNumber(field: string, value: number): number {
  const spec = numberSpec(field);
  if (!spec) {
    return Number.isFinite(value) ? value : 0;
  }
  // 非法输入（如粘贴非数字）回退到最小值，避免 NaN 进入配置导致后台反序列化失败
  const safe = Number.isFinite(value) ? value : spec.min;
  const min = spec.min;
  const max = spec.max;
  const rounded = Math.round(safe / spec.step) * spec.step;
  const clamped = Math.min(Math.max(rounded, min), max);
  if (spec.precision === 0) {
    return Math.round(clamped);
  }
  return Number(clamped.toFixed(spec.precision));
}

function formatNumber(field: string): string {
  const spec = numberSpec(field);
  const value = spec ? clampNumber(field, Number(config[field] ?? 0)) : Number(config[field] ?? 0);
  if (!spec || spec.precision === 0) {
    return String(Math.round(Number.isFinite(value) ? value : 0));
  }
  return (Number.isFinite(value) ? value : 0).toFixed(spec.precision);
}

/** 保存前兜底清洗：任何非有限数值都回退到当前已生效值，保证序列化永远合法。 */
function sanitizeConfig(next: Record<string, any>): void {
  for (const field of Object.keys(numberSpecs)) {
    const value = Number(next[field]);
    if (!Number.isFinite(value)) {
      next[field] = Number(config[field] ?? numberSpecs[field].min);
    }
  }
}

async function saveConfig(): Promise<void> {
  const currentSaveId = ++saveId;
  saving = true;
  setStatus("");
  const nextConfig = clone(config);
  sanitizeConfig(nextConfig);
  try {
    const nextState = (await invoke("save_settings_config", { config: nextConfig })) as SettingsState;
    if (currentSaveId !== saveId) {
      return;
    }
    applyState(nextState);
  } catch (error) {
    setStatus(String(error));
  } finally {
    if (currentSaveId === saveId) {
      saving = false;
      if (pendingExternalState) {
        const pending = pendingExternalState;
        pendingExternalState = null;
        applyState(pending);
      }
    }
  }
}

/** 字段变更的保存防抖：连续调整时合并为一次落盘与重渲染。 */
function scheduleSave(): void {
  if (saveTimer !== null) {
    window.clearTimeout(saveTimer);
  }
  saveTimer = window.setTimeout(() => {
    saveTimer = null;
    void saveConfig();
  }, SAVE_DEBOUNCE_MS);
}

function setField(field: string, value: unknown): void {
  if (numberSpec(field)) {
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      // 非法数值不写入配置，也不触发保存，避免整份配置无法反序列化
      return;
    }
    value = clampNumber(field, parsed);
  }
  if (field === "non_expanded_wheel_volume" && !config.spectrum_wheel_volume) {
    value = false;
  }
  config[field] = value;
  if (field === "spectrum_wheel_volume" && !value) {
    config.non_expanded_wheel_volume = false;
  }
  if (field === "base_width" || field === "lyrics_scroll_max_width") {
    config.lyrics_scroll_max_width = clampNumber(
      "lyrics_scroll_max_width",
      Number(config.lyrics_scroll_max_width ?? 0),
    );
  }
  render();
  scheduleSave();
}

async function refreshFromCommand(command: string): Promise<void> {
  setStatus("");
  try {
    const nextState = (await invoke(command)) as SettingsState;
    applyState(nextState);
  } catch (error) {
    setStatus(String(error));
  }
}

/** 主进程或其他写入方改了配置：本地正在保存时延后应用，避免覆盖用户输入。 */
function onExternalState(nextState: SettingsState): void {
  if (saving) {
    pendingExternalState = nextState;
    return;
  }
  if (JSON.stringify(nextState.config) === JSON.stringify(config)) {
    return;
  }
  applyState(nextState);
}

function navButton(page: string, label: string): string {
  const active = activePage === page ? " active" : "";
  return `<button class="nav-button${active}" data-action="page" data-page="${h(page)}">${h(label)}</button>`;
}

function tabButton(tab: string, label: string): string {
  const active = activeGeneralTab === tab ? " active" : "";
  return `<button class="tab-button${active}" data-action="tab" data-tab="${h(tab)}">${h(label)}</button>`;
}

function section(title: string, rows: string[]): string {
  return `
    <section class="section">
      <h2>${h(title)}</h2>
      <div class="group">${rows.join("")}</div>
    </section>
  `;
}

function switchRow(label: string, field: string, enabled = true, checkedValue: unknown = config[field]): string {
  const checked = checkedValue ? "checked" : "";
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

function selectRow(label: string, field: string, options: SelectOption[], enabled = true): string {
  const disabled = enabled ? "" : "disabled";
  const dim = enabled ? "" : " disabled";
  const value = field === "language" && config.language === "auto" ? "auto" : config[field];
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

function numberRow(label: string, field: string, enabled = true): string {
  const spec = numberSpec(field);
  if (!spec) {
    return "";
  }
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

function textRow(label: string, field: string, placeholder: string, enabled = true, invalid = false): string {
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

function colorRow(label: string, field: string, enabled = true): string {
  const raw = String(config[field] ?? "auto");
  // 非法值按 auto 展示，与后端 normalized 的回退保持一致
  const hex = isAutoColor(raw) ? null : normalizeHex(raw);
  const disabled = enabled ? "" : "disabled";
  const dim = enabled ? "" : " disabled";
  const style = hex ? ` style="background:${h(hex)}"` : "";
  const mark = hex ? "" : `<span class="color-swatch-mark">A</span>`;
  return `
    <div class="row color-row${dim}">
      <span>${h(label)}</span>
      <button class="color-swatch${hex ? "" : " auto"}"${style} data-action="open-color-picker" data-color-field="${h(field)}" title="${h(hex ?? tr("color_picker_auto"))}" aria-haspopup="dialog" aria-label="${h(label)}" ${disabled}>
        ${mark}
      </button>
    </div>
  `;
}

/** auto 状态下的起始颜色：借用另一个颜色项的当前值，避免每次都从同一个默认蓝开始 */
function colorFieldSeed(field: string): string | null {
  for (const other of COLOR_FIELDS) {
    if (other === field) {
      continue;
    }
    const hex = normalizeHex(String(config[other] ?? ""));
    if (hex) {
      return hex;
    }
  }
  return null;
}

function infoRow(text: string): string {
  return `<div class="row info-row"><span>${h(text)}</span></div>`;
}

function monitorOptions(): SelectOption[] {
  return state.monitors.map((label, index) => ({ value: String(index), label }));
}

function dockOptions(): SelectOption[] {
  return [
    { value: "top_center", label: tr("dock_position_top_center") },
    { value: "top_left", label: tr("dock_position_top_left") },
    { value: "top_right", label: tr("dock_position_top_right") },
    { value: "bottom_center", label: tr("dock_position_bottom_center") },
    { value: "bottom_left", label: tr("dock_position_bottom_left") },
    { value: "bottom_right", label: tr("dock_position_bottom_right") },
  ];
}

function processRulesSection(): string {
  const rules: ProcessDockRule[] = config.process_dock_rules || [];
  const rows = rules.map((rule, i) => {
    const opts = dockOptions()
      .map((o) => `<option value="${h(o.value)}"${o.value === rule.dock_position ? " selected" : ""}>${h(o.label)}</option>`)
      .join("");
    return `
      <div class="row process-rule-row">
        <div class="rule-inputs">
          <input type="text" data-rule-process="${i}" value="${h(rule.process_name)}" placeholder="${h(tr("process_name_placeholder"))}" />
          <select data-rule-position="${i}">${opts}</select>
        </div>
        <button class="icon-button remove-rule-btn" data-action="remove-rule" data-index="${i}">&times;</button>
      </div>
    `;
  });
  rows.push(`
    <div class="row add-rule-row">
      <button class="primary-button" data-action="add-rule">+ ${h(tr("add_rule"))}</button>
    </div>
  `);
  return section(tr("section_process_rules"), rows);
}

function generalAppearance(): string {
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
    processRulesSection(),
  ].join("");
}

function liquidGlassRows(): string[] {
  if (
    config.island_style !== "liquid_glass_wgc" &&
    config.island_style !== "liquid_glass_dxgi"
  ) {
    return [];
  }
  const rows = [
    numberRow(tr("liquid_glass_blur"), "liquid_glass_blur"),
    numberRow(tr("liquid_glass_refraction"), "liquid_glass_refraction"),
    numberRow(tr("liquid_glass_dispersion"), "liquid_glass_dispersion"),
    numberRow(tr("liquid_glass_tint"), "liquid_glass_tint"),
    switchRow(tr("liquid_glass_siri"), "liquid_glass_siri"),
    infoRow(tr("liquid_glass_siri_note")),
  ];
  if (config.island_style === "liquid_glass_wgc") {
    rows.push(
      switchRow(tr("liquid_glass_compat_capture"), "liquid_glass_compat_capture"),
      infoRow(
        tr(
          config.liquid_glass_compat_capture
            ? "liquid_glass_compat_capture_note"
            : "style_liquid_note",
        ),
      ),
    );
  }
  return rows;
}

function glassRows(): string[] {
  if (config.island_style !== "glass") {
    return [];
  }
  return [numberRow(tr("glass_blur"), "glass_blur")];
}

function generalEffects(): string {
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
      switchRow(tr("motion_blur"), "motion_blur"),
      switchRow(tr("cover_rotate"), "cover_rotate"),
      switchRow(tr("audio_gate"), "audio_gate"),
      switchRow(tr("volume_wheel_hint"), "volume_wheel_hint"),
    ]),
    section(tr("section_experimental"), [
      switchRow(tr("mini_controls"), "mini_controls"),
      selectRow(tr("island_style"), "island_style", [
        { value: "default", label: tr("style_default") },
        { value: "glass", label: tr("style_glass") },
        { value: "mica", label: tr("style_mica") },
        { value: "dynamic", label: tr("style_dynamic") },
        { value: "liquid_glass_wgc", label: tr("style_liquid_glass") },
        { value: "liquid_glass_dxgi", label: tr("style_liquid_glass_dxgi") },
      ]),
      ...glassRows(),
      ...liquidGlassRows(),
      fontPicker(),
    ]),
  ].join("");
}

function generalBehavior(): string {
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
  rows.push(switchRow(tr("spectrum_wheel_volume"), "spectrum_wheel_volume"));
  rows.push(
    switchRow(
      tr("non_expanded_wheel_volume"),
      "non_expanded_wheel_volume",
      config.spectrum_wheel_volume,
      config.spectrum_wheel_volume && config.non_expanded_wheel_volume,
    ),
  );
  rows.push(
    selectRow(tr("language"), "language", [
      { value: "auto", label: tr("language_auto") },
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

function fontPicker(): string {
  const hasFont = Boolean(config.custom_font_path);
  const label = hasFont ? tr("font_preview_custom") : tr("font_preview_default");
  const path = hasFont ? `<div class="path-text">${h(config.custom_font_path)}</div>` : "";
  const reset = hasFont ? `<button class="secondary-button" data-action="reset-font">${h(tr("font_reset"))}</button>` : "";
  const previewClass = hasFont ? " custom-font-preview" : "";
  return `
    <div class="row font-row">
      <span>${h(tr("custom_font"))}</span>
      <div class="font-actions">
        <button class="primary-button" data-action="select-font">${h(tr("font_select"))}</button>
        ${reset}
      </div>
    </div>
    <div class="font-preview${previewClass}">
      <span>${h(label)}</span>
      <strong>${h(tr("font_preview_sample"))}</strong>
      ${path}
    </div>
  `;
}

function generalContent(): string {
  if (activeGeneralTab === "effects") {
    return generalEffects();
  }
  if (activeGeneralTab === "behavior") {
    return generalBehavior();
  }
  return generalAppearance();
}

function musicContent(): string {
  const lyricsOn = config.show_lyrics;
  const charColorEnabled = lyricsOn && config.lyrics_char_highlight;
  const regexEnabled = lyricsOn && config.lyrics_filter_scope !== "off";
  const infiniteLoopEnabled = lyricsOn && config.lyrics_scroll && !config.lyrics_char_highlight;
  const infiniteLoopChecked = config.lyrics_char_highlight ? false : config.lyrics_scroll_infinite_loop;
  return [
    section(tr("section_lyrics"), [
      switchRow(tr("show_lyrics"), "show_lyrics"),
      infoRow(tr("lyrics_ws_source")),
      infoRow(tr("lyrics_ws_address")),
      numberRow(tr("lyrics_delay"), "lyrics_delay", lyricsOn),
      selectRow(
        tr("lyrics_transition_animation"),
        "lyrics_transition_animation",
        [
          { value: "blur", label: tr("lyrics_transition_blur") },
          { value: "slide", label: tr("lyrics_transition_slide") },
          { value: "fade", label: tr("lyrics_transition_fade") },
        ],
        lyricsOn,
      ),
      switchRow(tr("lyrics_scroll"), "lyrics_scroll", lyricsOn),
      numberRow(tr("lyrics_scroll_max_width"), "lyrics_scroll_max_width", lyricsOn && config.lyrics_scroll),
      switchRow(
        tr("lyrics_scroll_infinite_loop"),
        "lyrics_scroll_infinite_loop",
        infiniteLoopEnabled,
        infiniteLoopChecked,
      ),
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
      switchRow(tr("lyrics_char_lift_animation"), "lyrics_char_lift_animation", charColorEnabled),
      colorRow(tr("lyrics_char_color_unplayed"), "lyrics_char_color_unplayed", charColorEnabled),
      colorRow(tr("lyrics_char_color_played"), "lyrics_char_color_played", charColorEnabled),
    ]),
  ].join("");
}

function aboutContent(): string {
  return `
    <section class="about-panel">
      ${appLogo("app-mark", themedAppIconUrl())}
      <h2>EchoMusic-Lyrics-WinIsland</h2>
      <p>${h(tr("about_version").replace("{version}", state.app.version))}</p>
      <button class="primary-button wide" data-action="check-updates">${h(tr("check_updates_now"))}</button>
      <p>${h(tr("created_by"))} ${h(state.app.author)}</p>
      <button class="link-button" data-action="open-homepage">${h(tr("visit_homepage"))}</button>
    </section>
  `;
}

function pageTitle(): string {
  if (activePage === "music") {
    return tr("tab_music");
  }
  if (activePage === "about") {
    return tr("tab_about");
  }
  return tr("tab_general");
}

function pageContent(): string {
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

function render(): void {
  if (!root || !stateLoaded) {
    return;
  }
  const scrollPos = document.querySelector(".content")?.scrollTop ?? 0;
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
  requestAnimationFrame(() => {
    const el = document.querySelector(".content");
    if (el) {
      el.scrollTop = scrollPos;
    }
    // DOM 重建可能触发浏览器滚动锚定偏移根滚动器；复位兜底，避免整页上移错乱
    document.documentElement.scrollTop = 0;
    document.body.scrollTop = 0;
    // 滚动位置恢复之后再定位，顺序不能颠倒
    repositionColorPicker();
  });
}

function handleClick(event: MouseEvent): void {
  const target = event.target as HTMLElement | null;
  const button = target?.closest<HTMLElement>("[data-action]");
  if (!button || !root?.contains(button)) {
    return;
  }
  const action = button.dataset.action;
  if (action === "page") {
    activePage = button.dataset.page ?? "general";
    render();
  } else if (action === "tab") {
    activeGeneralTab = button.dataset.tab ?? "appearance";
    render();
  } else if (action === "step") {
    const field = button.dataset.field;
    if (!field) {
      return;
    }
    const delta = Number(button.dataset.delta);
    if (!Number.isFinite(delta)) {
      return;
    }
    setField(field, Number(config[field] ?? 0) + delta);
  } else if (action === "select-font") {
    void refreshFromCommand("select_font_file");
  } else if (action === "reset-font") {
    void refreshFromCommand("reset_font");
  } else if (action === "reset-settings") {
    if (window.confirm(tr("reset_defaults_confirm"))) {
      void refreshFromCommand("reset_settings");
    }
  } else if (action === "check-updates") {
    void invoke("check_updates_now").catch((error: unknown) => setStatus(String(error)));
  } else if (action === "open-homepage") {
    void invoke("open_homepage").catch((error: unknown) => setStatus(String(error)));
  } else if (action === "add-rule") {
    config.process_dock_rules = config.process_dock_rules || [];
    config.process_dock_rules.push({ process_name: "", dock_position: "top_center" });
    render();
    scheduleSave();
  } else if (action === "remove-rule") {
    const index = Number(button.dataset.index);
    config.process_dock_rules = config.process_dock_rules || [];
    if (index >= 0 && index < config.process_dock_rules.length) {
      config.process_dock_rules.splice(index, 1);
      render();
      scheduleSave();
    }
  } else if (action === "open-color-picker") {
    const field = button.dataset.colorField;
    if (field) {
      openColorPicker({
        field,
        value: String(config[field] ?? "auto"),
        anchor: button,
        seedHex: colorFieldSeed(field),
      });
    }
  }
}

function handleChange(event: Event): void {
  const target = event.target as HTMLInputElement | HTMLSelectElement | null;
  if (!target) {
    return;
  }
  const field = target.dataset.field;

  const ruleIdx = target.dataset.ruleProcess ?? target.dataset.rulePosition;
  if (ruleIdx !== undefined) {
    const idx = Number(ruleIdx);
    const rules: ProcessDockRule[] = config.process_dock_rules || [];
    if (idx >= 0 && idx < rules.length) {
      if (target.dataset.ruleProcess !== undefined) {
        rules[idx].process_name = target.value;
      } else if (target.dataset.rulePosition !== undefined) {
        rules[idx].dock_position = target.value;
      }
      render();
      scheduleSave();
    }
    return;
  }

  if (!field) {
    return;
  }
  if (target instanceof HTMLInputElement && target.type === "checkbox") {
    setField(field, target.checked);
  } else if (target.tagName === "SELECT") {
    const value = field === "monitor_index" ? Number(target.value) : target.value;
    setField(field, value);
  } else if (target instanceof HTMLInputElement && target.type === "number") {
    setField(field, Number(target.value));
  } else {
    setField(field, target.value);
  }
}

function handleKeydown(event: KeyboardEvent): void {
  const target = event.target as HTMLElement | null;
  if (event.key === "Enter" && target?.dataset.field) {
    target.blur();
  }
}

async function loadBuiltinFont(): Promise<void> {
  try {
    const b64 = (await invoke("get_builtin_font")) as string | null;
    if (!b64) {
      return;
    }
    const bytes = bytesFromBase64(b64);
    const blob = new Blob([bytes as BlobPart], { type: "font/otf" });
    const url = URL.createObjectURL(blob);
    const style = document.createElement("style");
    style.textContent =
      `@font-face { font-family: "AppBuiltinFont"; src: url("${url}") format("opentype"); font-display: swap; }`;
    document.head.appendChild(style);
  } catch (e) {
    console.warn("Failed to load built-in font:", e);
  }
}

function fontMimeFromPath(path: string): string {
  return path.toLowerCase().endsWith(".ttf") ? "font/ttf" : "font/otf";
}

function fontFormatFromPath(path: string): string {
  return path.toLowerCase().endsWith(".ttf") ? "truetype" : "opentype";
}

function clearCustomFont(): void {
  customFontStyle?.remove();
  customFontStyle = null;
  if (customFontUrl) {
    URL.revokeObjectURL(customFontUrl);
  }
  customFontUrl = "";
  customFontPath = "";
}

async function loadCustomFont(): Promise<void> {
  const path = config.custom_font_path || "";
  if (!path) {
    clearCustomFont();
    return;
  }
  if (path === customFontPath) {
    return;
  }
  try {
    const b64 = (await invoke("get_custom_font")) as string | null;
    if (config.custom_font_path !== path) {
      return;
    }
    if (!b64) {
      clearCustomFont();
      return;
    }
    const bytes = bytesFromBase64(b64);
    const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: fontMimeFromPath(path) }));
    const style = document.createElement("style");
    style.textContent =
      `@font-face { font-family: "AppCustomFont"; src: url("${url}") format("${fontFormatFromPath(path)}"); font-display: swap; }`;
    clearCustomFont();
    customFontStyle = style;
    customFontUrl = url;
    customFontPath = path;
    document.head.appendChild(style);
  } catch (e) {
    if (config.custom_font_path === path) {
      clearCustomFont();
    }
    console.warn("加载自定义字体失败:", e);
  }
}

/** 监听主进程推送的配置变更，保证设置页与真实运行状态一致。 */
async function listenConfigChanges(): Promise<void> {
  const listen = window.__TAURI__?.event?.listen;
  if (!listen) {
    return;
  }
  try {
    await listen("config-changed", (event) => {
      const payload = event.payload as SettingsState | undefined;
      if (payload?.config) {
        onExternalState(payload);
      }
    });
  } catch (e) {
    console.warn("Failed to listen config-changed:", e);
  }
}

async function load(): Promise<void> {
  if (!root) {
    return;
  }
  root.innerHTML = `<div class="loading">EchoMusic-Lyrics-WinIsland</div>`;
  void listenConfigChanges();
  void loadBuiltinFont();
  try {
    applyState((await invoke("get_settings_state")) as SettingsState);
  } catch (error) {
    root.innerHTML = `<div class="loading error">${h(error)}</div>`;
  }
}

/* ── 自定义右键菜单 ── */

type TextField = HTMLInputElement | HTMLTextAreaElement;

let contextTarget: TextField | null = null;

function buildContextMenu(): HTMLDivElement {
  const menu = document.createElement("div");
  menu.className = "context-menu";
  menu.dataset.role = "context-menu";

  const copyBtn = document.createElement("button");
  copyBtn.textContent = tr("action_copy");
  copyBtn.addEventListener("click", async () => {
    if (contextTarget && document.body.contains(contextTarget)) {
      const start = contextTarget.selectionStart;
      const end = contextTarget.selectionEnd;
      if (start !== null && end !== null && start !== end) {
        await navigator.clipboard.writeText(contextTarget.value.slice(start, end));
      } else {
        await navigator.clipboard.writeText(contextTarget.value);
      }
    }
    hideContextMenu();
  });

  const pasteBtn = document.createElement("button");
  pasteBtn.textContent = tr("action_paste");
  pasteBtn.addEventListener("click", async () => {
    if (contextTarget && document.body.contains(contextTarget)) {
      const text = await navigator.clipboard.readText();
      const start = contextTarget.selectionStart ?? contextTarget.value.length;
      const end = contextTarget.selectionEnd ?? contextTarget.value.length;
      const before = contextTarget.value.slice(0, start);
      const after = contextTarget.value.slice(end);
      contextTarget.value = before + text + after;
      const pos = start + text.length;
      contextTarget.setSelectionRange(pos, pos);
      contextTarget.dispatchEvent(new Event("change", { bubbles: true }));
      contextTarget.focus();
    }
    hideContextMenu();
  });

  menu.appendChild(copyBtn);
  menu.appendChild(pasteBtn);
  return menu;
}

function showContextMenu(x: number, y: number, el: TextField): void {
  hideContextMenu();
  contextTarget = el;
  const menu = document.querySelector<HTMLDivElement>(".context-menu");
  if (!menu) return;
  const btns = menu.querySelectorAll("button");
  if (btns.length >= 2) {
    btns[0].textContent = tr("action_copy");
    btns[1].textContent = tr("action_paste");
  }
  menu.style.display = "block";
  menu.style.left = x + "px";
  menu.style.top = y + "px";
}

function hideContextMenu(): void {
  contextTarget = null;
  const menu = document.querySelector<HTMLDivElement>(".context-menu");
  if (menu) {
    menu.style.display = "none";
  }
}

/* ── 全局拦截 ── */

document.body.appendChild(buildContextMenu());

document.addEventListener("contextmenu", (e) => {
  const target = e.target as HTMLElement | null;
  const isInput = target?.tagName === "INPUT" || target?.tagName === "TEXTAREA";
  if (isInput && root && document.body.contains(root)) {
    e.preventDefault();
    showContextMenu(e.clientX, e.clientY, target as TextField);
  } else if (!isInput) {
    e.preventDefault();
  }
});

document.addEventListener("click", (e) => {
  const target = e.target as HTMLElement | null;
  if (!target?.closest(".context-menu")) {
    hideContextMenu();
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    hideContextMenu();
    return;
  }
  if (
    e.key === "F12" ||
    (e.ctrlKey && e.shiftKey && ["I", "J", "C"].includes(e.key)) ||
    (e.ctrlKey && e.key === "U")
  ) {
    e.preventDefault();
  }
});

root?.addEventListener("click", handleClick);
root?.addEventListener("change", handleChange);
root?.addEventListener("keydown", handleKeydown);
systemTheme.addEventListener("change", () => {
  if (config.settings_theme === "system") {
    applyTheme();
    render();
  }
});

void load();
