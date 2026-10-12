//! Apex entry point: argument parsing, logging and window creation.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use apex_ui::{ApexApp, StartupArgs};
use clap::Parser;

mod ipc;
#[cfg(target_os = "macos")]
mod macos;
mod open_requests;

#[derive(Parser, Debug)]
#[command(name = "apex", version, about = "Apex — a native, high-performance text editor", long_about = None)]
struct Cli {
    /// Files or folders to open. `file.rs:120:8` opens at line 120, column 8.
    #[arg(value_name = "PATH")]
    paths: Vec<String>,
    /// Open the paths in a new window.
    #[arg(long)]
    new_window: bool,
    /// Reuse an existing window if one is running.
    #[arg(long)]
    reuse_window: bool,
    /// Wait until the opened files are closed before exiting.
    #[arg(long)]
    wait: bool,
    /// Compare two files side by side.
    #[arg(long, num_args = 2, value_names = ["OLD", "NEW"])]
    diff: Option<Vec<String>>,
    /// Log verbosity (error, warn, info, debug, trace). Also honours APEX_LOG.
    #[arg(long, default_value = "warn")]
    log: String,
}

/// Parse `path[:line[:col]]`, being careful with Windows drive letters.
fn parse_path_arg(arg: &str) -> (PathBuf, Option<usize>, Option<usize>) {
    if std::path::Path::new(arg).exists() {
        return (PathBuf::from(arg), None, None);
    }
    let mut parts: Vec<&str> = arg.rsplitn(3, ':').collect();
    parts.reverse();
    let try_num = |s: &str| s.parse::<usize>().ok();
    match parts.as_slice() {
        [p, l, c] if try_num(l).is_some() && try_num(c).is_some() && !p.is_empty() => (PathBuf::from(p), try_num(l), try_num(c)),
        [p, l] if try_num(l).is_some() && !p.is_empty() => (PathBuf::from(p), try_num(l), None),
        [p, l, c] if try_num(c).is_some() => (PathBuf::from(format!("{p}:{l}")), try_num(c), None),
        _ => (PathBuf::from(arg), None, None),
    }
}

fn main() -> std::process::ExitCode {
    // Finder may still pass a process serial number on old systems; it is not an argument.
    let argv = std::env::args_os().filter(|a| !a.to_string_lossy().starts_with("-psn_"));
    let cli = Cli::parse_from(argv);
    let filter = std::env::var("APEX_LOG").unwrap_or_else(|_| cli.log.clone());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(filter).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")))
        .with_writer(std::io::stderr)
        .init();

    let mut args = StartupArgs::default();
    for p in &cli.paths {
        let (path, line, col) = parse_path_arg(p);
        if path.is_dir() {
            args.folders.push(path);
        } else {
            args.files.push((path, line, col));
        }
    }
    if let Some(d) = &cli.diff
        && let [old, new] = d.as_slice()
    {
        args.diff = Some((PathBuf::from(old), PathBuf::from(new)));
    }

    // Hand the request to a running instance unless a new window was asked for.
    if !cli.new_window {
        let req = ipc::Request { files: args.files.clone(), folders: args.folders.clone(), diff: args.diff.clone(), wait: cli.wait, ..Default::default() };
        match ipc::try_send(req) {
            Ok(true) => return std::process::ExitCode::SUCCESS,
            Ok(false) => {}
            Err(e) => tracing::warn!("could not contact a running instance: {e}"),
        }
        if cli.reuse_window && (cli.paths.is_empty() && cli.diff.is_none()) {
            tracing::info!("no running instance to reuse; starting one");
        }
    }
    match ipc::serve() {
        Ok(p) => tracing::debug!("single-instance socket recorded in {}", p.display()),
        Err(e) => tracing::warn!("single-instance server not started: {e}"),
    }
    args.on_view_closed = Some(std::sync::Arc::new(|p: &std::path::Path| ipc::view_closed(p)));
    args.inbox = Some(open_requests::init());
    #[cfg(target_os = "macos")]
    macos::install();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(apex_project::paths::APP_NAME)
            .with_app_id(apex_project::paths::APP_ID)
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true)
            // Without an icon eframe installs egui's logo as the window and
            // (on macOS) Dock icon at run time, replacing the bundle's.
            .with_icon(app_icon()),
        centered: true,
        ..Default::default()
    };
    let creator = Box::new(|cc: &eframe::CreationContext<'_>| {
        open_requests::set_context(cc.egui_ctx.clone());
        Ok(Box::new(ApexApp::new(cc, args)) as Box<dyn eframe::App>)
    });
    let result = eframe::run_native(apex_project::paths::APP_NAME, options, creator);
    ipc::shutdown();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("apex: could not start the graphical interface: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icons/png/apex-512.png")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_args() {
        assert_eq!(parse_path_arg("src/main.rs:120:8"), (PathBuf::from("src/main.rs"), Some(120), Some(8)));
        assert_eq!(parse_path_arg("src/main.rs:120"), (PathBuf::from("src/main.rs"), Some(120), None));
        assert_eq!(parse_path_arg("C:\\x\\y.rs:7"), (PathBuf::from("C:\\x\\y.rs"), Some(7), None));
        assert_eq!(parse_path_arg("plain.txt"), (PathBuf::from("plain.txt"), None, None));
    }
}
