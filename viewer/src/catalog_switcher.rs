//! One window and database picker for both catalog schemas.
use crate::{assets::AssetViewer, catalogs, Viewer};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde_json::Value;
use crate::database_details::{Editor,Target};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
enum Selection {Assets, Images(Option<String>)}
enum Message {
    List(Result<Value,String>),
    Context(u64,Result<Value,String>),
}

struct Picker {
    rows:Vec<Value>, generation:u64, loading:bool, listing:bool, error:String,
    asset_name:String,details_requested:bool,
    tx:Sender<Message>, rx:Receiver<Message>,
}
impl Picker {
    fn empty()->Self {
        let (tx,rx)=crossbeam_channel::unbounded();
        Self{rows:vec![],generation:0,loading:false,listing:false,error:String::new(),asset_name:"Asset library".into(),details_requested:false,tx,rx}
    }
    fn refresh(&mut self,ctx:&egui::Context) {
        if self.listing {return;}
        self.listing=true;self.error.clear();
        let tx=self.tx.clone();let ctx=ctx.clone();
        std::thread::spawn(move || {
            let result=catalogs::cli(&["db".into(),"list".into()],None);
            let _=tx.send(Message::List(result));ctx.request_repaint();
        });
    }
    fn invalidate(&mut self) {self.generation+=1;self.loading=false;self.error.clear();}
    fn resolve(&mut self,id:Option<String>,ctx:&egui::Context) {
        self.invalidate();self.loading=true;
        let generation=self.generation;let tx=self.tx.clone();let ctx=ctx.clone();
        std::thread::spawn(move || {
            let result=catalogs::cli(&["db".into(),"context".into()],id.as_deref());
            let _=tx.send(Message::Context(generation,result));ctx.request_repaint();
        });
    }
    fn poll(&mut self)->Option<Value> {
        let mut context=None;
        while let Ok(message)=self.rx.try_recv() {
            match message {
                Message::List(result)=>{
                    self.listing=false;
                    match result {
                        Ok(value)=>{self.rows=value["catalogs"].as_array().cloned().unwrap_or_default();},
                        Err(_)=>self.error="Could not list image databases. Check image-atlas configuration, then refresh databases.".into(),
                    }
                },
                Message::Context(generation,result) if generation==self.generation=>{
                    self.loading=false;
                    match result {
                        Ok(value) if value["dsn"].as_str().is_some_and(|s|!s.is_empty())=>context=Some(value),
                        _=>self.error="Could not open the image database. Check its connection with image-atlas, then select it again.".into(),
                    }
                },
                _=>{},
            }
        }
        context
    }
    fn ui(&mut self,ui:&mut egui::Ui,current:&Selection,label:&str)->Option<Selection> {
        let mut selected=None;
        ui.horizontal_wrapped(|ui|{
            ui.strong("Database");
            egui::ComboBox::from_id_salt("shared-database-picker").width(300.)
                .selected_text(label).show_ui(ui,|ui|{
                    if ui.selectable_label(*current==Selection::Assets,format!("Assets — {}",self.asset_name)).clicked(){selected=Some(Selection::Assets);}
                    ui.separator();
                    for row in &self.rows {
                        let Some(id)=row["id"].as_str() else {continue;};
                        let selection=Selection::Images(Some(id.to_owned()));
                        let name=row["name"].as_str().unwrap_or("Unnamed image database");
                        if ui.selectable_label(*current==selection,format!("Images — {name}")).clicked(){selected=Some(selection);}
                    }
                }).response.on_hover_text("Switch image and asset databases in this window");
            if ui.add_enabled(!self.listing,egui::Button::new("Refresh databases")).clicked(){self.refresh(ui.ctx());}
            if ui.button("Database details").on_hover_text("Edit this database's display name, description and notes; inspect size and tables").clicked(){self.details_requested=true;}
            if self.loading {ui.spinner();ui.label("Opening database…");}
            else if self.listing {ui.spinner();ui.label("Loading databases…");}
        });
        if !self.error.is_empty(){ui.colored_label(ui.visuals().error_fg_color,&self.error);}
        if self.loading||self.listing {ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));}
        selected
    }
}

