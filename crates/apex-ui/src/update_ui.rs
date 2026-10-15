//! Help > Check for Updates: asks GitHub for the newest release
//! (`apex_project::updates`) on a worker thread and shows the answer in
//! `Dialog::Update`, with links to the release page and this platform's
//! download.

use apex_project::updates::{self, Release};
use crossbeam_channel::Receiver;

use crate::app::{ApexApp, Dialog, open_url};

/// The update dialog's state.
#[derive(Debug, Clone)]
pub(crate) enum UpdateCheck {
    /// Waiting for GitHub.
    Checking(Receiver<Result<Release, String>>),
    Done(Result<Release, String>),
}

impl ApexApp {
    /// Start a check and open the dialog that shows it.
    pub(crate) fn check_for_updates(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(updates::latest());
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        self.dialog = Some(Dialog::Update(UpdateCheck::Checking(rx)));
    }

    /// Draw `Dialog::Update`.
    pub(crate) fn show_update_dialog(&mut self, ctx: &egui::Context, mut check: UpdateCheck) {
        if let UpdateCheck::Checking(rx) = &check
            && let Ok(result) = rx.try_recv()
        {
            check = UpdateCheck::Done(result);
        }
        let current = env!("CARGO_PKG_VERSION");
        let mut open = None;
        let r = egui::Modal::new(egui::Id::new("update-check")).show(ctx, |ui| {
            ui.set_width(400.0);
            match &check {
                UpdateCheck::Checking(_) => {
                    ui.heading("Checking for updates");
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Asking GitHub for the latest release…");
                    });
                }
                UpdateCheck::Done(Err(e)) => {
                    ui.heading("Could not check for updates");
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                UpdateCheck::Done(Ok(rel)) if updates::is_newer(&rel.version, current) => {
                    ui.heading(format!("Apex {} is available", rel.version));
                    ui.label(format!("Released {}. You have {current}.", rel.date));
                    if ui.link("Release notes on GitHub").clicked() {
                        open = Some(rel.page.clone());
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Installed with Homebrew or a package repository? Update through that instead.")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(4.0);
                    let label = if rel.download.is_some() { "Download" } else { "Open Release Page" };
                    if ui.button(label).on_hover_text("Opens in your browser").clicked() {
                        open = Some(rel.download.clone().unwrap_or_else(|| rel.page.clone()));
                    }
                }
                UpdateCheck::Done(Ok(rel)) => {
                    ui.heading("Apex is up to date");
                    ui.label(format!("{current} is the latest version (released {}).", rel.date));
                    if ui.link("Release notes on GitHub").clicked() {
                        open = Some(rel.page.clone());
                    }
                }
            }
            ui.add_space(8.0);
            ui.button("Close").clicked()
        });
        if let Some(url) = open {
            open_url(&url);
        }
        self.dialog = if r.inner || r.should_close() { None } else { Some(Dialog::Update(check)) };
    }
}
