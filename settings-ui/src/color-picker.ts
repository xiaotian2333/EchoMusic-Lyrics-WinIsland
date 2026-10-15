/* ── 颜色转换（纯函数，供色块与色轮面板共用） ── */

export interface Rgb {
  r: number
  g: number
  b: number
}

export interface Hsv {
  h: number
  s: number
  v: number
}

export function clamp01(value: number): number {
  if (!Number.isFinite(value)) {
    return 0
  }
  return Math.min(1, Math.max(0, value))
}

export function isAutoColor(value: unknown): boolean {
  return String(value ?? "").trim().toLowerCase() === "auto"
}

export function normalizeHex(value: unknown): string | null {
  const text = String(value ?? "").trim()
  const hex = text.startsWith("#") ? text.slice(1) : text
  if (!/^[0-9a-fA-F]{6}$/.test(hex)) {
    return null
  }
  return "#" + hex.toUpperCase()
}

export function hexToRgb(hex: string): Rgb | null {
  const normalized = normalizeHex(hex)
  if (!normalized) {
    return null
  }
  return {
    r: parseInt(normalized.slice(1, 3), 16),
    g: parseInt(normalized.slice(3, 5), 16),
    b: parseInt(normalized.slice(5, 7), 16),
  }
}

export function rgbToHex(rgb: Rgb): string {
  const part = (channel: number) =>
    Math.min(255, Math.max(0, Math.round(channel))).toString(16).padStart(2, "0").toUpperCase()
  return `#${part(rgb.r)}${part(rgb.g)}${part(rgb.b)}`
}

export function rgbToHsv(rgb: Rgb): Hsv {
  const r = clamp01(rgb.r / 255)
  const g = clamp01(rgb.g / 255)
  const b = clamp01(rgb.b / 255)
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  const delta = max - min
  let h = 0
  if (delta !== 0) {
    if (max === r) {
      h = ((g - b) / delta) % 6
    } else if (max === g) {
      h = (b - r) / delta + 2
    } else {
      h = (r - g) / delta + 4
    }
    h *= 60
    if (h < 0) {
      h += 360
    }
  }
  return { h, s: max === 0 ? 0 : delta / max, v: max }
}

export function hsvToRgb(hsv: Hsv): Rgb {
  const h = ((hsv.h % 360) + 360) % 360
  const s = clamp01(hsv.s)
  const v = clamp01(hsv.v)
  const c = v * s
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1))
  const m = v - c
  let r = 0
  let g = 0
  let b = 0
  if (h < 60) {
    r = c
    g = x
  } else if (h < 120) {
    r = x
    g = c
  } else if (h < 180) {
    g = c
    b = x
  } else if (h < 240) {
    g = x
    b = c
  } else if (h < 300) {
    r = x
    b = c
  } else {
    r = c
    b = x
  }
  return {
    r: Math.round((r + m) * 255),
    g: Math.round((g + m) * 255),
    b: Math.round((b + m) * 255),
  }
}

export function hsvToHex(hsv: Hsv): string {
  return rgbToHex(hsvToRgb(hsv))
}

export function hexToHsv(hex: string): Hsv | null {
  const rgb = hexToRgb(hex)
  return rgb ? rgbToHsv(rgb) : null
}

/** 纯黑与纯灰的色相无意义，沿用上一次的色相/饱和度，避免拖到黑色再拖回来时色相丢失 */
export function rgbToHsvKeepingHue(rgb: Rgb, previous: Hsv | null): Hsv {
  const next = rgbToHsv(rgb)
  if (!previous) {
    return next
  }
  if (next.v === 0) {
    return { h: previous.h, s: previous.s, v: 0 }
  }
  if (next.s === 0) {
    return { h: previous.h, s: 0, v: next.v }
  }
  return next
}

/* ── 色轮几何常量（CSS 像素） ── */

