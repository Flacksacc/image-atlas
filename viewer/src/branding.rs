//! Shared branding for local, asset, and remote viewer windows.
use eframe::egui;

const LOGO: &[u8] = include_bytes!("../../src/image_atlas/branding/image-atlas.png");
const ICON: &[u8] = include_bytes!("../../src/image_atlas/branding/image-atlas-icon.png");
const REPOSITORY: &str = "https://github.com/Flacksacc/image-atlas";

pub fn icon() -> egui::IconData {
    let image = image::load_from_memory(ICON).expect("Invalid bundled ImageAtlas icon").into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData { rgba: image.into_raw(), width, height }
}

fn logo(ctx: &egui::Context) -> egui::TextureHandle {
    let id = egui::Id::new("imageatlas-logo");
    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return texture;
    }
    let image = image::load_from_memory(LOGO).expect("Invalid bundled ImageAtlas logo").into_rgba8();
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize], image.as_raw());
    let texture = ctx.load_texture("ImageAtlas logo", pixels, Default::default());
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

pub fn about_button(ui: &mut egui::Ui) {
    if ui.button("About ImageAtlas").clicked() {
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("imageatlas-about"), true));
    }
}

pub fn about_ui(ctx: &egui::Context) {
    let id = egui::Id::new("imageatlas-about");
    let mut open = ctx.data(|data| data.get_temp::<bool>(id)).unwrap_or(false);
    if !open { return; }
    let texture = logo(ctx);
    let mut close = false;
    egui::Window::new("About ImageAtlas").open(&mut open)
        .collapsible(false).resizable(false).default_width(350.0)
        .max_width((ctx.content_rect().width() - 32.0).max(200.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 100.0).max(120.0))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.image((texture.id(), egui::vec2(160.0, 160.0)));
                        ui.heading("ImageAtlas");
                        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                        ui.label("Find, explore, and organize your image library.");
                        ui.label("Local catalogs, visual search, and face review.");
                        ui.hyperlink_to("GitHub repository", REPOSITORY);
                        close = ui.button("Close").clicked();
                    });
                });
        });
    ctx.data_mut(|data| data.insert_temp(id, open && !close));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_window_icon_has_pixels_and_transparency() {
        let icon = icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }

    #[test]
    fn about_opens_renders_logo_and_closes_in_both_themes_and_sizes() {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            for size in [egui::vec2(640.0, 480.0), egui::vec2(1200.0, 800.0)] {
                let ctx = egui::Context::default();
                ctx.set_theme(theme);
                let frame = |events| ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    events, ..Default::default()
                }, |ui| { about_button(ui); about_ui(ui.ctx()); });
                let find = |output: &egui::FullOutput, label: &str| output.shapes.iter().find_map(|shape| {
                    if let egui::epaint::Shape::Text(text) = &shape.shape {
                        if text.galley.job.text == label { return Some(text.pos + text.galley.size() / 2.0); }
                    }
                    None
                }).unwrap_or_else(|| panic!("Missing {label}"));
                let click = |pos| {
                    frame(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
                    frame(vec![egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }])
                };
                let output = frame(vec![]);
                click(find(&output, "About ImageAtlas"));
                let output = frame(vec![]);
                find(&output, "ImageAtlas");
                find(&output, &format!("Version {}", env!("CARGO_PKG_VERSION")));
                let texture = logo(&ctx).id();
                assert!(output.shapes.iter().any(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect) => rect.fill_texture_id() == texture,
                    egui::epaint::Shape::Mesh(mesh) => mesh.texture_id == texture,
                    _ => false,
                }));
                click(find(&output, "Close"));
                assert_eq!(ctx.data(|data| data.get_temp::<bool>(egui::Id::new("imageatlas-about"))), Some(false));
            }
        }
    }
}
