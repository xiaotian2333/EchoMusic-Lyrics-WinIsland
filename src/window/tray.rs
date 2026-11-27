use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::config::WINDOW_TITLE;
use crate::core::i18n::tr;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct TrayManager {
    command_tx: Sender<TrayCommand>,
    action_rx: Receiver<TrayAction>,
    tray_thread: Option<JoinHandle<()>>,
    is_light: bool,
}

enum TrayCommand {
    UpdateTheme(bool),
    UpdateVisibilityText(bool),
    ReloadTexts(bool),
    Shutdown,
}

struct TrayThreadState {
    tray: TrayIcon,
    toggle_item: MenuItem,
    settings_item: MenuItem,
    restart_item: MenuItem,
    quit_item: MenuItem,
    is_light: bool,
}

impl TrayManager {
    pub fn new(is_light: bool) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        let tray_thread = thread::spawn(move || {
            run_tray_thread(is_light, action_tx, command_rx, ready_tx);
        });

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => panic!("Failed to create tray icon: {err}"),
            Err(_) => panic!("Failed to create tray icon thread"),
        }

        Self {
            command_tx,
            action_rx,
            tray_thread: Some(tray_thread),
            is_light,
        }
    }

    pub fn update_theme(&mut self, is_light: bool) {
        if self.is_light != is_light {
            self.is_light = is_light;
            let _ = self.command_tx.send(TrayCommand::UpdateTheme(is_light));
        }
    }

    pub fn update_item_text(&self, visible: bool) {
        let _ = self
            .command_tx
            .send(TrayCommand::UpdateVisibilityText(visible));
    }

    pub fn reload_texts(&self, visible: bool) {
        let _ = self.command_tx.send(TrayCommand::ReloadTexts(visible));
    }

    pub fn try_recv_action(&self) -> Option<TrayAction> {
        self.action_rx.try_recv().ok()
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        let _ = self.command_tx.send(TrayCommand::Shutdown);
        if let Some(tray_thread) = self.tray_thread.take() {
            let _ = tray_thread.join();
        }
    }
}

impl TrayThreadState {
    fn new(is_light: bool) -> Result<Self, String> {
        let menu = Menu::new();
        let toggle_item = MenuItem::new(tr("tray_hide"), true, None);
        let settings_item = MenuItem::new(tr("tray_settings"), true, None);
        let restart_item = MenuItem::new(tr("tray_restart"), true, None);
        let quit_item = MenuItem::new(tr("tray_exit"), true, None);
        let _ = menu.append(&toggle_item);
        let _ = menu.append(&settings_item);
        let _ = menu.append(&restart_item);
        let _ = menu.append(&quit_item);

        let tray = TrayIconBuilder::new()
            .with_tooltip(WINDOW_TITLE)
            .with_menu(Box::new(menu))
            .with_icon(load_tray_icon(is_light))
            .build()
            .map_err(|err| err.to_string())?;

        Ok(Self {
            tray,
            toggle_item,
            settings_item,
            restart_item,
            quit_item,
            is_light,
        })
    }

    fn update_theme(&mut self, is_light: bool) {
        if self.is_light != is_light {
            self.is_light = is_light;
            let _ = self.tray.set_icon(Some(load_tray_icon(is_light)));
        }
    }

    fn update_item_text(&self, visible: bool) {
        if visible {
            self.toggle_item.set_text(tr("tray_hide"));
        } else {
            self.toggle_item.set_text(tr("tray_show"));
        }
    }

    fn reload_texts(&self, visible: bool) {
        self.update_item_text(visible);
        self.settings_item.set_text(tr("tray_settings"));
        self.restart_item.set_text(tr("tray_restart"));
        self.quit_item.set_text(tr("tray_exit"));
    }

    fn action_from_id(&self, id: tray_icon::menu::MenuId) -> Option<TrayAction> {
        if id == self.toggle_item.id() {
            Some(TrayAction::ToggleVisibility)
        } else if id == self.settings_item.id() {
            Some(TrayAction::OpenSettings)
        } else if id == self.restart_item.id() {
            Some(TrayAction::Restart)
        } else if id == self.quit_item.id() {
            Some(TrayAction::Exit)
        } else {
            None
        }
    }
}

fn run_tray_thread(
    is_light: bool,
    action_tx: Sender<TrayAction>,
    command_rx: Receiver<TrayCommand>,
    ready_tx: Sender<Result<(), String>>,
) {
    let mut state = match TrayThreadState::new(is_light) {
        Ok(state) => {
            let _ = ready_tx.send(Ok(()));
            state
        }
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return;
        }
    };

    loop {
        pump_tray_messages();
        forward_menu_events(&state, &action_tx);

        match handle_tray_commands(&mut state, &command_rx) {
            TrayThreadControl::Continue => {}
            TrayThreadControl::Shutdown => break,
        }

        thread::sleep(Duration::from_millis(10));
    }
}

enum TrayThreadControl {
    Continue,
    Shutdown,
}

fn handle_tray_commands(
    state: &mut TrayThreadState,
    command_rx: &Receiver<TrayCommand>,
) -> TrayThreadControl {
    loop {
        match command_rx.try_recv() {
            Ok(TrayCommand::UpdateTheme(is_light)) => state.update_theme(is_light),
            Ok(TrayCommand::UpdateVisibilityText(visible)) => state.update_item_text(visible),
            Ok(TrayCommand::ReloadTexts(visible)) => state.reload_texts(visible),
            Ok(TrayCommand::Shutdown) => return TrayThreadControl::Shutdown,
            Err(TryRecvError::Empty) => return TrayThreadControl::Continue,
            Err(TryRecvError::Disconnected) => return TrayThreadControl::Shutdown,
        }
    }
}

fn forward_menu_events(state: &TrayThreadState, action_tx: &Sender<TrayAction>) {
    while let Ok(event) = MenuEvent::receiver().try_recv() {
        if let Some(action) = state.action_from_id(event.id) {
            let _ = action_tx.send(action);
        }
    }
}

#[cfg(windows)]
fn pump_tray_messages() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    // SAFETY: 只在托盘线程泵取当前线程消息队列；MSG 是本地栈变量，调用期间有效。
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn pump_tray_messages() {}

fn load_tray_icon(is_light: bool) -> Icon {
    let icon_bytes: &[u8] = if is_light {
        include_bytes!("../../resources/icon-dark.png")
    } else {
        include_bytes!("../../resources/icon.png")
    };
    let image = image::load_from_memory(icon_bytes).expect("Failed to load icon from resources");
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let rgba_vec = rgba.into_raw();
    Icon::from_rgba(rgba_vec, width, height).expect("Failed to create tray icon from RGBA data")
}

pub enum TrayAction {
    ToggleVisibility,
    OpenSettings,
    Restart,
    Exit,
}
