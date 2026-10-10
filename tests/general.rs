use versoview::verso_test;
use versoview::winit::event_loop::ActiveEventLoop;

fn smoke(_elwt: &ActiveEventLoop) {}
fn other_smoke(_elwt: &ActiveEventLoop) {}

verso_test!(smoke, other_smoke);