pub(super) struct Window {
    active:Selection, images:Option<Box<Viewer>>, assets:Option<Box<AssetViewer>>,
    asset_dsn:String, picker:Picker,editor:Editor,seen_metadata:HashSet<String>,
}
impl Window {
    pub(super) fn new(ctx:egui::Context)->Self {
        let asset_dsn=std::env::var("ASSET_CLI_VIEWER_DSN").or_else(|_|std::env::var("ASSET_CLI_DSN"))
            .unwrap_or_else(|_|"postgresql://imagecli@127.0.0.1:54329/assetcli".into());
        let start_assets=match std::env::var("CLI_VIEWER_START_MODE").as_deref() {
            Ok("assets")=>true,Ok("images")=>false,_=>std::env::var_os("ASSET_CLI_VIEWER_DSN").is_some(),
        };
        let mut window=Self{active:Selection::Assets,images:None,assets:None,asset_dsn,picker:Picker::empty(),editor:Editor::new(),seen_metadata:HashSet::new()};
        if start_assets {crate::install_fonts(&ctx);window.choose(Selection::Assets,&ctx);}
        else {
            let mut images=Viewer::new(ctx.clone());images.external_catalog_selector=true;
            window.active=image_selection(&images.catalog_panel.context);window.images=Some(Box::new(images));
        }
        window.picker.refresh(&ctx);
        window
    }
    fn choose(&mut self,selection:Selection,ctx:&egui::Context) {
        if self.assets.as_ref().is_some_and(|assets|assets.is_loading()) {return;}
        match selection {
            Selection::Assets=>{
                self.picker.invalidate();
                self.assets.get_or_insert_with(||Box::new(AssetViewer::open(self.asset_dsn.clone(),ctx.clone())));
                self.active=Selection::Assets;
            },
            Selection::Images(id)=>{
                if let Some(images)=&self.images {
                    if images.catalog_panel.context["id"].as_str()==id.as_deref() {
                        self.picker.invalidate();self.active=image_selection(&images.catalog_panel.context);return;
                    }
                }
                self.picker.resolve(id,ctx);
            },
        }
    }
    fn open_images(&mut self,context:Value,ctx:&egui::Context) {
        if let Some(images)=&mut self.images {images.switch_catalog(ctx.clone(),context);}
        else {self.images=Some(Box::new(Viewer::with_catalog(ctx.clone(),context)));}
        let images=self.images.as_mut().unwrap();images.external_catalog_selector=true;
        self.active=image_selection(&images.catalog_panel.context);
    }
    fn target(&self)->Option<Target> {
        if self.active==Selection::Assets {Some(Target{kind:"assets".into(),id:None,dsn:self.asset_dsn.clone(),name:self.picker.asset_name.clone()})}
        else {
            let context=&self.images.as_ref()?.catalog_panel.context;
            let dsn=context["dsn"].as_str()?.to_owned();
            if dsn.is_empty(){return None;}
            Some(Target{kind:"images".into(),id:context["id"].as_str().map(str::to_owned),dsn,name:context["name"].as_str().unwrap_or("Image database").into()})
        }
    }
    fn apply_metadata(&mut self,target:Target,metadata:Value) {
        let Some(name)=metadata["name"].as_str() else {return;};
        if target.kind=="assets" {self.picker.asset_name=name.into();}
        else {
            for row in &mut self.picker.rows {if row["id"].as_str()==target.id.as_deref(){row["name"]=name.into();}}
            if let Some(images)=&mut self.images {
                if images.catalog_panel.context["id"].as_str()==target.id.as_deref()&&images.catalog_panel.context["dsn"].as_str()==Some(target.dsn.as_str()) {
                    images.catalog_panel.context["name"]=name.into();
                }
            }
        }
    }
    fn draw(&mut self,ui:&mut egui::Ui) {
        if let Some(assets)=&mut self.assets {assets.background(ui.ctx());}
        let loading=self.assets.as_ref().is_some_and(|assets|assets.is_loading());
        for (target,metadata) in self.editor.poll(){self.apply_metadata(target,metadata);}
        if !loading {if let Some(context)=self.picker.poll(){self.open_images(context,ui.ctx());}}
        if self.active!=Selection::Assets {
            if let Some(images)=&self.images {self.active=image_selection(&images.catalog_panel.context);}
        }
        if let Some(target)=self.target() {if self.seen_metadata.insert(target.key()){self.editor.load_label(target,ui.ctx());}}
        let label=if self.active==Selection::Assets {format!("Assets — {}",self.picker.asset_name)}
            else {format!("Images — {}",self.images.as_ref().and_then(|v|v.catalog_panel.context["name"].as_str()).unwrap_or("Image database"))};
        let mut selected=None;
        egui::Panel::top("shared-database-header").show(ui,|ui|{
            ui.add_enabled_ui(!loading,|ui|{selected=self.picker.ui(ui,&self.active,&label);});
        });
        if let Some(selection)=selected {self.choose(selection,ui.ctx());}
        if std::mem::take(&mut self.picker.details_requested) {
            if let Some(target)=self.target(){self.editor.show(target,ui.ctx());}
            else {self.picker.error="Wait for the database connection before opening details.".into();}
        }
        // Each mode owns only the remaining viewport, including its background.
        let content=ui.available_rect_before_wrap();
        ui.scope_builder(egui::UiBuilder::new().id_salt("catalog-content").max_rect(content),|ui|{
            ui.set_clip_rect(content.intersect(ui.clip_rect()));
            if self.active==Selection::Assets {
                if let Some(assets)=&mut self.assets {assets.draw(ui);}
            } else if let Some(images)=&mut self.images {images.draw(ui);}
        });
        self.editor.ui(ui.ctx());
        if let Some(assets)=&mut self.assets {assets.loading_dialog(ui.ctx());}
    }
}
fn image_selection(context:&Value)->Selection {Selection::Images(context["id"].as_str().map(str::to_owned))}
impl eframe::App for Window {
    fn ui(&mut self,ui:&mut egui::Ui,_frame:&mut eframe::Frame){self.draw(ui);}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text_position(output:&egui::FullOutput,label:&str)->egui::Pos2 {
        output.shapes.iter().find_map(|s|match &s.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text==label=>Some(t.pos+t.galley.size()/2.),_=>None,
        }).unwrap_or_else(||panic!("Missing {label}"))
    }
    fn click(pos:egui::Pos2)->Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos),
            egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE},
            egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE}]
    }
    #[test]
    fn image_background_never_covers_picker_and_mouse_can_switch_both_ways() {
        for dark in [true,false] {for width in [650.,1250.] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            ctx.style_mut_of(egui::Theme::Dark,|s|s.animation_time=0.);ctx.style_mut_of(egui::Theme::Light,|s|s.animation_time=0.);
            let context=serde_json::json!({"id":"fixture","name":"Fixture","dsn":"","readonly":true});
            let mut images=Viewer::with_catalog(ctx.clone(),context.clone());images.external_catalog_selector=true;
            let asset_dsn="host=127.0.0.1 port=1 dbname=unavailable connect_timeout=1".to_owned();
            let asset_target=Target{kind:"assets".into(),id:None,dsn:asset_dsn.clone(),name:"Asset library".into()};
            let mut picker=Picker::empty();picker.rows=vec![context.clone()];
            let mut window=Window{active:image_selection(&context),images:Some(Box::new(images)),assets:None,asset_dsn,picker,editor:Editor::new(),seen_metadata:HashSet::from([asset_target.key()])};
            let frame=|events:Vec<egui::Event>,window:&mut Window|ctx.run_ui(egui::RawInput{
                screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,850.))),events,..Default::default()
            },|ui|window.draw(ui));
            frame(vec![],&mut window);
            let output=frame(vec![],&mut window);
            let pos=text_position(&output,"Images — Fixture");
            let header=output.shapes.iter().position(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text=="Images — Fixture")).unwrap();
            for shape in &output.shapes[header+1..] {
                if let egui::epaint::Shape::Rect(rect)=&shape.shape {
                    assert!(!(rect.fill.a()>0&&rect.rect.contains(pos)&&shape.clip_rect.contains(pos)),"Later image background hides database picker");
                }
            }
            frame(click(pos),&mut window);
            let output=frame(vec![],&mut window);
            frame(click(text_position(&output,"Assets — Asset library")),&mut window);
            assert_eq!(window.active,Selection::Assets);
            let output=frame(vec![],&mut window);
            frame(click(text_position(&output,"Assets — Asset library")),&mut window);
            let output=frame(vec![],&mut window);
            frame(click(text_position(&output,"Images — Fixture")),&mut window);
            assert_eq!(window.active,Selection::Images(Some("fixture".into())));
        }}
    }
    #[test]
    fn stale_database_reply_cannot_switch_back_after_selecting_assets() {
        let mut picker=Picker::empty();picker.generation=1;picker.loading=true;
        picker.invalidate();
        picker.tx.send(Message::Context(1,Ok(serde_json::json!({"id":"old","dsn":"private"})))).unwrap();
        assert!(picker.poll().is_none());assert!(!picker.loading);
        picker.tx.send(Message::Context(2,Err("private connection details".into()))).unwrap();
        assert!(picker.poll().is_none());assert!(!picker.error.contains("private"));
        picker.tx.send(Message::Context(2,Ok(serde_json::json!({"id":"new","dsn":"private"})))).unwrap();
        assert_eq!(picker.poll().unwrap()["id"],"new");
    }
    #[test]
    fn picker_shows_both_schemas_and_remains_interactive_while_opening() {
        for dark in [true,false] {for width in [650.,1250.] {
            let ctx=egui::Context::default();
            ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut picker=Picker::empty();picker.loading=true;
            picker.rows=vec![serde_json::json!({"id":"photos","name":"Photographs"})];
            ctx.style_mut_of(egui::Theme::Dark,|s|s.animation_time=0.);
            ctx.style_mut_of(egui::Theme::Light,|s|s.animation_time=0.);
            let mut frame=|events:Vec<egui::Event>| {
                let mut selected=None;
                let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,600.))),events,..Default::default()},|ui|{
                    selected=picker.ui(ui,&Selection::Assets,"Assets — Asset library");
                });
                (output,selected)
            };
            let position=|output:&egui::FullOutput,label:&str|output.shapes.iter().find_map(|s|match &s.shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text==label=>Some(t.pos+t.galley.size()/2.),_=>None,
            }).unwrap_or_else(||panic!("Missing {label}"));
            let click=|pos:egui::Pos2|vec![egui::Event::PointerMoved(pos),
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE},
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE}];
            frame(vec![]);
            let (output,_)=frame(vec![]);
            frame(click(position(&output,"Assets — Asset library")));
            let (output,_)=frame(vec![]);
            position(&output,"Assets — Asset library");
            let pos=position(&output,"Images — Photographs");
            let (_,selected)=frame(click(pos));
            assert_eq!(selected,Some(Selection::Images(Some("photos".into()))));
            assert!(picker.loading);
        }}
    }

    #[test]
    fn one_window_preserves_each_view_and_uses_both_real_catalogs() {
        let Ok(context)=std::env::var("SHARED_VIEWER_TEST_CONTEXT") else {return;};
        let context:Value=serde_json::from_str(&context).unwrap();
        let asset_dsn=std::env::var("ASSET_CLI_TEST_DSN").unwrap();
        let ctx=egui::Context::default();
        let images=Box::new(Viewer::with_catalog(ctx.clone(),context.clone()));
        let mut window=Window{active:image_selection(&context),images:Some(images),assets:None,asset_dsn,picker:Picker::empty(),editor:Editor::new(),seen_metadata:HashSet::new()};
        window.images.as_mut().unwrap().external_catalog_selector=true;
        window.images.as_mut().unwrap().search="Preserve image search".into();
        let image_address=window.images.as_ref().unwrap().as_ref() as *const Viewer;
        window.choose(Selection::Assets,&ctx);
        assert_eq!(window.active,Selection::Assets);
        let asset_address=window.assets.as_ref().unwrap().as_ref() as *const AssetViewer;
        for dark in [true,false] {for width in [900.,1600.] {
            ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            for selection in [image_selection(&context),Selection::Assets] {
                window.choose(selection,&ctx);
                let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,850.))),..Default::default()},|ui|window.draw(ui));
                assert!(!output.shapes.is_empty());
                assert_eq!(window.images.as_ref().unwrap().as_ref() as *const Viewer,image_address);
                assert_eq!(window.assets.as_ref().unwrap().as_ref() as *const AssetViewer,asset_address);
                assert_eq!(window.images.as_ref().unwrap().search,"Preserve image search");
            }
        }}
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(30);
        while !window.images.as_ref().unwrap().stats_known {
            window.images.as_mut().unwrap().receive(&ctx);
            assert!(std::time::Instant::now()<deadline,"Image database did not load");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(window.images.as_ref().unwrap().catalog_panel.context["readonly"].as_bool().unwrap());
    }
}
