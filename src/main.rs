// Prevent console window from appearing on Windows
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The versoview browser shell.
//!
//! Creates a winit window, drives a Servo [`WebView`] inside it and handles
//! the IPC protocol that the `verso` controller library speaks.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Mutex,
};

use crossbeam_channel::Receiver;
use euclid::Scale;
use ipc_channel::ipc::{self, IpcSender};
use log::{error, info, warn};
use servo::{
    Cursor, EmbedderControlId, InputEvent, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, NavigationRequest, Opts, Preferences, Servo,
    ServoBuilder, WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder, WebViewDelegate,
    protocol_handler::ProtocolRegistry,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use servo::{ContextMenuAction, ContextMenuItem};
use url::Url;
use versoview::{
    Result,
    config::{self, Config},
};
use versoview_messages::{ToControllerMessage, ToVersoMessage, WebResourceRequest};
use webrender_api::units::DevicePoint;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    raw_window_handle::{HasDisplayHandle, HasWindowHandle},
    window::Window,
};

mod keyutils;
mod resources;
mod tabs;

/// Messages that wake up the winit event loop.
#[derive(Debug)]
enum WakeEvent {
    /// Pump the Servo event loop.
    Wake,
    /// A message from the controller arrived.
    IpcMessage(Box<ToVersoMessage>),
    /// Shut the shell down gracefully.
    ExitRequested,
}

/// Implements Servo's waker on top of a winit event loop proxy.
#[derive(Clone)]
struct Waker(EventLoopProxy<WakeEvent>);

impl servo::EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.0.send_event(WakeEvent::Wake);
    }
}

/// Listeners registered by the controller through `Listen*` IPC messages.
#[derive(Default)]
struct Listeners {
    close_requested: bool,
    navigation_starting: bool,
    web_resource_requests: bool,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
type PendingContextMenu = Option<(servo::ContextMenu, Vec<(String, ContextMenuAction)>)>;

/// A panel is a special web view that focuses on controlling states around window.
/// It serves as the control panel or navigation bar of the window.
pub struct Panel {
    /// The panel's child window
    pub window: Rc<Window>,
    /// The panel's dedicated rendering context
    pub rendering_context: Rc<dyn servo::RenderingContext>,
    /// The panel's webview
    pub webview: WebView,
    /// The URL of the panel component
    pub initial_url: Url,
}

/// Everything shared between the winit `ApplicationHandler` and the Servo
/// `WebViewDelegate`. Servo's delegates receive `&self`, so interior
/// mutability lives behind locks and cells here.
struct Shell {
    window: Rc<Window>,
    content_window: Rc<Window>,
    proxy: EventLoopProxy<WakeEvent>,
    servo: Servo,
    webview: RefCell<Option<WebView>>,
    rendering_context: RefCell<Rc<dyn servo::RenderingContext>>,
    config: Config,
    controller: Mutex<Option<IpcSender<ToControllerMessage>>>,
    listeners: Mutex<Listeners>,
    /// The position of the mouse cursor in device pixels, tracked through
    /// `CursorMoved` events.
    mouse_position: Cell<Option<DevicePoint>>,
    /// The position of the mouse cursor inside the panel child window.
    panel_mouse_position: Cell<Option<DevicePoint>>,
    /// Keyboard modifiers state, tracked through `ModifiersChanged` events.
    modifiers: Cell<winit::keyboard::ModifiersState>,
    /// Navigation requests waiting for the controller's
    /// [`ToVersoMessage::OnNavigationStartingResponse`], keyed by the
    /// serialized identifier sent in
    /// [`ToControllerMessage::OnNavigationStarting`].
    pending_navigations: Mutex<HashMap<Vec<u8>, NavigationRequest>>,
    /// Web resource loads waiting for the controller's
    /// [`ToVersoMessage::WebResourceRequestResponse`], keyed by request id.
    pending_web_resources: Mutex<HashMap<uuid::Uuid, WebResourceLoad>>,
    /// WebDriver commands arriving from the WebDriver HTTP server.
    webdriver_receiver: Mutex<Option<Receiver<servo::WebDriverCommandMsg>>>,
    /// The id of the input method control that is currently shown, if any.
    visible_input_method: Mutex<Option<EmbedderControlId>>,
    /// The native context menu that is currently shown and the menu item ids
    /// mapped to their Servo actions (macOS and Windows only).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pending_context_menu: Mutex<PendingContextMenu>,
    /// Whether the cursor is currently inside the edge resize zone of an
    /// undecorated window (Linux and Windows only).
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    in_edge_resize_zone: Cell<bool>,
    /// The state of the standalone tabbed-browsing panel, enabled when running
    /// without a controller and `--with-panel` was passed.
    tabs: RefCell<Option<tabs::TabState>>,
    /// A weak handle to this shell, used by delegate callbacks to build
    /// further webviews for the tabbed-browsing panel.
    self_handle: tabs::SelfHandle,
    /// The browser control panel / navigation bar.
    panel: RefCell<Option<Panel>>,
}

type ShellHandle = Rc<Shell>;

impl Shell {
    fn send_to_controller(&self, message: ToControllerMessage) {
        let guard = self
            .controller
            .lock()
            .expect("Controller sender lock poisoned");
        let Some(sender) = guard.as_ref() else {
            warn!("No controller connected, dropping message");
            return;
        };
        if let Err(error) = sender.send(message) {
            error!("Failed to send a message to the controller: {error}");
            // The controller is gone, so there is nothing left to serve.
            std::process::exit(0);
        }
    }

    fn default_webview_url(&self) -> Url {
        self.config.url.clone()
    }

    fn last_mouse_position(&self) -> Option<DevicePoint> {
        self.mouse_position.get()
    }

    /// Show a native context menu for the given Servo context menu, storing it
    /// so that the selected action can be reported when the user picks a menu
    /// item (see `handle_context_menu_event`).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn show_context_menu(&self, context_menu: servo::ContextMenu) {
        use muda::ContextMenu as _;

        let menu = muda::Menu::new();
        let mut actions: Vec<(String, ContextMenuAction)> = Vec::new();
        for (index, item) in context_menu.items().iter().enumerate() {
            match item {
                ContextMenuItem::Item {
                    label,
                    action,
                    enabled,
                } => {
                    let id = format!("verso-cm-{index}");
                    let menu_item = muda::MenuItem::with_id(&id, label, *enabled, None);
                    if let Err(error) = menu.append(&menu_item) {
                        warn!("Failed to append a context menu item: {error}");
                        continue;
                    }
                    actions.push((id, *action));
                }
                ContextMenuItem::Separator => {
                    if let Err(error) = menu.append(&muda::PredefinedMenuItem::separator()) {
                        warn!("Failed to append a context menu separator: {error}");
                    }
                }
            }
        }

        // Replacing a previous pending menu drops it, which dismisses it with Servo.
        *self
            .pending_context_menu
            .lock()
            .expect("Pending context menu lock poisoned") = Some((context_menu, actions));

        // Show the context menu. This blocks until the menu is dismissed.
        let window_handle = self
            .window
            .window_handle()
            .expect("Failed to get the window handle");
        // The window handle is valid, since it comes from our own live window.
        unsafe {
            match window_handle.as_raw() {
                #[cfg(target_os = "macos")]
                winit::raw_window_handle::RawWindowHandle::AppKit(handle) => {
                    let ns_view = handle.ns_view.as_ptr();
                    menu.show_context_menu_for_nsview(ns_view, None);
                }
                #[cfg(target_os = "windows")]
                winit::raw_window_handle::RawWindowHandle::Win32(handle) => {
                    let hwnd = handle.hwnd;
                    menu.show_context_menu_for_hwnd(hwnd.into(), None);
                }
                handle => unreachable!("unknown window handle {handle:?} for this platform"),
            }
        }
    }

