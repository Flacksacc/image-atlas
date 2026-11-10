//! Reference-image selection shared by local and remote browsing.
use eframe::egui;
use std::path::PathBuf;

pub enum Action { Select(PathBuf), Clear, Error(String) }

#[derive(Default)]
pub struct Source {
    pub path: Option<PathBuf>,
    picker: Option<crossbeam_channel::Receiver<Option<PathBuf>>>,
}

impl Source {
    pub fn set(&mut self, path: PathBuf) -> Result<(), String> {
        let path = path.canonicalize().map_err(|_| "Choose an existing image file")?;
        let metadata = path.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() { return Err("Choose an image file, rather than a folder".into()); }
        if metadata.len() > 32 * 1024 * 1024 { return Err("Reference images must be 32 MiB or smaller".into()); }
        self.path = Some(path);
        Ok(())
    }

    pub fn key(&self) -> Option<String> {
        self.path.as_ref().map(|path| {
            let metadata = path.metadata().ok();
            format!("reference:{}|{}|{:?}", path.display(), metadata.as_ref().map(|value| value.len()).unwrap_or(0),
                metadata.and_then(|value| value.modified().ok()))
        })
    }

    pub fn controls(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        if let Some(selected) = self.picker.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.picker = None;
            action = selected.map(Action::Select);
        }
        let dropped = ui.input(|input| input.raw.dropped_files.clone());
        if dropped.len() > 1 {
            action = Some(Action::Error("Drop one reference image at a time".into()));
        } else if let Some(file) = dropped.first() {
            action = Some(match file.path.clone() {
                Some(path) => Action::Select(path),
                None => Action::Error("Drop an image file from your computer".into()),
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.picker.is_none(), egui::Button::new("Search by image…"))
                .on_hover_text("Choose a reference image; it does not need to be in this catalog").clicked() {
                let (tx, rx) = crossbeam_channel::bounded(1);
                self.picker = Some(rx);
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let path = rfd::FileDialog::new().set_title("Search ImageAtlas by image")
                        .add_filter("Images", &["jpg", "jpeg", "png", "webp", "tif", "tiff", "bmp", "gif", "avif", "heic", "heif"])
                        .pick_file();
                    let _ = tx.send(path);
                    ctx.request_repaint();
                });
            }
            if let Some(path) = &self.path {
                ui.add(egui::Label::new(path.file_name().unwrap_or_default().to_string_lossy()).truncate())
                    .on_hover_text(path.display().to_string());
                if ui.small_button("Clear image search").clicked() { action = Some(Action::Clear); }
            } else {
                ui.small("Or drop an image here to find similar pictures.");
            }
        });
        action
    }
}