const WHEEL_SIZE = 184
const RING_WIDTH = 17
const OUTER_R = WHEEL_SIZE / 2 - 1
const INNER_R = OUTER_R - RING_WIDTH
/** 内接正方形：四角正好碰到内圆，四边中点留出月牙空隙，与 Photoshop 一致 */
const SQUARE_SIDE = INNER_R * Math.SQRT2
const SQUARE_LEFT = (WHEEL_SIZE - SQUARE_SIDE) / 2
const SQUARE_TOP = SQUARE_LEFT
const DEFAULT_HSV: Hsv = { h: 210, s: 0.85, v: 0.95 }

type DragMode = "ring" | "square"
type Channel = "r" | "g" | "b"

interface Draft {
  open: boolean
  field: string
  anchor: HTMLElement | null
  lastAnchorRect: DOMRect | null
  /** 打开面板时的配置值，取消时不需要还原动作，它只是判断"值有没有真的变"的基准 */
  baseValue: string
  mode: "auto" | "hex"
  hsv: Hsv
  dirty: boolean
  drag: DragMode | null
  pointerId: number | null
}

export interface ColorPickerHooks {
  tr: (key: string) => string
  getValue: (field: string) => unknown
  onCommit: (field: string, value: string) => void
}

export interface ColorPickerOptions {
  field: string
  value: string
  anchor: HTMLElement
  /** auto 状态下色轮的起始颜色，通常借用另一个颜色项的当前值 */
  seedHex?: string | null
}

const draft: Draft = {
  open: false,
  field: "",
  anchor: null,
  lastAnchorRect: null,
  baseValue: "auto",
  mode: "auto",
  hsv: { ...DEFAULT_HSV },
  dirty: false,
  drag: null,
  pointerId: null,
}

let hooks: ColorPickerHooks | null = null
let backdropEl: HTMLElement | null = null
let pickerEl: HTMLElement | null = null
let canvasEl: HTMLCanvasElement | null = null
let ctx: CanvasRenderingContext2D | null = null
let previewCurrentEl: HTMLElement | null = null
let previewOriginalEl: HTMLElement | null = null
let hexInputEl: HTMLInputElement | null = null
let rgbInputEls: Record<Channel, HTMLInputElement> | null = null
let autoButtonEl: HTMLButtonElement | null = null
let hintEl: HTMLElement | null = null

let hueRingCache: HTMLCanvasElement | null = null
let cachedDpr = 0
let renderPending = false

const RGB_CHANNELS: Channel[] = ["r", "g", "b"]

function cssEscape(value: string): string {
  return typeof CSS !== "undefined" && typeof CSS.escape === "function" ? CSS.escape(value) : value
}

/* ── 色轮绘制 ── */

function ensureCanvasSize(): void {
  if (!canvasEl || !ctx) {
    return
  }
  const dpr = window.devicePixelRatio || 1
  const pixels = Math.round(WHEEL_SIZE * dpr)
  if (canvasEl.width !== pixels || canvasEl.height !== pixels) {
    // 赋值 width/height 会重置 context 状态，之后必须重新应用缩放
    canvasEl.width = pixels
    canvasEl.height = pixels
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    hueRingCache = null
    cachedDpr = 0
  }
  if (cachedDpr !== dpr || !hueRingCache) {
    drawHueRingCache(dpr)
    cachedDpr = dpr
  }
}

/** 色相环是静态内容，只在 DPR 变化时重建 */
function drawHueRingCache(dpr: number): void {
  const cache = document.createElement("canvas")
  cache.width = Math.round(WHEEL_SIZE * dpr)
  cache.height = Math.round(WHEEL_SIZE * dpr)
  const cacheCtx = cache.getContext("2d")
  if (!cacheCtx) {
    return
  }
  cacheCtx.setTransform(dpr, 0, 0, dpr, 0, 0)

  const center = WHEEL_SIZE / 2
  const midR = (OUTER_R + INNER_R) / 2
  if (typeof cacheCtx.createConicGradient === "function") {
    const gradient = cacheCtx.createConicGradient(0, center, center)
    for (let i = 0; i <= 36; i++) {
      gradient.addColorStop(i / 36, `hsl(${i * 10} 100% 50%)`)
    }
    cacheCtx.beginPath()
    cacheCtx.arc(center, center, midR, 0, Math.PI * 2)
    cacheCtx.lineWidth = RING_WIDTH
    cacheCtx.strokeStyle = gradient
    cacheCtx.stroke()
  } else {
    for (let deg = 0; deg < 360; deg++) {
      const start = ((deg - 0.4) * Math.PI) / 180
      const end = ((deg + 1.4) * Math.PI) / 180
      cacheCtx.beginPath()
      cacheCtx.arc(center, center, midR, start, end)
      cacheCtx.lineWidth = RING_WIDTH
      cacheCtx.strokeStyle = `hsl(${deg + 0.5} 100% 50%)`
      cacheCtx.stroke()
    }
  }
  hueRingCache = cache
}