    /// Handle a native context menu event by selecting the Servo action that
    /// belongs to the chosen menu item id.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn handle_context_menu_event(&self, id: &str) {
        let mut pending_context_menu = self
            .pending_context_menu
            .lock()
            .expect("Pending context menu lock poisoned");
        let Some((_, actions)) = pending_context_menu.as_ref() else {
            return;
        };
        let Some(action) = actions
            .iter()
            .find(|(menu_id, _)| menu_id == id)
            .map(|(_, action)| *action)
        else {
            return;
        };
        let Some((context_menu, _)) = pending_context_menu.take() else {
            return;
        };
        context_menu.select(action);
    }

    /// Check the current window state allows applying drag-resize in the
    /// client region of an undecorated window.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn should_use_client_region_drag(&self) -> bool {
        !self.window.is_decorated()
            && !self.window.is_maximized()
            && self.window.is_resizable()
            && self.window.fullscreen().is_none()
    }

    /// Get the drag-resize direction for the current mouse position, if it is
    /// within the edge zone of the window.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn drag_resize_direction(&self) -> Option<winit::window::ResizeDirection> {
        use winit::window::ResizeDirection;

        let mouse_position = self.mouse_position.get()?;
        let window_size = self.window.outer_size();
        let border_size = (5.0 * self.window.scale_factor()) as f32;

        let x_direction = if mouse_position.x < border_size {
            Some(ResizeDirection::West)
        } else if mouse_position.x > window_size.width as f32 - border_size {
            Some(ResizeDirection::East)
        } else {
            None
        };

        let y_direction = if mouse_position.y < border_size {
            Some(ResizeDirection::North)
        } else if mouse_position.y > window_size.height as f32 - border_size {
            Some(ResizeDirection::South)
        } else {
            None
        };

        let direction = match (x_direction, y_direction) {
            (Some(ResizeDirection::East), None) => ResizeDirection::East,
            (Some(ResizeDirection::West), None) => ResizeDirection::West,
            (None, Some(ResizeDirection::North)) => ResizeDirection::North,
            (None, Some(ResizeDirection::South)) => ResizeDirection::South,
            (Some(ResizeDirection::East), Some(ResizeDirection::North)) => {
                ResizeDirection::NorthEast
            }
            (Some(ResizeDirection::West), Some(ResizeDirection::North)) => {
                ResizeDirection::NorthWest
            }
            (Some(ResizeDirection::East), Some(ResizeDirection::South)) => {
                ResizeDirection::SouthEast
            }
            (Some(ResizeDirection::West), Some(ResizeDirection::South)) => {
                ResizeDirection::SouthWest
            }
            _ => return None,
        };

        Some(direction)
    }

    /// Start a drag-resize of the window in the direction of the current edge
    /// zone, if the cursor is inside one.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn drag_resize_window(&self) {
        if let Some(direction) = self.drag_resize_direction()
            && let Err(error) = self.window.drag_resize_window(direction)
        {
            error!("Failed to drag-resize the window: {error:?}");
        }
    }

    /// Set the drag-resize cursor when the mouse hovers the border of an
    /// undecorated window, resetting it to the default cursor otherwise.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn update_edge_resize_cursor(&self) {
        let direction = self
            .should_use_client_region_drag()
            .then(|| self.drag_resize_direction())
            .flatten();
        self.in_edge_resize_zone.set(direction.is_some());
        match direction {
            Some(direction) => {
                self.window
                    .set_cursor(winit::window::CursorIcon::from(direction));
            }
            None => {
                self.window.set_cursor(winit::window::CursorIcon::Default);
            }
        }
    }
}

/// The state needed to bootstrap Servo.
///
/// Boxed into [`App::Initial`] because it is much larger than the shell handle.
struct Startup {
    proxy: EventLoopProxy<WakeEvent>,
    config: Config,
    controller: Option<IpcSender<ToControllerMessage>>,
    listeners: Listeners,
}

/// The winit `ApplicationHandler` driving the shell.
enum App {
    /// Waiting for `resumed` to bootstrap Servo.
    Initial(Box<Startup>),
    /// Servo and the webview are up.
    Running(ShellHandle),
}

