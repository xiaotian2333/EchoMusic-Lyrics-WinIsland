# EchoMusic-Lyrics-WinIsland Architecture

## Overview

EchoMusic-Lyrics-WinIsland is a Windows desktop application that creates a Dynamic Island overlay — a translucent, always-on-top island that displays media playback info, lyrics, and audio visualization. Built entirely in Rust with Skia for GPU-accelerated rendering.

- **Window system**: winit + softbuffer (主窗口), Tauri v2 Web (设置窗口)
- **Rendering**: skia-safe (Skia canvas API)
- **Media integration**: WebSocket 桥接 (替代 Windows SMTC)
- **Audio visualization**: cpal (loopback capture) + realfft (6-band spectrum)
- **Language**: English & Chinese (i18n via custom .lang files)

---

## Directory structure

```
src/
├── core/              Core business logic
│   ├── audio.rs       Audio loopback capture + FFT spectrum
│   ├── config.rs      AppConfig struct and defaults
│   ├── i18n.rs        Translation system (key-value .lang files)
│   ├── lyrics.rs      MusicData payload parser & lyric index helpers
│   ├── lyrics_ws.rs   WebSocket server (127.0.0.1:17195) for EchoMusic plugin
│   ├── media_info.rs  MediaInfo struct — current track, position, lyrics, cover
│   ├── persistence.rs Config save/load (~/.echomusic-lyrics-winisland/config.toml)
│   ├── render.rs      Main draw_island() — all Skia rendering lives here
│   └── ws_media.rs    WsMediaListener — WS event loop, state management, play control
├── icons/             Custom Skia path icons (arrows, controls, music, settings)
│   ├── arrows.rs
│   ├── controls.rs
│   ├── music.rs
│   └── settings.rs
├── ui/expanded/       Expanded island views
│   ├── music_view.rs  Music player page (album art, controls, progress)
│   └── widget_view.rs Widget/page view for additional content
├── utils/             Utilities
│   ├── autostart.rs   Registry-based auto-start
│   ├── backdrop.rs    Mica & dynamic color background effects
│   ├── blur.rs        Motion blur sigma calculation
│   ├── color.rs       Adaptive island border color from screen pixels
│   ├── font.rs        Font manager with caching
│   ├── glass.rs       Frosted glass effect (GDI capture + blur + dark overlay)
│   ├── icon.rs        Helper for loading/rendering icon bitmaps
│   ├── liquid_glass.rs Refractive liquid glass (WinIsland liquid.sksl + frost blur)
│   ├── mouse.rs       Global cursor position, hit-test, fullscreen detection
│   ├── physics.rs     Spring physics for smooth animations
│   ├── process.rs     Process discovery & external app detection
│   ├── scroll.rs      Scroll container helpers
│   ├── updater.rs     Nightly release check + download
│   └── win32.rs       Raw Win32 API wrappers (topmost, window styles, etc.)
├── gpu/               WinIsland-style GPU pipeline (D3D12 + DirectComposition + WGC)
│   ├── mod.rs         GpuIsland orchestration, wake proxy, shared types
│   ├── device.rs      D3D12 device + Skia Ganesh D3D12 context
│   ├── target.rs      Composition swap chain render target
│   ├── backdrop.rs    Shared texture + GPU frost pipeline
│   └── capture.rs     Windows.Graphics.Capture screen capture
├── crates/host-backdrop/  WinIsland frosted-glass host backdrop (separate crate)
│   ├── lib.rs         DWM host backdrop brush + DirectComposition visual tree
│   ├── blur.rs        D2D1 GaussianBlur IGraphicsEffectD2D1Interop implementation
│   └── styles.rs      DWMWA_USE_HOSTBACKDROPBRUSH / acrylic fallback
└── window/
    ├── app.rs         Main App struct — event loop, state, input, orchestration
    ├── tray.rs        System tray icon + context menu
    └── settings/      Separate settings window (Tauri v2 Web)
```

---

## Rendering pipeline

The application runs on winit's **Poll** event loop in [app.rs](src/window/app.rs):

