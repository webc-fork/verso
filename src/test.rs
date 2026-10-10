/// Simple test harness for testing `versoview`.
///
/// Add your test to `Cargo.toml` with the `harness = false` option. This will prevent Rust's default test harness from running your test.
///
/// ```toml
/// [[test]]
/// name = "my_test"
/// harness = false
/// ```
///
/// Then, in your test, use the `verso_test!` macro to run your tests. The tests must be functions that take an `ActiveEventLoop`.
///
/// ```rust
/// use versoview::verso_test;
/// use versoview::winit::event_loop::ActiveEventLoop;
///
/// fn my_test(elwt: &ActiveEventLoop) {
///     // ...
/// }
///
/// fn other_test(elwt: &ActiveEventLoop) {
///     // ...
/// }
///
/// verso_test!(my_test, other_test);
/// ```
#[macro_export]
macro_rules! verso_test {
    ($($test:expr),*) => {
        fn main() -> Result<(), Box<dyn std::error::Error>> {
            const TESTS: &[$crate::test::__private::VersoBasedTest] = &[$(
                $crate::__verso_test_internal_collect_test!($test)
            ),*];

            // Test runners like cargo-nextest list a harness-less binary by
            // invoking it with `--list`; answer in the libtest format instead
            // of starting the event loop.
            if std::env::args().any(|arg| arg == "--list") {
                for test in TESTS {
                    println!("{}: test", test.name);
                }
                return Ok(());
            }

            $crate::test::__private::run(TESTS);
            Ok(())
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __verso_test_internal_collect_test {
    ($name:expr) => {
        $crate::test::__private::VersoBasedTest {
            name: stringify!($name),
            function: $crate::test::__private::TestFunction::OneOff($name),
        }
    };
}

#[doc(hidden)]
// This module is forked from winit-test.
pub mod __private {
    use winit::application::ApplicationHandler;
    use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};

    use std::any::Any;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::time::Instant;

    use crate::Verso;
    use crate::verso::EventLoopProxyMessage;

    pub type Context = ();

    struct State {
        passed: i32,
        panics: Vec<(&'static str, Box<dyn Any + Send>)>,
        start: Instant,
        run: bool,
    }

    struct TestApp {
        tests: &'static [VersoBasedTest],
        verso: Option<Verso>,
        proxy: EventLoopProxy<EventLoopProxyMessage>,
        state: State,
    }

    impl ApplicationHandler<EventLoopProxyMessage> for TestApp {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.verso = Some(Verso::new(event_loop, self.proxy.clone()));
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            // Run the tests (and terminate) once the event loop is up and
            // running. `ActiveEventLoop::exit` is unreliable when called from
            // inside a callback, and servo's background threads keep running
            // anyway, so flush output and terminate the process directly.
            if !self.state.run {
                run_internal(self.tests, &mut self.state, event_loop);
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let code = if self.state.panics.is_empty() { 0 } else { 1 };
                std::process::exit(code);
            }
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window_id: winit::window::WindowId,
            event: winit::event::WindowEvent,
        ) {
            if let Some(v) = self.verso.as_mut() {
                v.handle_window_event(event_loop, window_id, event);
            }
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: EventLoopProxyMessage) {
            if let Some(v) = self.verso.as_mut() {
                match event {
                    EventLoopProxyMessage::Wake => v.request_redraw(event_loop),
                    EventLoopProxyMessage::IpcMessage(message) => {
                        v.handle_incoming_webview_message(event_loop, *message);
                    }
                }
            }
        }
    }

    /// Run a set of tests using a Winit context.
    pub fn run(tests: &'static [VersoBasedTest]) {
        // Never show a window during tests.
        // SAFETY: this runs before any other thread is spawned by the test harness.
        unsafe {
            std::env::set_var("VERSO_HEADLESS", "1");
        }

        // Watchdog: never let a hung servo instance block the run forever.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(180));
            eprintln!("test harness timed out after 180s");
            std::process::exit(124);
        });

        let event_loop = EventLoop::<EventLoopProxyMessage>::with_user_event()
            .build()
            .expect("Failed to build event loop");
        let proxy = event_loop.create_proxy();

        println!("\nRunning {} tests...", tests.len());
        let mut app = TestApp {
            tests,
            verso: None,
            proxy,
            state: State {
                passed: 0,
                panics: vec![],
                start: Instant::now(),
                run: false,
            },
        };

        event_loop
            .run_app(&mut app)
            .expect("Event loop failed to run");

        if app.state.panics.is_empty() {
            println!("test result: ok");
        } else {
            println!("test result: FAILED");
            std::process::exit(1);
        }
    }

    /// Run a set of tests using a Winit context.
    fn run_internal(tests: &'static [VersoBasedTest], state: &mut State, elwt: &ActiveEventLoop) {
        if state.run {
            return;
        }
        state.run = true;

        for test in tests {
            print!("test {} ... ", test.name);

            match test.function {
                TestFunction::OneOff(f) => match catch_unwind(AssertUnwindSafe(move || f(elwt))) {
                    Ok(()) => {
                        println!("ok");
                        state.passed += 1;
                    }

                    Err(e) => {
                        println!("FAILED");
                        state.panics.push((test.name, e));
                    }
                },
            }
        }

        let failures = state.panics.len();
        println!();
        if !state.panics.is_empty() {
            println!("failures:\n");
            for (name, e) in state.panics.drain(..) {
                println!("---- {} panic ----", name);

                if let Some(s) = e.downcast_ref::<&'static str>() {
                    println!("{}", s);
                } else if let Some(s) = e.downcast_ref::<String>() {
                    println!("{}", s);
                } else {
                    println!("unknown panic type");
                }

                println!();
            }
        }

        println!(
            ". {} passed; {} failed; finished in {:?}",
            state.passed,
            failures,
            state.start.elapsed()
        );
    }

    pub struct VersoBasedTest {
        pub name: &'static str,
        pub function: TestFunction,
    }

    pub enum TestFunction {
        OneOff(fn(&ActiveEventLoop)),
    }
}