impl ApplicationHandler<WakeEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let App::Initial(startup) = self else {
            return;
        };
        let Startup {
            proxy,
            config,
            controller,
            listeners,
        } = &mut **startup;

        // The resource reader is registered through the `submit_resource_reader!`
        // inventory macro; this sets the directory it reads resources from.
        resources::set_resource_dir(config.resource_dir.clone());

        let window = Rc::new(
            event_loop
                .create_window(config.window_attributes.clone())
                .expect("Failed to create the winit window"),
        );
        let display_handle = event_loop
            .display_handle()
            .expect("Failed to get the display handle");
        let parent_window_handle = window
            .window_handle()
            .expect("Failed to get the window handle");
        let parent_raw = parent_window_handle.as_raw();
        let scale_factor = window.scale_factor();
        let window_size = window.inner_size();

        let has_panel = controller.is_none() && config.with_panel;
        let panel_height = if has_panel {
            ((50.0 + 4.0) * scale_factor).round() as u32
        } else {
            0
        };

        let (content_window, content_size) = if has_panel {
            let content_height = window_size.height.saturating_sub(panel_height);
            let content_size = dpi::PhysicalSize::new(window_size.width, content_height);

            let mut content_attrs = winit::window::Window::default_attributes()
                .with_title("Verso Content")
                .with_inner_size(content_size)
                .with_decorations(false);

            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if let Ok(mut pos) = window.inner_position() {
                pos.y += panel_height as i32;
                content_attrs = content_attrs.with_position(pos);
            }

            let content_attrs_with_parent =
                unsafe { content_attrs.clone().with_parent_window(Some(parent_raw)) };
            let content_win = event_loop
                .create_window(content_attrs_with_parent)
                .or_else(|_| event_loop.create_window(content_attrs))
                .expect("Failed to create content child window");
            (Rc::new(content_win), content_size)
        } else {
            (window.clone(), window.inner_size())
        };

        let content_window_handle = content_window
            .window_handle()
            .expect("Failed to get the content window handle");

        let rendering_context = Rc::new(
            servo::WindowRenderingContext::new(display_handle, content_window_handle, content_size)
                .expect("Failed to create the rendering context"),
        );

        let mut protocol_registry = ProtocolRegistry::with_internal_protocols();
        // Serve Verso's resource directory over the `verso` scheme.
        if let Err(error) = protocol_registry.register("verso", resources::VersoProtocolHandler) {
            warn!("Registering the `verso` protocol failed: {error:?}");
        }
        for custom_protocol in &config.custom_protocols {
            // The contents of custom protocol schemes are served through the
            // `load_web_resource` delegate callback; registering the scheme
            // only marks it as secure and fetchable.
            if let Err(error) = protocol_registry.register(
                &custom_protocol.scheme,
                CustomProtocolHandler(custom_protocol.clone()),
            ) {
                warn!(
                    "Registering the custom protocol {} failed: {error:?}",
                    custom_protocol.scheme
                );
            }
        }

        let mut preferences = Preferences {
            user_agent: config.user_agent.clone(),
            dom_notification_enabled: true,
            ..Default::default()
        };
        if let Some(devtools_port) = config.devtools_port {
            preferences.devtools_server_enabled = true;
            preferences.devtools_server_listen_address = format!("127.0.0.1:{devtools_port}");
        }
        let mut opts = Opts::default();
        if let Some(profiler_settings) = &config.profiler_settings {
            opts.time_profiling = Some(profiler_settings.output_options.clone());
            opts.time_profiler_trace_path = profiler_settings.trace_path.clone();
        }

        let webdriver_receiver = config.webdriver_port.map(|webdriver_port| {
            let (sender, receiver) = crossbeam_channel::unbounded::<servo::WebDriverCommandMsg>();
            webdriver_server::start_server(
                webdriver_port,
                sender,
                Box::new(Waker(proxy.clone())),
                preferences.clone(),
            );
            info!("WebDriver server listening on port {webdriver_port}");
            receiver
        });

        let servo = ServoBuilder::default()
            .opts(opts)
            .preferences(preferences)
            .protocol_registry(protocol_registry)
            .event_loop_waker(Box::new(Waker(proxy.clone())))
            .build();
        servo.setup_logging();

        let shell = Rc::new(Shell {
            window: window.clone(),
            content_window: content_window.clone(),
            proxy: proxy.clone(),
            servo: servo.clone(),
            webview: RefCell::new(None),
            rendering_context: RefCell::new(rendering_context.clone()),
            config: config.clone(),
            controller: Mutex::new(controller.clone()),
            listeners: Mutex::new(std::mem::take(listeners)),
            mouse_position: Cell::new(None),
            panel_mouse_position: Cell::new(None),
            modifiers: Cell::new(winit::keyboard::ModifiersState::default()),
            pending_navigations: Mutex::new(HashMap::new()),
            pending_web_resources: Mutex::new(HashMap::new()),
            webdriver_receiver: Mutex::new(webdriver_receiver),
            visible_input_method: Mutex::new(None),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            pending_context_menu: Mutex::new(None),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            in_edge_resize_zone: Cell::new(false),
            tabs: RefCell::new(None),
            self_handle: RefCell::new(std::rc::Weak::new()),
            panel: RefCell::new(None),
        });

        let webview = WebViewBuilder::new(&servo, rendering_context.clone())
            .url(shell.default_webview_url())
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(shell.clone())
            .build();
        if let Some(zoom_level) = config.zoom_level {
            webview.set_page_zoom(zoom_level);
        }
        shell.webview.borrow_mut().replace(webview.clone());

        // Standalone tabbed browsing and control panel: only when no controller is attached and
        // the panel was enabled (default for standalone).
        if controller.is_none() && config.with_panel {
            tabs::store_self_handle(&shell);
            shell
                .tabs
                .borrow_mut()
                .replace(tabs::TabState::new(webview));

            let parent_window_handle = window
                .window_handle()
                .expect("Failed to get parent window handle");
            let parent_raw = parent_window_handle.as_raw();
            let scale_factor = window.scale_factor();
            let window_size = window.inner_size();
            let panel_height = ((50.0 + 4.0) * scale_factor).round() as u32;
            let panel_size = dpi::PhysicalSize::new(window_size.width, panel_height);

            let mut panel_attrs = winit::window::Window::default_attributes()
                .with_title("Verso Panel")
                .with_inner_size(panel_size)
                .with_decorations(false)
                .with_transparent(true);

            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if let Ok(pos) = window.inner_position() {
                panel_attrs = panel_attrs.with_position(pos);
            }

            let panel_attrs_with_parent =
                unsafe { panel_attrs.clone().with_parent_window(Some(parent_raw)) };
            let panel_window_res = event_loop
                .create_window(panel_attrs_with_parent)
                .or_else(|_| event_loop.create_window(panel_attrs));

            match panel_window_res {
                Ok(panel_window) => {
                    let panel_window = Rc::new(panel_window);
                    let panel_wh = panel_window
                        .window_handle()
                        .expect("Failed to get panel window handle");
                    match servo::WindowRenderingContext::new(
                        display_handle,
                        panel_wh,
                        panel_window.inner_size(),
                    ) {
                        Ok(panel_rc) => {
                            let panel_rendering_context: Rc<dyn servo::RenderingContext> =
                                Rc::new(panel_rc);
                            if let Ok(panel_url) =
                                Url::parse("verso://resources/components/panel.html")
                            {
                                let panel_webview = WebViewBuilder::new(
                                    &servo,
                                    panel_rendering_context.clone(),
                                )
                                .url(panel_url.clone())
                                .hidpi_scale_factor(Scale::new(scale_factor as f32))
                                .delegate(shell.clone())
                                .build();
                                shell.panel.borrow_mut().replace(Panel {
                                    window: panel_window,
                                    rendering_context: panel_rendering_context,
                                    webview: panel_webview,
                                    initial_url: panel_url,
                                });
                            }
                        }
                        Err(e) => {
                            warn!("Failed to create rendering context for panel: {e:?}");
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to create child window for panel: {e:?}");
                }
            }
        }

        *self = Self::Running(shell);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let App::Running(shell) = &*self else {
            return;
        };
        shell.servo.spin_event_loop();

        // Servo may have shut down while its event loop was being spun.
        let App::Running(shell) = &*self else {
            return;
        };

        // Check if event is for the panel child window.
        let is_panel_window = shell
            .panel
            .borrow()
            .as_ref()
            .is_some_and(|p| p.window.id() == window_id);

        if is_panel_window {
            let panel_guard = shell.panel.borrow();
            let Some(panel) = panel_guard.as_ref() else {
                return;
            };

            match event {
                WindowEvent::CloseRequested => {
                    event_loop.exit();
                }
                WindowEvent::RedrawRequested => {
                    panel.webview.paint();
                    panel.rendering_context.present();
                }
                WindowEvent::Resized(size) => {
                    panel.webview.resize(size);
                }
                WindowEvent::Focused(focused) => {
                    panel.webview.set_focused(focused);
                }
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    panel
                        .webview
                        .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let point = DevicePoint::new(position.x as f32, position.y as f32);
                    shell.panel_mouse_position.set(Some(point));
                    panel
                        .webview
                        .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                            point.into(),
                        )));
                }
                WindowEvent::CursorLeft { .. } => {
                    shell.panel_mouse_position.set(None);
                    panel.webview.notify_input_event(InputEvent::MouseLeftViewport(
                        MouseLeftViewportEvent::default(),
                    ));
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let Some(point) = shell.panel_mouse_position.get() else {
                        return;
                    };
                    let (x, y, mode) = match delta {
                        winit::event::MouseScrollDelta::LineDelta(dx, dy) => (
                            (dx * 76.0) as f64,
                            (dy * 76.0) as f64,
                            servo::WheelMode::DeltaLine,
                        ),
                        winit::event::MouseScrollDelta::PixelDelta(position) => {
                            (position.x, position.y, servo::WheelMode::DeltaPixel)
                        }
                    };
                    panel.webview.notify_input_event(InputEvent::Wheel(
                        servo::WheelEvent::new(servo::WheelDelta { x, y, z: 0.0, mode }, point.into()),
                    ));
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    let Some(point) = shell.panel_mouse_position.get() else {
                        return;
                    };
                    let action = match state {
                        winit::event::ElementState::Pressed => MouseButtonAction::Down,
                        winit::event::ElementState::Released => MouseButtonAction::Up,
                    };
                    let button = match button {
                        winit::event::MouseButton::Left => servo::MouseButton::Primary,
                        winit::event::MouseButton::Right => servo::MouseButton::Secondary,
                        winit::event::MouseButton::Middle => servo::MouseButton::Auxiliary,
                        winit::event::MouseButton::Back => servo::MouseButton::Back,
                        winit::event::MouseButton::Forward => servo::MouseButton::Forward,
                        winit::event::MouseButton::Other(value) => {
                            servo::MouseButton::Other(value)
                        }
                    };
                    panel
                        .webview
                        .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                            action,
                            button,
                            point.into(),
                        )));
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    let keyboard_event =
                        keyutils::keyboard_event_from_winit(&event, shell.modifiers.get());
                    panel.webview.notify_input_event(InputEvent::Keyboard(
                        servo::KeyboardEvent::new(keyboard_event),
                    ));
                }
                WindowEvent::Ime(ime) => {
                    let ime_event = match ime {
                        winit::event::Ime::Enabled => {
                            servo::ImeEvent::Composition(servo::CompositionEvent {
                                state: servo::CompositionState::Start,
                                data: String::new(),
                            })
                        }
                        winit::event::Ime::Preedit(text, _) => {
                            servo::ImeEvent::Composition(servo::CompositionEvent {
                                state: servo::CompositionState::Update,
                                data: text,
                            })
                        }
                        winit::event::Ime::Commit(text) => {
                            servo::ImeEvent::Composition(servo::CompositionEvent {
                                state: servo::CompositionState::End,
                                data: text,
                            })
                        }
                        winit::event::Ime::Disabled => {
                            servo::ImeEvent::Composition(servo::CompositionEvent {
                                state: servo::CompositionState::End,
                                data: String::new(),
                            })
                        }
                    };
                    panel.webview.notify_input_event(InputEvent::Ime(ime_event));
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    shell.modifiers.set(modifiers.state());
                }
                _ => {}
            }
            return;
        }

        // Consume tabbed-browsing panel shortcuts before anything else sees
        // them; switching tabs replaces the shell's active webview, so this
        // must run before it is borrowed below.
        if let WindowEvent::KeyboardInput { event, .. } = &event
            && tabs::TabState::handle_shortcut(shell, event)
        {
            return;
        }

        // Handle events for the top-level container window (when child windows are in use).
        if window_id == shell.window.id() && shell.content_window.id() != shell.window.id() {
            match event {
                WindowEvent::CloseRequested => {
                    if shell
                        .listeners
                        .lock()
                        .expect("Listeners lock poisoned")
                        .close_requested
                    {
                        shell.send_to_controller(ToControllerMessage::OnCloseRequested);
                    } else {
                        event_loop.exit();
                    }
                }
                WindowEvent::Resized(size) => {
                    let scale_factor = shell.window.scale_factor();
                    let panel_height = if shell.panel.borrow().is_some() {
                        ((50.0 + 4.0) * scale_factor).round() as u32
                    } else {
                        0
                    };
                    let content_height = size.height.saturating_sub(panel_height);
                    let panel_size = dpi::PhysicalSize::new(size.width, panel_height);
                    let content_size = dpi::PhysicalSize::new(size.width, content_height);

                    if let Some(panel) = shell.panel.borrow().as_ref() {
                        let _ = panel.window.request_inner_size(panel_size);
                        panel.webview.resize(panel_size);
                    }

                    let _ = shell.content_window.request_inner_size(content_size);
                    if let Some(webview) = shell.webview.borrow().as_ref() {
                        webview.resize(content_size);
                    }
                }
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    if let Some(panel) = shell.panel.borrow().as_ref() {
                        panel
                            .webview
                            .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                    }
                    if let Some(webview) = shell.webview.borrow().as_ref() {
                        webview.set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                    }
                }
                _ => {}
            }
            return;
        }

        let webview_guard = shell.webview.borrow();
        let Some(webview) = webview_guard.as_ref() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                if shell
                    .listeners
                    .lock()
                    .expect("Listeners lock poisoned")
                    .close_requested
                {
                    shell.send_to_controller(ToControllerMessage::OnCloseRequested);
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                webview.paint();
                shell.rendering_context.borrow().present();
            }
            WindowEvent::Resized(size) => {
                webview.resize(size);
            }
            WindowEvent::Moved(_) => {}
            WindowEvent::Focused(focused) => {
                webview.set_focused(focused);
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                webview.set_hidpi_scale_factor(Scale::new(scale_factor as f32));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(point) = shell.last_mouse_position() else {
                    return;
                };

                // Handle drag-resize in the client region of undecorated windows.
                #[cfg(any(target_os = "linux", target_os = "windows"))]
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Left
                    && shell.in_edge_resize_zone.get()
                {
                    shell.drag_resize_window();
                }

                let action = match state {
                    winit::event::ElementState::Pressed => MouseButtonAction::Down,
                    winit::event::ElementState::Released => MouseButtonAction::Up,
                };
                let button = match button {
                    winit::event::MouseButton::Left => servo::MouseButton::Primary,
                    winit::event::MouseButton::Right => servo::MouseButton::Secondary,
                    winit::event::MouseButton::Middle => servo::MouseButton::Auxiliary,
                    winit::event::MouseButton::Back => servo::MouseButton::Back,
                    winit::event::MouseButton::Forward => servo::MouseButton::Forward,
                    winit::event::MouseButton::Other(value) => servo::MouseButton::Other(value),
                };
                webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                    action,
                    button,
                    point.into(),
                )));
            }
            WindowEvent::CursorMoved { position, .. } => {
                let point = DevicePoint::new(position.x as f32, position.y as f32);
                shell.mouse_position.set(Some(point));
                webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));

                // Handle the resize cursor in the client region of undecorated
                // windows.
                #[cfg(any(target_os = "linux", target_os = "windows"))]
                shell.update_edge_resize_cursor();
            }
            WindowEvent::CursorLeft { .. } => {
                shell.mouse_position.set(None);
                webview.notify_input_event(InputEvent::MouseLeftViewport(
                    MouseLeftViewportEvent::default(),
                ));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(point) = shell.last_mouse_position() else {
                    return;
                };
                let (x, y, mode) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(dx, dy) => (
                        (dx * 76.0) as f64,
                        (dy * 76.0) as f64,
                        servo::WheelMode::DeltaLine,
                    ),
                    winit::event::MouseScrollDelta::PixelDelta(position) => {
                        (position.x, position.y, servo::WheelMode::DeltaPixel)
                    }
                };
                webview.notify_input_event(InputEvent::Wheel(servo::WheelEvent::new(
                    servo::WheelDelta { x, y, z: 0.0, mode },
                    point.into(),
                )));
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let keyboard_event =
                    keyutils::keyboard_event_from_winit(&event, shell.modifiers.get());
                webview.notify_input_event(InputEvent::Keyboard(servo::KeyboardEvent::new(
                    keyboard_event,
                )));
            }
            WindowEvent::Ime(ime) => {
                let ime_event = match ime {
                    winit::event::Ime::Enabled => {
                        servo::ImeEvent::Composition(servo::CompositionEvent {
                            state: servo::CompositionState::Start,
                            data: String::new(),
                        })
                    }
                    winit::event::Ime::Preedit(text, _) => {
                        servo::ImeEvent::Composition(servo::CompositionEvent {
                            state: servo::CompositionState::Update,
                            data: text,
                        })
                    }
                    winit::event::Ime::Commit(text) => {
                        servo::ImeEvent::Composition(servo::CompositionEvent {
                            state: servo::CompositionState::End,
                            data: text,
                        })
                    }
                    winit::event::Ime::Disabled => {
                        servo::ImeEvent::Composition(servo::CompositionEvent {
                            state: servo::CompositionState::End,
                            data: String::new(),
                        })
                    }
                };
                webview.notify_input_event(InputEvent::Ime(ime_event));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                shell.modifiers.set(modifiers.state());
            }
            WindowEvent::ThemeChanged(theme) => {
                let servo_theme = match theme {
                    winit::window::Theme::Light => servo::Theme::Light,
                    winit::window::Theme::Dark => servo::Theme::Dark,
                };
                webview.notify_theme_change(servo_theme);
            }
            WindowEvent::PinchGesture { delta, .. } => {
                if let Some(point) = shell.last_mouse_position() {
                    webview.adjust_pinch_zoom(delta as f32 + 1.0, point);
                }
            }
            _ => (),
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        let App::Running(shell) = &*self else {
            return;
        };
        shell.servo.spin_event_loop();

        match event {
            WakeEvent::Wake => {}
            WakeEvent::ExitRequested => {
                info!("Exiting");
                event_loop.exit();
            }
            WakeEvent::IpcMessage(message) => {
                // Servo may have shut down while its event loop was being spun.
                let App::Running(shell) = &*self else {
                    return;
                };
                handle_controller_message(shell, *message);
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let App::Running(shell) = &*self else {
            return;
        };
        shell.servo.spin_event_loop();

        // Handle selections in a native context menu shown through muda.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
                shell.handle_context_menu_event(event.id().as_ref());
            }
        }

        if let Some(receiver) = shell
            .webdriver_receiver
            .lock()
            .expect("Webdriver receiver lock poisoned")
            .as_ref()
        {
            while let Ok(command) = receiver.try_recv() {
                shell.servo.execute_webdriver_command(command);
            }
        }
    }
}

