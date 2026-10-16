//! AppKit's Dock menu, using the same native menu event dispatcher as Tauri.
use crate::navigation::{open_view, AppView};
use muda::{ContextMenu, Menu, MenuItem};
use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};
use std::cell::RefCell;

thread_local! {
    // AppKit borrows this menu. Keep it and its item targets alive on the main thread.
    static DOCK_MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

unsafe extern "C-unwind" fn application_dock_menu(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut AnyObject,
) -> *mut AnyObject {
    DOCK_MENU.with(|menu| {
        menu.borrow()
            .as_ref()
            .map_or(std::ptr::null_mut(), |menu| menu.ns_menu().cast())
    })
}

pub(crate) fn install(app: &tauri::App) -> Result<(), String> {
    let menu = Menu::with_items(&[
        &MenuItem::with_id("dock.download", "Download", true, None),
        &MenuItem::with_id("dock.history", "History", true, None),
        &MenuItem::with_id("dock.browser-import", "Browser Import", true, None),
        &MenuItem::with_id("dock.settings", "Settings", true, None),
    ])
    .map_err(|err| err.to_string())?;

    // SAFETY: Tauri setup runs on AppKit's main thread. Extend the existing
    // delegate with this optional public callback, preserving all Tao handlers.
    // Its Objective-C signature is NSMenu *(id, SEL, NSApplication *).
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![application, delegate];
        let delegate = delegate
            .as_ref()
            .ok_or("Application delegate unavailable")?;
        let implementation: Imp = std::mem::transmute(
            application_dock_menu
                as unsafe extern "C-unwind" fn(
                    *mut AnyObject,
                    Sel,
                    *mut AnyObject,
                ) -> *mut AnyObject,
        );
        let added = objc2::ffi::class_addMethod(
            (delegate.class() as *const objc2::runtime::AnyClass).cast_mut(),
            sel!(applicationDockMenu:),
            implementation,
            c"@@:@".as_ptr(),
        );
        if !added.as_bool() {
            return Err("Application delegate already provides a Dock menu".into());
        }
        DOCK_MENU.with(|stored| *stored.borrow_mut() = Some(menu));
        // Refresh AppKit's optional delegate-method cache without replacing it.
        let _: () = msg_send![application, setDelegate: delegate];
    }

    app.on_menu_event(|app, event| {
        let view = match event.id().as_ref() {
            "dock.download" => AppView::Download,
            "dock.history" => AppView::History,
            "dock.browser-import" => AppView::LinkDump,
            "dock.settings" => AppView::Settings,
            _ => return,
        };
        if let Err(err) = open_view(app, view) {
            eprintln!("Dock navigation failed: {err}");
        }
    });
    Ok(())
}