function drawSvSquare(hue: number): void {
  if (!ctx) {
    return
  }
  const left = SQUARE_LEFT
  const top = SQUARE_TOP
  const side = SQUARE_SIDE

  ctx.fillStyle = `hsl(${hue.toFixed(2)} 100% 50%)`
  ctx.fillRect(left, top, side, side)

  const saturationGradient = ctx.createLinearGradient(left, 0, left + side, 0)
  saturationGradient.addColorStop(0, "rgba(255,255,255,1)")
  saturationGradient.addColorStop(1, "rgba(255,255,255,0)")
  ctx.fillStyle = saturationGradient
  ctx.fillRect(left, top, side, side)

  const valueGradient = ctx.createLinearGradient(0, top, 0, top + side)
  valueGradient.addColorStop(0, "rgba(0,0,0,0)")
  valueGradient.addColorStop(1, "rgba(0,0,0,1)")
  ctx.fillStyle = valueGradient
  ctx.fillRect(left, top, side, side)

  ctx.strokeStyle = "rgba(128,128,128,0.45)"
  ctx.lineWidth = 1
  ctx.strokeRect(left + 0.5, top + 0.5, side - 1, side - 1)
}

/** 黑白双环标记，任何底色上都清晰 */
function drawMarker(x: number, y: number, radius: number): void {
  if (!ctx) {
    return
  }
  ctx.lineWidth = 2
  ctx.strokeStyle = "rgba(0,0,0,0.72)"
  ctx.beginPath()
  ctx.arc(x, y, radius, 0, Math.PI * 2)
  ctx.stroke()
  ctx.strokeStyle = "#ffffff"
  ctx.beginPath()
  ctx.arc(x, y, Math.max(1, radius - 1.6), 0, Math.PI * 2)
  ctx.stroke()
}

function renderWheel(): void {
  if (!ctx) {
    return
  }
  ensureCanvasSize()
  ctx.clearRect(0, 0, WHEEL_SIZE, WHEEL_SIZE)
  if (hueRingCache) {
    ctx.drawImage(hueRingCache, 0, 0, WHEEL_SIZE, WHEEL_SIZE)
  }

  const { h, s, v } = draft.hsv
  drawSvSquare(h)

  const center = WHEEL_SIZE / 2
  const angle = (h * Math.PI) / 180
  const midR = (OUTER_R + INNER_R) / 2
  drawMarker(center + Math.cos(angle) * midR, center + Math.sin(angle) * midR, 8)
  drawMarker(SQUARE_LEFT + s * SQUARE_SIDE, SQUARE_TOP + (1 - v) * SQUARE_SIDE, 6)
}

function scheduleRender(): void {
  if (renderPending) {
    return
  }
  renderPending = true
  requestAnimationFrame(() => {
    renderPending = false
    renderWheel()
  })
}

/* ── 指针拾取 ── */

function canvasPoint(event: PointerEvent): { x: number; y: number } {
  const rect = canvasEl?.getBoundingClientRect()
  if (!rect) {
    return { x: 0, y: 0 }
  }
  return { x: event.clientX - rect.left, y: event.clientY - rect.top }
}

function hitTest(x: number, y: number): DragMode | "none" {
  const dx = x - WHEEL_SIZE / 2
  const dy = y - WHEEL_SIZE / 2
  const distance = Math.hypot(dx, dy)
  if (distance > OUTER_R + 4) {
    return "none"
  }
  if (distance >= INNER_R) {
    return "ring"
  }
  return "square"
}