fn handle_controller_message(shell: &Shell, message: ToVersoMessage) {
    let webview_guard = shell.webview.borrow();
    let Some(webview) = webview_guard.as_ref() else {
        return;
    };
    let window = &shell.window;

    match message {
        ToVersoMessage::SetConfig(_) => {
            warn!("Received a late `SetConfig` message, ignoring");
        }
        ToVersoMessage::Exit => {
            info!("Exiting on controller request");
            let _ = shell.proxy.send_event(WakeEvent::ExitRequested);
        }
        ToVersoMessage::ListenToOnCloseRequested => {
            shell
                .listeners
                .lock()
                .expect("Listeners lock poisoned")
                .close_requested = true;
        }
        ToVersoMessage::NavigateTo(url) => webview.load(url),
        ToVersoMessage::Reload => webview.reload(),
        ToVersoMessage::ListenToOnNavigationStarting => {
            shell
                .listeners
                .lock()
                .expect("Listeners lock poisoned")
                .navigation_starting = true;
        }
        ToVersoMessage::OnNavigationStartingResponse(id, allow) => {
            if let Some(request) = shell
                .pending_navigations
                .lock()
                .expect("Pending navigations lock poisoned")
                .remove(&id)
            {
                if allow {
                    request.allow();
                } else {
                    request.deny();
                }
            } else {
                warn!("Received a navigation response for an unknown request");
            }
        }
        ToVersoMessage::ExecuteScript(script) => {
            webview.evaluate_javascript(script, |result| {
                if let Err(error) = result {
                    error!("Failed to evaluate JavaScript: {error:?}");
                }
            });
        }
        ToVersoMessage::ListenToWebResourceRequests => {
            shell
                .listeners
                .lock()
                .expect("Listeners lock poisoned")
                .web_resource_requests = true;
        }
        ToVersoMessage::WebResourceRequestResponse(response) => {
            if let Some(load) = shell
                .pending_web_resources
                .lock()
                .expect("Pending web resources lock poisoned")
                .remove(&response.id)
            {
                match response.response {
                    Some(http_response) => {
                        let url = load.request.url.clone();
                        let mut intercepted = load.intercept(
                            WebResourceResponse::new(url)
                                .headers(http_response.headers().clone())
                                .status_code(http_response.status()),
                        );
                        intercepted.send_body_data(http_response.into_body());
                        intercepted.finish();
                    }
                    None => drop(load),
                }
            } else {
                warn!("Received a web resource response for an unknown request");
            }
        }
        ToVersoMessage::SetTitle(title) => window.set_title(&title),
        ToVersoMessage::SetSize(size) => {
            let _ = window.request_inner_size(size);
        }
        ToVersoMessage::SetPosition(position) => window.set_outer_position(position),
        ToVersoMessage::SetMaximized(maximized) => window.set_maximized(maximized),
        ToVersoMessage::SetMinimized(minimized) => window.set_minimized(minimized),
        ToVersoMessage::SetFullscreen(fullscreen) => {
            window.set_fullscreen(fullscreen.then_some(winit::window::Fullscreen::Borderless(None)))
        }
        ToVersoMessage::SetVisible(visible) => window.set_visible(visible),
        ToVersoMessage::SetWindowLevel(level) => {
            window.set_window_level(config::to_winit_window_level(level));
        }
        ToVersoMessage::SetTheme(theme) => {
            let theme = theme.map(|theme| match theme {
                versoview_messages::Theme::Light => winit::window::Theme::Light,
                versoview_messages::Theme::Dark => winit::window::Theme::Dark,
            });
            window.set_theme(theme);
            webview.notify_theme_change(match theme {
                Some(winit::window::Theme::Light) => servo::Theme::Light,
                Some(winit::window::Theme::Dark) | None => servo::Theme::Dark,
            });
        }

        ToVersoMessage::StartDragging => {
            let _ = window.drag_window();
        }
        ToVersoMessage::Focus => window.focus_window(),
        ToVersoMessage::GetTitle(id) => {
            let title = window.title();
            shell.send_to_controller(ToControllerMessage::GetTitleResponse(id, title));
        }
        ToVersoMessage::GetSize(id, size_type) => {
            let size = match size_type {
                versoview_messages::SizeType::Inner => window.inner_size(),
                versoview_messages::SizeType::Outer => window.outer_size(),
            };
            shell.send_to_controller(ToControllerMessage::GetSizeResponse(id, size));
        }
        ToVersoMessage::GetPosition(id, position_type) => {
            let position = match position_type {
                versoview_messages::PositionType::Inner => window.inner_position().ok(),
                versoview_messages::PositionType::Outer => window.outer_position().ok(),
            };
            shell.send_to_controller(ToControllerMessage::GetPositionResponse(id, position));
        }
        ToVersoMessage::GetMaximized(id) => shell.send_to_controller(
            ToControllerMessage::GetMaximizedResponse(id, window.is_maximized()),
        ),
        ToVersoMessage::GetMinimized(id) => shell.send_to_controller(
            ToControllerMessage::GetMinimizedResponse(id, window.is_minimized().unwrap_or(false)),
        ),
        ToVersoMessage::GetFullscreen(id) => shell.send_to_controller(
            ToControllerMessage::GetFullscreenResponse(id, window.fullscreen().is_some()),
        ),
        ToVersoMessage::GetVisible(id) => shell.send_to_controller(
            ToControllerMessage::GetVisibleResponse(id, window.is_visible().unwrap_or(false)),
        ),
        ToVersoMessage::GetScaleFactor(id) => shell.send_to_controller(
            ToControllerMessage::GetScaleFactorResponse(id, window.scale_factor()),
        ),
        ToVersoMessage::GetTheme(id) => {
            let theme = match window.theme() {
                Some(winit::window::Theme::Light) => versoview_messages::Theme::Light,
                Some(winit::window::Theme::Dark) | None => versoview_messages::Theme::Dark,
            };
            shell.send_to_controller(ToControllerMessage::GetThemeResponse(id, theme));
        }
        ToVersoMessage::GetCurrentUrl(id) => {
            let url = webview.url().unwrap_or_else(|| shell.default_webview_url());
            shell.send_to_controller(ToControllerMessage::GetCurrentUrlResponse(id, url));
        }
    }
}

