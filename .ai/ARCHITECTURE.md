# WinIsland Architecture

## Overview

WinIsland is a Windows desktop application that creates a Dynamic Island overlay — a translucent, always-on-top island that displays media playback info, lyrics, and audio visualization. Built entirely in Rust with Skia for GPU-accelerated rendering.

- **Window system**: winit + softbuffer
- **Rendering**: skia-safe (Skia canvas API)
- **Media integration**: Windows SMTC (System Media Transport Controls) via COM
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
│   ├── lyrics.rs      Async lyrics fetcher (NetEase, lrclib, local .lrc)
│   ├── persistence.rs Config save/load (~/.winisland/config.toml)
│   ├── render.rs      Main draw_island() — all Skia rendering lives here
│   └── smtc.rs        SMTC session manager — polls media info, handles commands
├── icons/             Custom Skia path icons (arrows, controls, music, settings)
├── ui/expanded/       Expanded island views
│   ├── music_view.rs  Music player page (album art, controls, progress)
│   └── widget_view.rs Widget/page view for additional content
├── utils/             Utilities
│   ├── animations.rs  Animation curve helpers
│   ├── autostart.rs   Registry-based auto-start
│   ├── backdrop.rs    Mica & dynamic color background effects
│   ├── blur.rs        Motion blur sigma calculation
│   ├── color.rs       Adaptive island border color from screen pixels
│   ├── font.rs        Font manager with caching
│   ├── glass.rs       Frosted glass effect (GDI capture + blur + dark overlay)
│   ├── liquid_glass.rs Advanced refractive liquid glass effect (SKSL shader)
│   ├── mouse.rs       Global cursor position, hit-test, fullscreen detection
│   ├── physics.rs     Spring physics for smooth animations
│   ├── scroll.rs      Scroll container helpers
│   ├── settings_ui/   Skia-rendered settings UI components
│   ├── updater.rs     Nightly release check + download
│   └── win32.rs       Raw Win32 API wrappers (topmost, window styles, etc.)
└── window/
    ├── app.rs         Main App struct — event loop, state, input, orchestration
    ├── tray.rs        System tray icon + context menu
    └── settings/      Separate settings window
```

---

## Rendering pipeline

The application runs on winit's **Poll** event loop in [app.rs](src/window/app.rs):

```
resumed() → create window (transparent, topmost, skip-taskbar)
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
  2. Get current MediaInfo from SMTC
  3. Get spectrum from AudioProcessor
  4. Draw background (5 styles: default, glass, mica, dynamic, liquid_glass)
  5. Draw album art (rounded/cover fit)
  6. Draw lyrics with transitions
  7. Draw spectrum visualizer bars
  8. Draw progress bar
  9. Draw mini controls (play/pause/prev/next)
  10. Read Skia surface pixels → softbuffer → present
```

Each style draws its background differently:
- **glass**: GDI screen capture → heavy blur → dark multiply blend
- **liquid_glass**: GDI screen capture → moderate blur → SKSL shader (refraction + specular)
- **mica**: AcquireDesktopImage (Win11) or GDI capture → blur → overlay
- **dynamic**: Solid color extracted from album art palette
- **default**: Solid black

---

## SMTC integration

[SMTC](src/core/smtc.rs) uses Windows `GlobalSystemMediaTransportControlsSessionManager`:

- Polls session properties every 300ms (title, artist, thumbnail, position, duration)
- On song change: triggers async lyrics fetch + thumbnail download
- Auto-allow list: known music apps are automatically registered
- Handles seek/play/pause/skip commands from the UI
- Periodically refreshes (every 30th poll ~9s) to catch new apps

---

## Windows API usage

| Area | APIs |
|------|------|
| SMTC | `GlobalSystemMediaTransportControlsSessionManager` |
| COM | `CoInitializeEx`, `CoUninitialize` |
| Audio | `IMMDeviceEnumerator`, `IAudioMeterInformation` |
| Window | `SetWindowPos` (topmost), extended styles (WS_EX_TOOLWINDOW, WS_EX_NOACTIVATE, WS_EX_LAYERED, WS_EX_TRANSPARENT) |
| GDI | `GetDC`, `CreateCompatibleDC`, `BitBlt`, `GetDIBits`, `StretchBlt` |
| DWM | `DwmEnableBlurBehindWindow` (deprecated), `DwmSetWindowAttribute` (mica) |
| IME | `ImmGetContext`, `ImmSetCompositionWindow` |
| Registry | Auto-start registration |
| Locale | `GetUserDefaultLocaleName` for language auto-detect |
| Shell | `SetCurrentProcessExplicitAppUserModelID` |

All calls are in `unsafe` blocks with detailed `// SAFETY:` comments.

---

## Configuration

Stored as TOML at `~/.winisland/config.toml`:

- Window dimensions (compact/expanded)
- Visual style (default/glass/mica/dynamic/liquid_glass)
- Language (auto/en/zh)
- SMTC settings (auto-allow, lyric sources)
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