function pickHueFromPoint(x: number, y: number): number {
  const degrees = (Math.atan2(y - WHEEL_SIZE / 2, x - WHEEL_SIZE / 2) * 180) / Math.PI
  return (degrees + 360) % 360
}

function pickSvFromPoint(x: number, y: number): { s: number; v: number } {
  return {
    s: clamp01((x - SQUARE_LEFT) / SQUARE_SIDE),
    v: clamp01(1 - (y - SQUARE_TOP) / SQUARE_SIDE),
  }
}

function onPointerDown(event: PointerEvent): void {
  if (!canvasEl || event.button !== 0) {
    return
  }
  const point = canvasPoint(event)
  const zone = hitTest(point.x, point.y)
  if (zone === "none") {
    return
  }
  event.preventDefault()
  canvasEl.setPointerCapture(event.pointerId)
  draft.drag = zone
  draft.pointerId = event.pointerId
  if (zone === "ring") {
    setHue(pickHueFromPoint(point.x, point.y))
  } else {
    const sv = pickSvFromPoint(point.x, point.y)
    setSv(sv.s, sv.v)
  }
}

function onPointerMove(event: PointerEvent): void {
  if (!draft.drag || event.pointerId !== draft.pointerId) {
    return
  }
  const point = canvasPoint(event)
  // 进入拖拽后不再做命中测试：环模式只取角度，方块模式由 clamp 吸附到边界
  if (draft.drag === "ring") {
    setHue(pickHueFromPoint(point.x, point.y))
  } else {
    const sv = pickSvFromPoint(point.x, point.y)
    setSv(sv.s, sv.v)
  }
}

function endDrag(event: PointerEvent): void {
  if (event.pointerId !== draft.pointerId) {
    return
  }
  if (canvasEl?.hasPointerCapture(event.pointerId)) {
    canvasEl.releasePointerCapture(event.pointerId)
  }
  draft.drag = null
  draft.pointerId = null
}

/* ── 状态同步 ── */

function applyHsv(next: Hsv): void {
  draft.hsv = next
  draft.mode = "hex"
  draft.dirty = true
  syncInputs()
  updatePreview()
  updateAutoButton()
  scheduleRender()
}

function setHue(hue: number): void {
  applyHsv({ h: hue, s: draft.hsv.s, v: draft.hsv.v })
}

function setSv(saturation: number, value: number): void {
  applyHsv({ h: draft.hsv.h, s: saturation, v: value })
}

function applyRgb(rgb: Rgb): void {
  applyHsv(rgbToHsvKeepingHue(rgb, draft.hsv))
}

/** 不覆盖当前获得焦点的输入框，否则会打断用户输入并把光标顶到末尾 */
function syncInputs(): void {
  const focused = document.activeElement
  const rgb = hsvToRgb(draft.hsv)
  const auto = draft.mode === "auto"
  const hexText = auto ? "" : rgbToHex(rgb)
  if (hexInputEl && hexInputEl !== focused && hexInputEl.value !== hexText) {
    hexInputEl.value = hexText
  }
  if (rgbInputEls) {
    for (const channel of RGB_CHANNELS) {
      const input = rgbInputEls[channel]
      const text = auto ? "" : String(rgb[channel])
      if (input !== focused && input.value !== text) {
        input.value = text
      }
    }
  }
}

function paintCell(cell: HTMLElement | null, value: string): void {
  if (!cell) {
    return
  }
  const hex = normalizeHex(value)
  if (hex) {
    cell.classList.remove("auto")
    cell.style.background = hex
    cell.textContent = ""
  } else {
    cell.classList.add("auto")
    cell.style.background = ""
    cell.textContent = "AUTO"
  }
}

function updatePreview(): void {
  paintCell(previewCurrentEl, draft.mode === "auto" ? "auto" : hsvToHex(draft.hsv))
  if (hintEl && hooks) {
    hintEl.textContent = draft.mode === "auto" ? hooks.tr("color_picker_auto_hint") : hsvToHex(draft.hsv)
  }
}