/// A no-op protocol handler for the controller-registered custom protocol
/// schemes, whose contents are served through the `load_web_resource` delegate
/// callback instead. The handler only carries the scheme's security flags.
struct CustomProtocolHandler(versoview_messages::CustomProtocol);

impl servo::protocol_handler::ProtocolHandler for CustomProtocolHandler {
    fn load(
        &self,
        _request: &mut servo::protocol_handler::Request,
        _done_chan: &mut servo::protocol_handler::DoneChannel,
        _context: &servo::protocol_handler::FetchContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = servo::protocol_handler::Response> + Send>,
    > {
        Box::pin(std::future::ready(
            servo::protocol_handler::Response::network_error(
                servo::protocol_handler::NetworkError::ConnectionFailure,
            ),
        ))
    }

    fn is_secure(&self) -> bool {
        self.0.secure
    }

    fn is_fetchable(&self) -> bool {
        self.0.fetchable
    }
}

impl WebViewDelegate for Shell {
    fn notify_new_frame_ready(&self, webview: WebView) {
        if let Some(panel) = self.panel.borrow().as_ref()
            && panel.webview.id() == webview.id()
        {
            panel.window.request_redraw();
        } else {
            self.content_window.request_redraw();
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        // The controller polls the URL with `GetCurrentUrl`; the tabbed panel
        // caches it for a future visible tab strip.
        if let Some(tabs) = self.tabs.borrow_mut().as_mut() {
            tabs.set_url(webview.id(), url.clone());
        }

        // Keep the panel's address bar and navigation buttons in sync
        if let Some(panel) = self.panel.borrow().as_ref()
            && panel.webview.id() != webview.id()
        {
            let can_back = webview.can_go_back();
            let can_forward = webview.can_go_forward();
            let script = format!(
                "if (window.navbar) {{ \
                    if (window.navbar.setNavbarUrl) {{ window.navbar.setNavbarUrl('{url}'); }} \
                    if (window.navbar.setNavBtnEnabled) {{ window.navbar.setNavBtnEnabled({can_back}, {can_forward}); }} \
                }}"
            );
            panel.webview.evaluate_javascript(script, |_| {});
        }
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        // The controller polls the title with `GetTitle`; the tabbed panel
        // reflects the active tab's title in the window title.
        if let Some(tabs) = self.tabs.borrow_mut().as_mut()
            && let Some(window_title) = tabs.set_title(webview.id(), title)
        {
            self.window.set_title(&window_title);
        }
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, backtrace: Option<String>) {
        error!("Servo crashed: {reason}\n{backtrace:?}");
        let _ = self.proxy.send_event(WakeEvent::ExitRequested);
    }

    fn notify_closed(&self, webview: WebView) {
        // The page closed itself with `window.close()`. In tabbed mode this
        // closes that tab; otherwise the webview was the whole window.
        let mut tabs_guard = self.tabs.borrow_mut();
        if let Some(tabs) = tabs_guard.as_mut() {
            if !tabs.close_by_id(self, webview.id()) {
                *tabs_guard = None;
                let _ = self.proxy.send_event(WakeEvent::ExitRequested);
            }
            return;
        }
        drop(tabs_guard);
        info!("The webview was closed, exiting");
        let _ = self.proxy.send_event(WakeEvent::ExitRequested);
    }

    fn request_navigation(&self, _webview: WebView, navigation_request: NavigationRequest) {
        let listening = self
            .listeners
            .lock()
            .expect("Listeners lock poisoned")
            .navigation_starting;
        if !listening {
            navigation_request.allow();
            return;
        }

        // The serialized identifier is opaque to the controller; the URL is
        // used for the same reason the old shell used the pipeline id.
        let id = uuid::Uuid::new_v4().as_bytes().to_vec();
        self.send_to_controller(ToControllerMessage::OnNavigationStarting(
            id.clone(),
            navigation_request.url.clone(),
        ));
        self.pending_navigations
            .lock()
            .expect("Pending navigations lock poisoned")
            .insert(id, navigation_request);
    }

    fn request_move_to(&self, _webview: WebView, point: webrender_api::units::DeviceIntPoint) {
        self.window
            .set_outer_position(winit::dpi::PhysicalPosition::new(point.x, point.y));
    }

    fn request_resize_to(
        &self,
        _webview: WebView,
        requested_outer_size: webrender_api::units::DeviceIntSize,
    ) {
        let _ = self
            .window
            .request_inner_size(winit::dpi::PhysicalSize::new(
                requested_outer_size.width as u32,
                requested_outer_size.height as u32,
            ));
    }

    fn request_unload(&self, _webview: WebView, request: servo::AllowOrDenyRequest) {
        request.allow();
    }

    fn request_permission(&self, _webview: WebView, request: servo::PermissionRequest) {
        // TODO: surface permission requests to the controller. Deny by default
        // so pages cannot obtain granted features without consent.
        request.deny();
    }

    fn request_authentication(&self, _webview: WebView, _request: servo::AuthenticationRequest) {
        // TODO: surface authentication requests to the controller.
        // Dropping the request cancels the authentication.
    }

    fn request_create_new(
        &self,
        _parent_webview: WebView,
        request: servo::CreateNewWebViewRequest,
    ) {
        // With the tabbed-browsing panel, popups open in a new tab. Without
        // it, dropping the request denies the popup.
        if self.tabs.borrow().is_none() {
            return;
        }
        let Some(shell) = tabs::shell_handle(self) else {
            return;
        };
        let webview = request
            .builder(self.rendering_context.borrow().clone())
            .delegate(shell.clone())
            .build();
        if let Some(tabs) = self.tabs.borrow_mut().as_mut() {
            tabs.adopt(&shell, webview);
        }
    }

    fn show_embedder_control(&self, _webview: WebView, control: servo::EmbedderControl) {
        match control {
            servo::EmbedderControl::FilePicker(mut file_picker) => {
                let mut dialog = rfd::FileDialog::new();
                if let Some(current_path) = file_picker.current_paths().first() {
                    dialog = dialog.set_directory(current_path);
                }
                let files = if file_picker.allow_select_multiple() {
                    dialog.pick_files().unwrap_or_default()
                } else {
                    dialog.pick_file().into_iter().collect()
                };
                file_picker.select(&files);
            }
            servo::EmbedderControl::SelectElement(mut select_element) => {
                // No native picker UI yet: keep the current selection.
                let selected = select_element.selected_options();
                select_element.select(selected);
            }
            servo::EmbedderControl::ColorPicker(mut color_picker) => {
                // No native color picker UI yet: keep the current color.
                let current = color_picker.current_color();
                color_picker.select(current);
            }
            servo::EmbedderControl::ContextMenu(context_menu) => {
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    self.show_context_menu(context_menu);
                }
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                {
                    context_menu.dismiss();
                    warn!("Native context menus are not supported on this platform");
                }
            }
            servo::EmbedderControl::SimpleDialog(dialog) => {
                let message = dialog.message().to_string();
                match dialog {
                    servo::SimpleDialog::Alert(_) => {
                        rfd::MessageDialog::new()
                            .set_level(rfd::MessageLevel::Info)
                            .set_buttons(rfd::MessageButtons::Ok)
                            .set_title("Alert")
                            .set_description(&message)
                            .show();
                        dialog.confirm();
                    }
                    servo::SimpleDialog::Confirm(_) => {
                        let result = rfd::MessageDialog::new()
                            .set_level(rfd::MessageLevel::Warning)
                            .set_buttons(rfd::MessageButtons::YesNo)
                            .set_title("Confirm")
                            .set_description(&message)
                            .show();
                        if matches!(result, rfd::MessageDialogResult::Yes) {
                            dialog.confirm();
                        } else {
                            dialog.dismiss();
                        }
                    }
                    servo::SimpleDialog::Prompt(_) => {
                        if let Some(unparsed_url) = message.strip_prefix("NAVIGATE_TO:") {
                            let url = match Url::parse(unparsed_url) {
                                Ok(url_parsed) => Some(url_parsed),
                                Err(url::ParseError::RelativeUrlWithoutBase) => {
                                    Url::parse(&format!("https://{}", unparsed_url)).ok()
                                }
                                Err(e) => {
                                    warn!("Verso Panel failed to parse URL {unparsed_url}: {e}");
                                    None
                                }
                            };
                            if let Some(url) = url
                                && let Some(content_webview) = self.webview.borrow().as_ref()
                            {
                                content_webview.load(url);
                            }
                            dialog.dismiss();
                        } else {
                            match message.as_str() {
                                "PREV" => {
                                    if let Some(content_webview) = self.webview.borrow().as_ref()
                                        && content_webview.can_go_back()
                                    {
                                        content_webview.go_back(1);
                                    }
                                    dialog.dismiss();
                                }
                                "FORWARD" => {
                                    if let Some(content_webview) = self.webview.borrow().as_ref()
                                        && content_webview.can_go_forward()
                                    {
                                        content_webview.go_forward(1);
                                    }
                                    dialog.dismiss();
                                }
                                "REFRESH" => {
                                    if let Some(content_webview) = self.webview.borrow().as_ref() {
                                        content_webview.reload();
                                    }
                                    dialog.dismiss();
                                }
                                "MINIMIZE" => {
                                    self.window.set_minimized(true);
                                    dialog.dismiss();
                                }
                                "MAXIMIZE" => {
                                    let is_max = self.window.is_maximized();
                                    self.window.set_maximized(!is_max);
                                    dialog.dismiss();
                                }
                                "DRAG_WINDOW" => {
                                    let _ = self.window.drag_window();
                                    dialog.dismiss();
                                }
                                "DBCLICK_PANEL" => {
                                    let is_max = self.window.is_maximized();
                                    self.window.set_maximized(!is_max);
                                    dialog.dismiss();
                                }
                                other => {
                                    log::trace!("Verso Panel received unhandled prompt: {other}");
                                    dialog.dismiss();
                                }
                            }
                        }
                    }
                }
            }
            servo::EmbedderControl::InputMethod(input_method) => {
                // Show the input method (e.g. a candidate window) anchored to
                // the position of the focused editable element.
                *self
                    .visible_input_method
                    .lock()
                    .expect("Visible input method lock poisoned") = Some(input_method.id());
                let position = input_method.position();
                self.window.set_ime_allowed(true);
                self.window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(position.min.x, position.min.y),
                    winit::dpi::LogicalSize::new(
                        position.max.x - position.min.x,
                        position.max.y - position.min.y,
                    ),
                );
            }
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, control_id: servo::EmbedderControlId) {
        let hiding_input_method = {
            let mut visible_input_method = self
                .visible_input_method
                .lock()
                .expect("Visible input method lock poisoned");
            let matches = *visible_input_method == Some(control_id);
            if matches {
                *visible_input_method = None;
            }
            matches
        };
        if hiding_input_method {
            self.window.set_ime_allowed(false);
        } else {
            // If the hidden control is a pending native context menu, drop it,
            // which dismisses it with Servo.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                let mut pending_context_menu = self
                    .pending_context_menu
                    .lock()
                    .expect("Pending context menu lock poisoned");
                if pending_context_menu
                    .as_ref()
                    .is_some_and(|(context_menu, _)| context_menu.id() == control_id)
                {
                    *pending_context_menu = None;
                }
            }
        }
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        if let Some(panel) = self.panel.borrow().as_ref()
            && panel.webview.id() == webview.id()
        {
            let winit_cursor = match cursor {
                Cursor::None => {
                    panel.window.set_cursor_visible(false);
                    return;
                }
                Cursor::Default => winit::window::CursorIcon::Default,
                Cursor::Pointer => winit::window::CursorIcon::Pointer,
                Cursor::ContextMenu => winit::window::CursorIcon::ContextMenu,
                Cursor::Help => winit::window::CursorIcon::Help,
                Cursor::Progress => winit::window::CursorIcon::Progress,
                Cursor::Wait => winit::window::CursorIcon::Wait,
                Cursor::Cell => winit::window::CursorIcon::Cell,
                Cursor::Crosshair => winit::window::CursorIcon::Crosshair,
                Cursor::Text => winit::window::CursorIcon::Text,
                Cursor::VerticalText => winit::window::CursorIcon::VerticalText,
                Cursor::Alias => winit::window::CursorIcon::Alias,
                Cursor::Copy => winit::window::CursorIcon::Copy,
                Cursor::Move => winit::window::CursorIcon::Move,
                Cursor::NoDrop => winit::window::CursorIcon::NoDrop,
                Cursor::NotAllowed => winit::window::CursorIcon::NotAllowed,
                Cursor::Grab => winit::window::CursorIcon::Grab,
                Cursor::Grabbing => winit::window::CursorIcon::Grabbing,
                Cursor::EResize => winit::window::CursorIcon::EResize,
                Cursor::NResize => winit::window::CursorIcon::NResize,
                Cursor::NeResize => winit::window::CursorIcon::NeResize,
                Cursor::NwResize => winit::window::CursorIcon::NwResize,
                Cursor::SResize => winit::window::CursorIcon::SResize,
                Cursor::SeResize => winit::window::CursorIcon::SeResize,
                Cursor::SwResize => winit::window::CursorIcon::SwResize,
                Cursor::WResize => winit::window::CursorIcon::WResize,
                Cursor::EwResize => winit::window::CursorIcon::EwResize,
                Cursor::NsResize => winit::window::CursorIcon::NsResize,
                Cursor::NeswResize => winit::window::CursorIcon::NeswResize,
                Cursor::NwseResize => winit::window::CursorIcon::NwseResize,
                Cursor::ColResize => winit::window::CursorIcon::ColResize,
                Cursor::RowResize => winit::window::CursorIcon::RowResize,
                Cursor::AllScroll => winit::window::CursorIcon::AllScroll,
                Cursor::ZoomIn => winit::window::CursorIcon::ZoomIn,
                Cursor::ZoomOut => winit::window::CursorIcon::ZoomOut,
            };
            panel.window.set_cursor(winit_cursor);
            panel.window.set_cursor_visible(true);
            return;
        }

