use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddingModel {
    #[default]
    OpenClip,
    EmbeddingGemma2,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selector_can_switch_models_in_both_themes_and_window_sizes() {
        for theme in [egui::Theme::Dark,egui::Theme::Light] { for width in [640.,1200.] {
            let ctx=egui::Context::default();ctx.set_theme(theme);
            let mut model=EmbeddingModel::default();
            let frame=|model:&mut EmbeddingModel,events| ctx.run_ui(egui::RawInput {
                screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.))),events,..Default::default()
            },|ui| {model.controls(ui);});
            let click=|model:&mut EmbeddingModel,label:&str| {
                let output=frame(model,vec![]);
                let pos=output.shapes.iter().find_map(|shape|match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text==label => Some(text.pos+text.galley.size()/2.),_=>None
                }).unwrap_or_else(||panic!("Missing model selector option: {label}"));
                assert!(pos.x<width);
                frame(model,vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
                frame(model,vec![egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
            };
            click(&mut model,"OpenCLIP");click(&mut model,"EmbeddingGemma 2");
            assert_eq!(model,EmbeddingModel::EmbeddingGemma2);
            click(&mut model,"EmbeddingGemma 2");click(&mut model,"OpenCLIP");
            assert_eq!(model,EmbeddingModel::OpenClip);
        }}
    }
}

impl EmbeddingModel {
    pub fn key(self) -> &'static str {
        match self { Self::OpenClip => "openclip", Self::EmbeddingGemma2 => "embeddinggemma2" }
    }
    pub fn label(self) -> &'static str {
        match self { Self::OpenClip => "OpenCLIP", Self::EmbeddingGemma2 => "EmbeddingGemma 2" }
    }
    pub fn id(self) -> &'static str {
        match self { Self::OpenClip => "open_clip:ViT-B-32:laion2b_s34b_b79k", Self::EmbeddingGemma2 => "google:embeddinggemma-2:512" }
    }
    pub fn prefix(self) -> &'static str {
        match self { Self::OpenClip => "", Self::EmbeddingGemma2 => "gemma_" }
    }
    pub fn search_sql(self, sql: &str) -> String {
        if self == Self::OpenClip { return sql.to_owned(); }
        sql.replace("i.embedding", "i.gemma_embedding")
            .replace(Self::OpenClip.id(), self.id())
    }
    pub fn controls(&mut self, ui: &mut egui::Ui) -> bool {
        let previous = *self;
        ui.horizontal_wrapped(|ui| {
            ui.label("Search model");
            egui::ComboBox::from_id_salt("embedding-model").selected_text(self.label()).show_ui(ui, |ui| {
                for model in [Self::OpenClip, Self::EmbeddingGemma2] {
                    ui.selectable_value(self, model, model.label());
                }
            });
        });
        previous != *self
    }
}