function updateOriginalPreview(): void {
  paintCell(previewOriginalEl, draft.baseValue)
}

function updateAutoButton(): void {
  autoButtonEl?.classList.toggle("active", draft.mode === "auto")
}

function resetDraft(value: string, seedHex: string | null): void {
  const auto = isAutoColor(value)
  draft.mode = auto ? "auto" : "hex"
  const hex = normalizeHex(auto ? seedHex : value)
  draft.hsv = (hex ? hexToHsv(hex) : null) ?? { ...DEFAULT_HSV }
}

/* ── 输入框 ── */

function readHexInput(): string | null {
  return normalizeHex(hexInputEl?.value ?? "")
}

function applyHexInput(): void {
  const hex = readHexInput()
  if (!hex) {
    return
  }
  const hsv = hexToHsv(hex)
  if (hsv) {
    applyHsv(hsv)
  }
}

function onHexInput(): void {
  applyHexInput()
}

function onHexBlur(): void {
  if (!hexInputEl) {
    return
  }
  if (readHexInput()) {
    applyHexInput()
  } else {
    hexInputEl.value = draft.mode === "auto" ? "" : hsvToHex(draft.hsv)
  }
}

function rgbChannelOf(target: EventTarget | null): Channel | null {
  const channel = (target as HTMLElement | null)?.dataset?.colorChannel
  return channel === "r" || channel === "g" || channel === "b" ? channel : null
}

function onRgbInput(event: Event): void {
  const channel = rgbChannelOf(event.target)
  const input = event.target as HTMLInputElement
  if (!channel) {
    return
  }
  const raw = input.value.trim()
  // 中间态（空、超 3 位、越界）不应用，等 blur 时再钳制，避免打字过程被反复改写
  if (!/^\d{1,3}$/.test(raw) || Number(raw) > 255) {
    return
  }
  const rgb = hsvToRgb(draft.hsv)
  rgb[channel] = Number(raw)
  applyRgb(rgb)
}

function onRgbBlur(event: Event): void {
  const channel = rgbChannelOf(event.target)
  const input = event.target as HTMLInputElement
  if (!channel) {
    return
  }
  const current = hsvToRgb(draft.hsv)
  const raw = input.value.trim()
  const parsed = Number(raw)
  const value =
    raw !== "" && Number.isFinite(parsed) ? Math.min(255, Math.max(0, Math.round(parsed))) : current[channel]
  const rgb = hsvToRgb(draft.hsv)
  rgb[channel] = value
  applyRgb(rgb)
}

function onFieldKeydown(event: KeyboardEvent): void {
  if (event.key === "Enter") {
    event.preventDefault()
    ;(event.target as HTMLElement).blur()
  }
}

/* ── 生命周期 ── */

function layoutColorPicker(): void {
  if (!pickerEl || !draft.open) {
    return
  }
  pickerEl.style.visibility = "hidden"
  const width = pickerEl.offsetWidth
  const height = pickerEl.offsetHeight

  const rect =
    draft.anchor && document.body.contains(draft.anchor)
      ? draft.anchor.getBoundingClientRect()
      : draft.lastAnchorRect
  if (rect) {
    draft.lastAnchorRect = rect
  }
  const anchorRect = rect ?? { left: 0, top: 0, right: 0, bottom: 0 }
  const margin = 10

  let left = anchorRect.right - width
  if (left < margin) {
    left = anchorRect.left
  }
  left = Math.min(Math.max(left, margin), Math.max(margin, window.innerWidth - width - margin))

  let top = anchorRect.bottom + 6
  if (top + height > window.innerHeight - margin) {
    const above = anchorRect.top - height - 6
    top = above >= margin ? above : Math.max(margin, window.innerHeight - height - margin)
  }

  pickerEl.style.left = `${Math.round(left)}px`
  pickerEl.style.top = `${Math.round(top)}px`
  pickerEl.style.visibility = "visible"
}