        // On Linux and Windows the cursor may be claimed by the edge resize
        // zone of an undecorated window; don't clobber it.
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if self.in_edge_resize_zone.get() {
            return;
        }
        let winit_cursor = match cursor {
            Cursor::None => {
                self.window.set_cursor_visible(false);
                return;
            }
            Cursor::Default => winit::window::CursorIcon::Default,
            Cursor::Pointer => winit::window::CursorIcon::Pointer,
            Cursor::ContextMenu => winit::window::CursorIcon::ContextMenu,
            Cursor::Help => winit::window::CursorIcon::Help,
            Cursor::Progress => winit::window::CursorIcon::Progress,
            Cursor::Wait => winit::window::CursorIcon::Wait,
            Cursor::Cell => winit::window::CursorIcon::Cell,
            Cursor::Crosshair => winit::window::CursorIcon::Crosshair,
            Cursor::Text => winit::window::CursorIcon::Text,
            Cursor::VerticalText => winit::window::CursorIcon::VerticalText,
            Cursor::Alias => winit::window::CursorIcon::Alias,
            Cursor::Copy => winit::window::CursorIcon::Copy,
            Cursor::Move => winit::window::CursorIcon::Move,
            Cursor::NoDrop => winit::window::CursorIcon::NoDrop,
            Cursor::NotAllowed => winit::window::CursorIcon::NotAllowed,
            Cursor::Grab => winit::window::CursorIcon::Grab,
            Cursor::Grabbing => winit::window::CursorIcon::Grabbing,
            Cursor::EResize => winit::window::CursorIcon::EResize,
            Cursor::NResize => winit::window::CursorIcon::NResize,
            Cursor::NeResize => winit::window::CursorIcon::NeResize,
            Cursor::NwResize => winit::window::CursorIcon::NwResize,
            Cursor::SResize => winit::window::CursorIcon::SResize,
            Cursor::SeResize => winit::window::CursorIcon::SeResize,
            Cursor::SwResize => winit::window::CursorIcon::SwResize,
            Cursor::WResize => winit::window::CursorIcon::WResize,
            Cursor::EwResize => winit::window::CursorIcon::EwResize,
            Cursor::NsResize => winit::window::CursorIcon::NsResize,
            Cursor::NeswResize => winit::window::CursorIcon::NeswResize,
            Cursor::NwseResize => winit::window::CursorIcon::NwseResize,
            Cursor::ColResize => winit::window::CursorIcon::ColResize,
            Cursor::RowResize => winit::window::CursorIcon::RowResize,
            Cursor::AllScroll => winit::window::CursorIcon::AllScroll,
            Cursor::ZoomIn => winit::window::CursorIcon::ZoomIn,
            Cursor::ZoomOut => winit::window::CursorIcon::ZoomOut,
        };
        self.window.set_cursor(winit_cursor);
        self.window.set_cursor_visible(true);
    }

