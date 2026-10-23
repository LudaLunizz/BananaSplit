//! Native Windows desktop window for the mixer page.
//!
//! The audio engine stays in the parent process. This child owns the Win32
//! window and embeds Microsoft WebView2 through Wry, so closing or restarting
//! the UI cannot interrupt audio processing.

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

pub fn run(url: &str) -> Result<(), String> {
    if !(url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")) {
        return Err("the desktop window only accepts BananaSplit's local mixer page".into());
    }

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("BananaSplit")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)
        .map_err(|e| format!("could not create the window: {e}"))?;
    let _webview = WebViewBuilder::new()
        .with_url(url)
        .build(&window)
        .map_err(|e| format!("could not start WebView2: {e}"))?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, window_id } = event {
            if window_id == window.id() {
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}
