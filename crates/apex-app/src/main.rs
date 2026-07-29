//! Apex entry point: argument parsing, logging and window creation.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use apex_ui::{ApexApp, StartupArgs};
use clap::Parser;

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
    let cli = Cli::parse();
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
    if let Some(d) = &cli.diff {
        for p in d {
            args.files.push((PathBuf::from(p), None, None));
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(apex_project::paths::APP_NAME)
            .with_app_id(apex_project::paths::APP_ID)
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true),
        centered: true,
        ..Default::default()
    };
    match eframe::run_native(apex_project::paths::APP_NAME, options, Box::new(|cc| Ok(Box::new(ApexApp::new(cc, args))))) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("apex: could not start the graphical interface: {e}");
            std::process::ExitCode::from(2)
        }
    }
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