    fn load_web_resource(&self, _webview: WebView, load: WebResourceLoad) {
        let listening = self
            .listeners
            .lock()
            .expect("Listeners lock poisoned")
            .web_resource_requests;
        if !listening {
            // Dropping the load without intercepting lets Servo load it.
            return;
        }

        let id = uuid::Uuid::new_v4();
        let request = &load.request;
        let mut builder = http::request::Builder::new()
            .uri(request.url.as_str())
            .method(request.method.clone());
        for (key, value) in request.headers.iter() {
            builder = builder.header(key, value);
        }
        let http_request = match builder.body(Vec::new()) {
            Ok(request) => request,
            Err(error) => {
                if request.url.scheme() == "data" {
                    log::debug!(
                        "Skipping data URL as it can't be parsed into an `http::Uri`: {error}"
                    );
                } else {
                    error!("Failed to construct a web resource request: {error}");
                }
                return;
            }
        };

        self.send_to_controller(ToControllerMessage::OnWebResourceRequested(
            WebResourceRequest {
                id,
                request: http_request,
            },
        ));
        self.pending_web_resources
            .lock()
            .expect("Pending web resources lock poisoned")
            .insert(id, load);
    }

    fn show_notification(&self, _webview: WebView, notification: servo::Notification) {
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        if let Err(error) = notify_rust::Notification::new()
            .summary(&notification.title)
            .body(&notification.body)
            .show()
        {
            warn!("Failed to show a notification: {error}");
        }
    }