```
resumed() → create window (transparent, topmost, skip-taskbar)
           → create hidden companion backdrop window (no redirection bitmap)
           → create softbuffer surface
           → create Skia thread-local surface

about_to_wait() [every frame ~144 FPS]:
  1. Enforce topmost position
  2. Handle tray events
  3. Check config changes (every 30 frames)
  4. Update cursor hit-test & auto-hide state
  5. Update seeking, borders, lyrics transitions
  6. Compute spring targets, update all springs
  7. Request redraw if animating
  8. Sleep to maintain 144 FPS (~6944 µs)

RedrawRequested → draw_island():
  1. Compute dt, motion blur sigmas
  2. Get current MediaInfo from WebSocket bridge
  3. Get spectrum from AudioProcessor
  4. Draw background (5 styles: default, glass, mica, dynamic, liquid_glass)
  5. Draw album art (rounded/cover fit)
  6. Draw lyrics with transitions
  7. Draw spectrum visualizer bars
  8. Draw progress bar
  9. Draw mini controls (play/pause/prev/next)
  10. Read Skia surface pixels → softbuffer → present (liquid_glass with an active
     GPU pipeline draws on the DirectComposition swap chain instead, bypassing softbuffer)
```

Each style draws its background differently:
- **glass**: WinIsland original host backdrop — a companion window with the DWM host
  backdrop brush (`DWMWA_USE_HOSTBACKDROPBRUSH`) composites the real desktop content
  below the island through DirectComposition, clipped to the island's rounded rect with
  an optional D2D Gaussian blur (`glass_blur`); the island itself only paints a
  translucent dark overlay. Falls back to the CPU path (GDI capture → heavy blur →
  dark multiply blend) when the host backdrop is unavailable
- **liquid_glass**: GPU path (WinIsland original) — D3D12 + DirectComposition swap chain, Windows.Graphics.Capture backdrop via shared texture/fences, GPU frost + liquid.sksl refraction; falls back to CPU path (GDI capture → frost → liquid.sksl) when GPU or capture is unavailable
- **mica**: GDI screen capture (downscaled) → blur → dark overlay
- **dynamic**: Solid color extracted from album art palette
- **default**: Solid black

---

## Media integration (WebSocket bridge)

The project no longer uses Windows SMTC directly. Instead, media state is obtained through a WebSocket bridge:

1. **`src/core/lyrics_ws.rs`** starts a WebSocket server on `127.0.0.1:17195`.
2. The EchoMusic plugin (via [Lyrics-bridge](https://github.com/xiaotian2333/Lyrics-bridge)) connects as a client and pushes `MusicData`, `PlaybackState`, and playback action events.
3. **`src/core/ws_media.rs`** runs `ws_media_loop()` which:
   - Receives `MusicData` → extracts metadata (title, artist, cover), lyrics, and merges into `MediaInfo`
   - Receives `PlaybackState` → updates position/duration/is_playing
   - Receives playback actions → updates state accordingly
   - Sends seek/play/pause/next/prev commands back to the plugin
   - Periodically (every 2s) requests playback state refresh via `get_playback_state()`
4. **`src/core/media_info.rs`** defines the `MediaInfo` struct used throughout the UI layer.
5. **`src/core/lyrics.rs`** parses the `MusicData` JSON payload (including Base64 cover decoding, lyric line sorting, character-level timestamps).

---

## Windows API usage

| Area | APIs |
|------|------|
| COM | `CoInitializeEx`, `CoUninitialize` |
| Audio | `IMMDeviceEnumerator`, `IAudioMeterInformation` |
| Window | `SetWindowPos` (topmost), extended styles (WS_EX_TOOLWINDOW, WS_EX_NOACTIVATE) |
| GDI | `GetDC`, `CreateCompatibleDC`, `BitBlt`, `GetDIBits`, `StretchBlt` |
| DWM | `DwmSetWindowAttribute` (mica) |
| Registry | Auto-start registration |
| Locale | `GetUserDefaultLocaleName` for language auto-detect |
| Shell | `SetCurrentProcessExplicitAppUserModelID` |

All calls are in `unsafe` blocks with detailed `// SAFETY:` comments.

---

## Configuration

Stored as TOML at `~/.echomusic-lyrics-winisland/config.toml`:

- Window dimensions (compact/expanded)
- Visual style (default/glass/mica/dynamic/liquid_glass)
- Language (auto/en/zh)
- Lyric sources & WebSocket config
- Audio visualization (gate threshold)
- Auto-hide and auto-start behavior

---

## Build & test

```bash
# Development
cargo check                           # Fast type-checking
cargo clippy --workspace -- -D warnings  # Lint (warnings are errors)
cargo fmt --all                       # Format

# Release
cargo build --release                 # Production build (LTO, abort on panic)

# Test
cargo test                            # Run all tests
```

Build requirements: Windows SDK, LLVM/clang (via Visual Studio or `choco install llvm ninja`).
