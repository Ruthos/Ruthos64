//! macOS: files from Finder and the native menu bar.
//!
//! Finder's "Open with", a double-click on a `.dcm` file and a drop on the
//! Dock icon reach a running application as `application:openURLs:` on
//! the application delegate, an Apple Event that winit does not surface.
//! winit installs its own delegate, so the method is added to that
//! delegate's class at runtime and forwards the paths to the open queue.
//!
//! The menu bar is built from `super::MENU_BAR` with muda, with the
//! standard application menu in front, and its events are polled once per
//! frame.
//!
//! This file is compiled and tested on the macOS runner of the CI matrix.

use std::ffi::CStr;
use std::path::PathBuf;

use muda::accelerator::Accelerator;
use muda::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSArray, NSURL};

use super::{Entry, MENU_BAR};

pub fn install() {
    install_open_hook();
    install_menu_bar();
}

/// `application:openURLs:` as AppKit calls it.
unsafe extern "C-unwind" fn open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _application: *mut AnyObject,
    urls: *mut NSArray<NSURL>,
) {
    if urls.is_null() {
        return;
    }
    let urls = &*urls;
    let mut paths = Vec::new();
    for i in 0..urls.count() {
        let url = urls.objectAtIndex(i);
        if let Some(path) = url.path() {
            paths.push(PathBuf::from(path.to_string()));
        }
    }
    super::queue_open_files(paths);
}

fn install_open_hook() {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("open hook not installed: not on the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let delegate: *mut AnyObject = unsafe { msg_send![&*app, delegate] };
    if delegate.is_null() {
        tracing::warn!("open hook not installed: the application has no delegate yet");
        return;
    }
    let class = unsafe { (*delegate).class() };
    let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut NSArray<NSURL>) = open_urls;
    // Type encoding: void return, self, _cmd, two objects.
    let types: &CStr = c"v@:@@";
    let added = unsafe {
        objc2::ffi::class_addMethod(
            class.as_ptr(),
            sel!(application:openURLs:).as_ptr(),
            Some(std::mem::transmute::<
                unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut NSArray<NSURL>),
                unsafe extern "C-unwind" fn(),
            >(imp)),
            types.as_ptr(),
        )
    };
    if added as u8 != 0 {
        tracing::info!("Finder open events forwarded to the import queue");
    } else {
        tracing::warn!("open hook not installed: the delegate already handles application:openURLs:");
    }
}

fn accel(canonical: &str) -> Option<Accelerator> {
    // muda reads "CmdOrCtrl+S"; the definitions are written the Windows way.
    let text = canonical.replace("Ctrl+", "CmdOrCtrl+").replace("Alt+", "Option+");
    text.parse().ok()
}

fn install_menu_bar() {
    let menu = Menu::new();
    let app_menu = Submenu::new("Ruthos", true);
    let about = AboutMetadata {
        name: Some("Ruthos".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: Some("The Ruthos contributors, LGPL-3.0".into()),
        ..Default::default()
    };
    let _ = app_menu.append_items(&[
        &PredefinedMenuItem::about(Some("About Ruthos"), Some(about)),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("app:preferences", "Preferences…", true, accel("Ctrl+,")),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ]);
    let _ = menu.append(&app_menu);

    for def in MENU_BAR {
        let sub = Submenu::new(def.title, true);
        for entry in def.entries {
            match entry {
                Entry::Separator => {
                    let _ = sub.append(&PredefinedMenuItem::separator());
                }
                // Preferences already sits in the application menu.
                Entry::Item(it) if it.id == "app:preferences" => {}
                Entry::Item(it) => {
                    let _ = sub.append(&MenuItem::with_id(it.id, it.label, true, it.shortcut.and_then(accel)));
                }
            }
        }
        if def.title == "File" {
            // Drop a trailing separator left by the Preferences item.
            let _ = sub.remove_at(sub.items().len().saturating_sub(1));
        }
        if def.title == "Window" {
            let _ = sub.append_items(&[&PredefinedMenuItem::separator(), &PredefinedMenuItem::minimize(None)]);
            sub.set_as_windows_menu_for_nsapp();
        }
        if def.title == "Help" {
            sub.set_as_help_menu_for_nsapp();
        }
        let _ = menu.append(&sub);
    }
    menu.init_for_nsapp();
    // NSApp holds the menu; keep the Rust side alive for the process too.
    std::mem::forget(menu);
}

/// Ids of the menu items chosen since the last call.
pub fn poll_menu_events() -> Vec<String> {
    let rx = MenuEvent::receiver();
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event.id().0.clone());
    }
    out
}