    fn show_console_message(
        &self,
        _webview: WebView,
        level: servo::ConsoleLogLevel,
        message: String,
    ) {
        info!("[console] {level:?}: {message}");
    }
}

fn connect_to_controller(
    proxy: &EventLoopProxy<WakeEvent>,
) -> (Config, Option<IpcSender<ToControllerMessage>>, Listeners) {
    let cli_args = config::parse_cli_args().unwrap_or_default();
    let Some(ipc_channel) = cli_args.ipc_channel.clone() else {
        let config = Config::from_cli_args(cli_args);
        return (config, None, Listeners::default());
    };

    let sender = IpcSender::<ToControllerMessage>::connect(ipc_channel)
        .unwrap_or_else(|error| panic!("Failed to connect to the controller IPC channel: {error}"));
    let (to_verso_sender, receiver) =
        ipc::channel::<ToVersoMessage>().expect("Failed to create the verso IPC channel");
    sender
        .send(ToControllerMessage::SetToVersoSender(to_verso_sender))
        .expect("Failed to send the verso sender to the controller");
    let ToVersoMessage::SetConfig(settings) = receiver
        .recv()
        .expect("Failed to receive the initial settings from the controller")
    else {
        panic!("The initial message sent from versoview was not `ToVersoMessage::SetConfig`");
    };

    // Forward incoming controller messages to the event loop.
    let proxy = proxy.clone();
    ipc_channel::router::ROUTER.add_typed_route(
        receiver,
        Box::new(move |message| match message {
            Ok(message) => {
                if let Err(error) = proxy.send_event(WakeEvent::IpcMessage(Box::new(message))) {
                    error!("Failed to forward a controller message: {error}");
                }
            }
            Err(error) => error!("Failed to receive a controller message: {error}"),
        }),
    );

    (
        Config::from_controller_config(*settings),
        Some(sender),
        Listeners::default(),
    )
}

fn init_crypto() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");
}

fn main() -> Result<()> {
    init_crypto();

    let event_loop = EventLoop::<WakeEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let (config, controller, listeners) = connect_to_controller(&proxy);
    let mut app = App::Initial(Box::new(Startup {
        proxy,
        config,
        controller,
        listeners,
    }));
    let result = event_loop.run_app(&mut app);

    // Break the Servo <-> delegate reference cycle (WebView holds the delegate,
    // the shell holds Servo) so servo's `Drop` runs and flushes cookies and
    // storage to disk before the process exits.
    if let App::Running(shell) = &mut app {
        shell.panel.borrow_mut().take();
        shell.webview.borrow_mut().take();
        shell.tabs.borrow_mut().take();
    }

    result?;

    Ok(())
}
