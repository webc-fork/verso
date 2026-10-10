//! Standalone tabbed browsing.
//!
//! When Verso runs without an IPC controller and the panel is enabled through
//! the `--with-panel` command line flag, the shell hosts multiple [`WebView`]s
//! that share one [`RenderingContext`](servo::RenderingContext). Servo anchors
//! every [`WebView`] at the origin of its rendering context (there is no
//! per-webview offset yet, see servo#36232), so exactly one tab is visible at
//! a time: tab switching shows the new tab and hides the previous one, which
//! is the same mechanism servoshell uses. Since no visible tab strip can
//! coexist with page content, tabs are managed through keyboard shortcuts and
//! the active tab is reflected in the window title:
//!
//! - `Cmd/Ctrl+T`: open a new tab
//! - `Cmd/Ctrl+W`: close the active tab (closing the last one exits Verso)
//! - `Ctrl+Tab` / `Ctrl+Shift+Tab`: cycle through the tabs

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use euclid::Scale;
use log::info;
use servo::{WebView, WebViewBuilder, WebViewId};
use url::Url;

use crate::{Shell, ShellHandle, WakeEvent};

/// The state of the standalone tabbed-browsing panel.
pub(crate) struct TabState {
    /// The open tabs, in creation order.
    tabs: Vec<Tab>,
    /// The index of the visible tab.
    active: usize,
}

/// One open tab: a [`WebView`] plus cached metadata for the window title.
struct Tab {
    id: WebViewId,
    webview: WebView,
    title: Option<String>,
    url: Option<Url>,
}

impl TabState {
    /// Wrap the initial webview in a new tab state.
    pub(crate) fn new(webview: WebView) -> Self {
        let id = webview.id();
        Self {
            tabs: vec![Tab {
                id,
                webview,
                title: None,
                url: None,
            }],
            active: 0,
        }
    }

    /// Handle a keyboard event as a tab shortcut, returning whether it was
    /// consumed.
    pub(crate) fn handle_shortcut(shell: &ShellHandle, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};

        let mut tabs_guard = shell.tabs.borrow_mut();
        let Some(tabs) = tabs_guard.as_mut() else {
            return false;
        };
        if event.state != ElementState::Pressed {
            return false;
        }
        let modifiers = shell.modifiers.get();
        let ctrl_or_cmd = modifiers.control_key() || modifiers.super_key();
        match &event.logical_key {
            Key::Character(character) if ctrl_or_cmd && character.eq_ignore_ascii_case("t") => {
                tabs.open_new_tab(shell);
                true
            }
            Key::Character(character) if ctrl_or_cmd && character.eq_ignore_ascii_case("w") => {
                if !tabs.close_active(shell) {
                    // The last tab was closed; shut the shell down.
                    *tabs_guard = None;
                    let _ = shell.proxy.send_event(WakeEvent::ExitRequested);
                }
                true
            }
            Key::Named(NamedKey::Tab) if modifiers.control_key() => {
                tabs.cycle(shell, if modifiers.shift_key() { -1 } else { 1 });
                true
            }
            _ => false,
        }
    }

    /// Cache the title of a tab, returning the new window title if the tab is
    /// the active one.
    pub(crate) fn set_title(&mut self, id: WebViewId, title: Option<String>) -> Option<String> {
        let tab = self.tabs.iter_mut().find(|tab| tab.id == id)?;
        tab.title = title.clone();
        if self.tabs[self.active].id == id {
            Some(title.unwrap_or_else(|| "Verso".to_string()))
        } else {
            None
        }
    }

    /// Cache the URL of a tab.
    pub(crate) fn set_url(&mut self, id: WebViewId, url: Url) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.url = Some(url);
        }
    }

    /// Open a new tab loading the default URL and make it active.
    fn open_new_tab(&mut self, shell: &ShellHandle) {
        info!("Opening a new tab");
        let webview = build_webview(shell, shell.default_webview_url());
        self.adopt(shell, webview);
    }

    /// Make a freshly built webview a new active tab.
    pub(crate) fn adopt(&mut self, shell: &ShellHandle, webview: WebView) {
        let id = webview.id();
        self.tabs.push(Tab {
            id,
            webview,
            title: None,
            url: None,
        });
        self.activate(shell, self.tabs.len() - 1);
    }

    /// Close the active tab. Returns `false` if this was the last tab, in
    /// which case the tab state is empty and the caller should exit.
    fn close_active(&mut self, shell: &Shell) -> bool {
        self.close(shell, self.active)
    }

    /// Close the tab with the given id, e.g. because the page closed itself.
    /// Returns `false` if this was the last tab.
    pub(crate) fn close_by_id(&mut self, shell: &Shell, id: WebViewId) -> bool {
        match self.tabs.iter().position(|tab| tab.id == id) {
            Some(index) => self.close(shell, index),
            None => true,
        }
    }

    /// Close the tab at the given index. Returns `false` if this was the last
    /// tab.
    fn close(&mut self, shell: &Shell, index: usize) -> bool {
        if self.tabs.len() == 1 {
            // Dropping the webview closes it with Servo.
            self.tabs.clear();
            *shell.webview.borrow_mut() = None;
            return false;
        }
        let was_active = index == self.active;
        // Dropping the removed tab closes its webview with Servo.
        self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        // Only switch tabs if the visible tab was closed.
        if was_active {
            self.activate(shell, self.active.min(self.tabs.len() - 1));
        }
        true
    }

    /// Cycle through the tabs by `offset` positions.
    fn cycle(&mut self, shell: &Shell, offset: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let index = (self.active as isize + offset).rem_euclid(self.tabs.len() as isize) as usize;
        self.activate(shell, index);
    }

    /// Show the tab at the given index and hide all the others.
    fn activate(&mut self, shell: &Shell, index: usize) {
        let Some(active) = self.tabs.get(index) else {
            return;
        };
        self.active = index;
        for tab in &self.tabs {
            if tab.id == active.id {
                tab.webview.show();
                tab.webview.set_focused(true);
                tab.webview.resize(shell.window.inner_size());
                tab.webview
                    .set_hidpi_scale_factor(Scale::new(shell.window.scale_factor() as f32));
            } else {
                tab.webview.hide();
                tab.webview.set_focused(false);
            }
        }
        *shell.webview.borrow_mut() = Some(active.webview.clone());
        shell
            .window
            .set_title(active.title.as_deref().unwrap_or("Verso"));
        shell.window.request_redraw();
    }
}

/// Build a tab webview that shares the shell's rendering context.
fn build_webview(shell: &ShellHandle, url: Url) -> WebView {
    WebViewBuilder::new(&shell.servo, shell.rendering_context.borrow().clone())
        .url(url)
        .hidpi_scale_factor(Scale::new(shell.window.scale_factor() as f32))
        .delegate(shell.clone())
        .build()
}

/// Upgrade the shell's weak self handle, if the shell is still alive.
pub(crate) fn shell_handle(shell: &Shell) -> Option<ShellHandle> {
    shell.self_handle.borrow().upgrade()
}

/// Keep a weak handle to the shell in the shell itself, so delegate callbacks
/// (which only receive `&Shell`) can obtain an [`Rc<Shell>`](Rc) handle for
/// building further webviews. A strong handle would leak the shell.
pub(crate) fn store_self_handle(shell: &ShellHandle) {
    *shell.self_handle.borrow_mut() = Rc::downgrade(shell);
}

/// The type of the shell's weak self handle.
pub(crate) type SelfHandle = RefCell<Weak<Shell>>;
