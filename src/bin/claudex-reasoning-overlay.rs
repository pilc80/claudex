use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

const OVERLAY_MARGIN_X: f64 = 48.0;
const OVERLAY_MARGIN_Y: f64 = 80.0;

struct OverlayArgs {
    url: String,
    parent_pid: Option<u32>,
    ready_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = parse_args()?;

    let event_loop = EventLoop::new();
    let monitor = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next());

    let size = LogicalSize::new(520.0, 320.0);
    let position = active_screen_top_right_position(size.width).or_else(|| {
        monitor.as_ref().map(|monitor| {
            let monitor_pos = monitor.position();
            let monitor_size = monitor.size();
            LogicalPosition::new(
                monitor_pos.x as f64 + monitor_size.width as f64 - size.width - OVERLAY_MARGIN_X,
                monitor_pos.y as f64 + OVERLAY_MARGIN_Y,
            )
        })
    });

    let mut builder = WindowBuilder::new()
        .with_title("Claudex Reasoning")
        .with_inner_size(size)
        .with_always_on_top(true)
        .with_decorations(false)
        .with_transparent(true)
        .with_focused(false)
        .with_focusable(false)
        .with_visible_on_all_workspaces(true)
        .with_resizable(true);

    if let Some(position) = position {
        builder = builder.with_position(position);
    }

    let window = builder.build(&event_loop)?;
    window.set_outer_position(position.unwrap_or(LogicalPosition::new(24.0, 48.0)));
    let _webview = WebViewBuilder::new()
        .with_url(&args.url)
        .with_focused(false)
        .with_transparent(true)
        .build(&window)?;

    if let Some(path) = &args.ready_file {
        std::fs::write(path, b"ready")?;
    }

    let parent_pid = args.parent_pid;
    let mut last_parent_check = Instant::now();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::MainEventsCleared => {
                if let Some(parent_pid) = parent_pid {
                    if last_parent_check.elapsed() >= Duration::from_secs(1) {
                        last_parent_check = Instant::now();
                        if !process_exists(parent_pid) {
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                }
            }
            _ => {}
        }
    });
}

#[cfg(target_os = "macos")]
fn active_screen_top_right_position(width: f64) -> Option<LogicalPosition<f64>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let marker = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(marker)?;
    let frame = screen.visibleFrame();
    Some(LogicalPosition::new(
        frame.origin.x + frame.size.width - width - OVERLAY_MARGIN_X,
        frame.origin.y + OVERLAY_MARGIN_Y,
    ))
}

#[cfg(not(target_os = "macos"))]
fn active_screen_top_right_position(_width: f64) -> Option<LogicalPosition<f64>> {
    None
}

fn parse_args() -> Result<OverlayArgs> {
    let mut url = None;
    let mut parent_pid = None;
    let mut ready_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = args.next(),
            "--parent-pid" => {
                parent_pid = args.next().and_then(|value| value.parse::<u32>().ok());
            }
            "--ready-file" => ready_file = args.next().map(PathBuf::from),
            value if url.is_none() => url = Some(value.to_string()),
            _ => {}
        }
    }

    Ok(OverlayArgs {
        url: url.context("usage: claudex-reasoning-overlay --url <overlay-url>")?,
        parent_pid,
        ready_file,
    })
}

fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
