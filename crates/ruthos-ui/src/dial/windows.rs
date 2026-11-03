//! The dial on Windows, through RadialController.
//!
//! A desktop application gets a controller for one of its windows from
//! `IRadialControllerInterop::CreateForWindow`, which needs the window's
//! handle. Ruthos's viewers are egui deferred viewports whose handles eframe
//! does not expose, so controllers are made lazily: each frame the
//! foreground window is checked, and if it belongs to this process and has
//! no controller yet, one is made for it. Windows that have gone are
//! dropped on the same pass. Events go into a channel that the focused
//! viewer drains.

use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use windows::core::HSTRING;
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::Radial::{IRadialControllerConfigurationInterop, IRadialControllerInterop};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsWindow};
use windows::UI::Input::{
    RadialController, RadialControllerButtonClickedEventArgs, RadialControllerConfiguration, RadialControllerMenuItem,
    RadialControllerRotationChangedEventArgs, RadialControllerSystemMenuItemKind,
};

use super::{DialEvent, DialMode, DialSettings};

struct Registered {
    hwnd: isize,
    /// Kept alive: dropping the controller stops its events.
    _controller: RadialController,
    _items: Vec<RadialControllerMenuItem>,
}

/// The dial controllers of Ruthos's windows and the events they send.
pub struct DialHost {
    registered: Vec<Registered>,
    tx: Sender<(Instant, DialEvent)>,
    rx: Receiver<(Instant, DialEvent)>,
    /// The settings the controllers were made with; a change remakes them.
    configured: Option<DialSettings>,
    /// Windows refused a controller for these windows (no dial support).
    refused: Vec<isize>,
    pub last_error: Mutex<Option<String>>,
}

impl Default for DialHost {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            registered: Vec::new(),
            tx,
            rx,
            configured: None,
            refused: Vec::new(),
            last_error: Mutex::new(None),
        }
    }
}

/// The icon font: Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on 10.
fn icon_font() -> &'static str {
    let fonts = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");
    if fonts.join("SegoeIcons.ttf").is_file() {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

/// A glyph from the Segoe icon fonts for each mode.
fn glyph(mode: DialMode) -> &'static str {
    match mode {
        DialMode::Navigate => "\u{E8B9}",           // slideshow
        DialMode::WindowWidth => "\u{E740}",        // full screen (spread)
        DialMode::WindowLevel => "\u{E706}",        // brightness
        DialMode::MeasurementPoint => "\u{ED5E}",   // ruler
        DialMode::LoupeMagnification => "\u{E71E}", // zoom
    }
}

impl DialHost {
    /// Why Windows refused a controller, if it did.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    /// Events since the last call, oldest first.
    pub fn drain(&self) -> Vec<(Instant, DialEvent)> {
        self.rx.try_iter().collect()
    }

    /// Keep a controller on the foreground Ruthos window, in step with the
    /// settings. Two system calls a frame when nothing changes.
    pub fn pump(&mut self, ctx: &egui::Context, settings: &DialSettings) {
        if !settings.enabled {
            self.registered.clear();
            self.configured = None;
            return;
        }
        if self.configured.as_ref() != Some(settings) {
            // New modes or detent: remake every controller.
            self.registered.clear();
            self.refused.clear();
            self.configured = Some(settings.clone());
        }
        // Drop the controllers of closed windows.
        self.registered
            .retain(|r| unsafe { IsWindow(Some(HWND(r.hwnd as *mut core::ffi::c_void))) }.as_bool());

        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return;
        }
        let key = hwnd.0 as isize;
        if self.registered.iter().any(|r| r.hwnd == key) || self.refused.contains(&key) {
            return;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != std::process::id() {
            return;
        }
        match self.register(hwnd, ctx, settings) {
            Ok(r) => {
                tracing::info!("dial: controller made for window {key:#x}");
                self.registered.push(r);
            }
            Err(e) => {
                tracing::info!("dial: no controller for window {key:#x}: {e}");
                *self.last_error.lock() = Some(e.message().to_string());
                self.refused.push(key);
            }
        }
    }

    fn register(&self, hwnd: HWND, ctx: &egui::Context, settings: &DialSettings) -> windows::core::Result<Registered> {
        let interop = windows::core::factory::<RadialController, IRadialControllerInterop>()?;
        let controller: RadialController = unsafe { interop.CreateForWindow(hwnd)? };
        controller.SetRotationResolutionInDegrees(settings.degrees_per_step.max(1) as f64)?;
        controller.SetUseAutomaticHapticFeedback(true)?;

        // Ruthos's modes in the dial's menu, and Windows' own items out of it.
        let font = HSTRING::from(icon_font());
        let menu = controller.Menu()?;
        let mut items = Vec::new();
        for mode in DialMode::ALL.into_iter().filter(|m| settings.modes.contains(m)) {
            let item = RadialControllerMenuItem::CreateFromFontGlyph(
                &HSTRING::from(mode.label()),
                &HSTRING::from(glyph(mode)),
                &font,
            )?;
            let (tx, c) = (self.tx.clone(), ctx.clone());
            item.Invoked(&TypedEventHandler::<
                RadialControllerMenuItem,
                windows::core::IInspectable,
            >::new(move |_, _| {
                let _ = tx.send((Instant::now(), DialEvent::Choose(mode)));
                c.request_repaint();
                Ok(())
            }))?;
            menu.Items()?.Append(&item)?;
            items.push(item);
        }
        let config_interop =
            windows::core::factory::<RadialControllerConfiguration, IRadialControllerConfigurationInterop>()?;
        let config: RadialControllerConfiguration = unsafe { config_interop.GetForWindow(hwnd)? };
        let none: windows_collections::IIterable<RadialControllerSystemMenuItemKind> = Vec::new().into();
        config.SetDefaultMenuItems(&none)?;

        let (tx, c) = (self.tx.clone(), ctx.clone());
        controller.RotationChanged(&TypedEventHandler::<
            RadialController,
            RadialControllerRotationChangedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                let _ = tx.send((Instant::now(), DialEvent::Rotate(args.RotationDeltaInDegrees()?)));
                c.request_repaint();
            }
            Ok(())
        }))?;
        let (tx, c) = (self.tx.clone(), ctx.clone());
        controller.ButtonClicked(&TypedEventHandler::<
            RadialController,
            RadialControllerButtonClickedEventArgs,
        >::new(move |_, _| {
            let _ = tx.send((Instant::now(), DialEvent::Press));
            c.request_repaint();
            Ok(())
        }))?;
        Ok(Registered {
            hwnd: hwnd.0 as isize,
            _controller: controller,
            _items: items,
        })
    }
}