export function openColorPicker(options: ColorPickerOptions): void {
  if (!pickerEl || !backdropEl || !hooks) {
    return
  }
  draft.open = true
  draft.field = options.field
  draft.anchor = options.anchor
  draft.lastAnchorRect = options.anchor.getBoundingClientRect()
  draft.baseValue = String(options.value ?? "auto")
  draft.dirty = false
  draft.drag = null
  draft.pointerId = null
  resetDraft(draft.baseValue, options.seedHex ?? null)

  backdropEl.classList.add("open")
  pickerEl.classList.add("open")
  syncInputs()
  updateAutoButton()
  updatePreview()
  updateOriginalPreview()
  layoutColorPicker()
  scheduleRender()
  pickerEl.focus()
}

export function closeColorPicker(): void {
  if (!draft.open) {
    return
  }
  draft.open = false
  draft.drag = null
  draft.pointerId = null
  backdropEl?.classList.remove("open")
  pickerEl?.classList.remove("open")
  const anchor = draft.anchor
  draft.anchor = null
  if (anchor && document.body.contains(anchor)) {
    anchor.focus()
  }
}

function confirmColorPicker(): void {
  if (!draft.open) {
    return
  }
  const next = draft.mode === "auto" ? "auto" : hsvToHex(draft.hsv)
  const field = draft.field
  // 值没变就不提交，省掉一次无谓的保存与重绘
  const changed = next !== draft.baseValue
  closeColorPicker()
  if (changed && hooks) {
    hooks.onCommit(field, next)
  }
}

/** 供 render() 之后调用：root 被整体重建，锚点与配置都可能已经变了 */
export function repositionColorPicker(): void {
  if (!draft.open || !hooks) {
    return
  }
  const next = document.querySelector(`[data-color-field="${cssEscape(draft.field)}"]`)
  if (next instanceof HTMLElement) {
    draft.anchor = next
  }
  const current = String(hooks.getValue(draft.field) ?? draft.baseValue)
  if (current !== draft.baseValue) {
    draft.baseValue = current
    if (!draft.dirty) {
      resetDraft(current, null)
      syncInputs()
      updateAutoButton()
      updatePreview()
      updateOriginalPreview()
      scheduleRender()
    }
  }
  layoutColorPicker()
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (!draft.open || event.key !== "Escape") {
    return
  }
  event.preventDefault()
  closeColorPicker()
}

function onBackdropPointerDown(event: PointerEvent): void {
  // 只认落在遮罩本身的按下，避免色轮上起拖、松手落到遮罩时误关
  if (event.target === backdropEl) {
    confirmColorPicker()
  }
}

/* ── 构建 ── */

export function initColorPicker(colorPickerHooks: ColorPickerHooks): void {
  hooks = colorPickerHooks
  if (!pickerEl) {
    buildColorPicker()
  }
  updateColorPickerText()
}

/** 语言可能在运行中切换，面板 DOM 是常驻的，需要按当前翻译重刷静态文案 */
function updateColorPickerText(): void {
  if (!pickerEl || !hooks) {
    return
  }
  if (previewCurrentEl) {
    previewCurrentEl.title = hooks.tr("color_picker_current")
  }
  if (previewOriginalEl) {
    previewOriginalEl.title = hooks.tr("color_picker_original")
  }
  if (hexInputEl) {
    hexInputEl.placeholder = hooks.tr("color_picker_auto")
  }
  if (autoButtonEl) {
    autoButtonEl.textContent = hooks.tr("color_picker_auto")
  }
  const cancelButton = pickerEl.querySelector('[data-color-action="cancel"]')
  if (cancelButton) {
    cancelButton.textContent = hooks.tr("color_picker_cancel")
  }
  const okButton = pickerEl.querySelector('[data-color-action="ok"]')
  if (okButton) {
    okButton.textContent = hooks.tr("color_picker_ok")
  }
  updatePreview()
}

function buildColorPicker(): void {
  if (!hooks) {
    return
  }

  backdropEl = document.createElement("div")
  backdropEl.className = "color-picker-backdrop"
  backdropEl.addEventListener("pointerdown", onBackdropPointerDown)

  pickerEl = document.createElement("div")
  pickerEl.className = "color-picker"
  pickerEl.tabIndex = -1
  pickerEl.setAttribute("role", "dialog")
  pickerEl.innerHTML = `
    <div class="color-picker-body">
      <canvas class="color-wheel"></canvas>
      <div class="color-picker-side">
        <div class="color-preview">
          <span class="color-preview-cell color-preview-current"></span>
          <span class="color-preview-cell color-preview-original"></span>
        </div>
        <label class="color-field">
          <span>HEX</span>
          <input class="color-hex-input" type="text" maxlength="7" spellcheck="false" autocomplete="off" />
        </label>
        ${RGB_CHANNELS.map(
          (channel) => `
        <label class="color-field">
          <span>${channel.toUpperCase()}</span>
          <input class="color-rgb-input" data-color-channel="${channel}" type="text" inputmode="numeric" maxlength="3" autocomplete="off" />
        </label>`,
        ).join("")}
        <button class="color-auto-button" data-color-action="auto"></button>
      </div>
    </div>
    <div class="color-picker-footer">
      <span class="color-picker-hint"></span>
      <button class="secondary-button" data-color-action="cancel"></button>
      <button class="primary-button" data-color-action="ok"></button>
    </div>
  `

  canvasEl = pickerEl.querySelector<HTMLCanvasElement>(".color-wheel")
  ctx = canvasEl?.getContext("2d") ?? null
  previewCurrentEl = pickerEl.querySelector(".color-preview-current")
  previewOriginalEl = pickerEl.querySelector(".color-preview-original")
  hexInputEl = pickerEl.querySelector(".color-hex-input")
  autoButtonEl = pickerEl.querySelector(".color-auto-button")
  hintEl = pickerEl.querySelector(".color-picker-hint")

  const rgbInputs = {} as Record<Channel, HTMLInputElement>
  for (const channel of RGB_CHANNELS) {
    const input = pickerEl.querySelector<HTMLInputElement>(`[data-color-channel="${channel}"]`)
    if (!input) {
      return
    }
    rgbInputs[channel] = input
    input.addEventListener("input", onRgbInput)
    input.addEventListener("change", onRgbInput)
    input.addEventListener("blur", onRgbBlur)
    input.addEventListener("keydown", onFieldKeydown)
  }
  rgbInputEls = rgbInputs

  hexInputEl?.addEventListener("input", onHexInput)
  // 应用自带的右键"粘贴"只派发 change，不派发 input
  hexInputEl?.addEventListener("change", onHexInput)
  hexInputEl?.addEventListener("blur", onHexBlur)
  hexInputEl?.addEventListener("keydown", onFieldKeydown)

  canvasEl?.addEventListener("pointerdown", onPointerDown)
  canvasEl?.addEventListener("pointermove", onPointerMove)
  canvasEl?.addEventListener("pointerup", endDrag)
  canvasEl?.addEventListener("pointercancel", endDrag)
  canvasEl?.addEventListener("lostpointercapture", () => {
    draft.drag = null
    draft.pointerId = null
  })

  autoButtonEl?.addEventListener("click", () => {
    draft.mode = draft.mode === "auto" ? "hex" : "auto"
    draft.dirty = true
    syncInputs()
    updatePreview()
    updateAutoButton()
  })

  pickerEl.querySelector('[data-color-action="cancel"]')?.addEventListener("click", closeColorPicker)
  pickerEl.querySelector('[data-color-action="ok"]')?.addEventListener("click", confirmColorPicker)

  document.body.appendChild(backdropEl)
  document.body.appendChild(pickerEl)
  document.addEventListener("keydown", onDocumentKeydown)
  // scroll 不冒泡，只能靠捕获阶段跟上内容区滚动
  document.addEventListener("scroll", onViewportChange, true)
  window.addEventListener("resize", onViewportChange)
}

function onViewportChange(): void {
  if (draft.open) {
    layoutColorPicker()
  }
}
