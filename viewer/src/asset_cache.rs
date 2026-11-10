//! Disk cache accounting and deletion stay off the UI thread.
use crossbeam_channel::{Receiver,Sender};
use eframe::egui;
use serde_json::Value;
use std::time::Duration;
#[cfg(not(test))]
use std::{path::Path,process::Command};

#[cfg(not(test))]
fn call(action:&str)->Result<Value,String> {
    let executable=std::env::var_os("ASSET_CLI_BIN").unwrap_or_else(||Path::new(env!("CARGO_MANIFEST_DIR")).join("../../asset-cli/.venv/Scripts/asset-cli.exe").into_os_string());
    let mut command=Command::new(executable);command.args(["cache",action]);
    crate::actions::scrub_environment(&mut command);
    let output=crate::bridge::output(&mut command,Duration::from_secs(if action=="clear"{1800}else{120}),1024*1024)?;
    let value:Value=serde_json::from_slice(&output.stdout).map_err(|_|"Cache service did not return a valid response".to_owned())?;
    if let Some(error)=value["error"].as_str(){return Err(error.to_owned());}
    if !output.status.success(){return Err("Cache operation failed; refresh its size and retry.".into());}
    Ok(value)
}

fn size(bytes:u64)->String {
    let mut value=bytes as f64;let mut unit="B";
    for next in ["KiB","MiB","GiB","TiB"] {if value<1024. {break;}value/=1024.;unit=next;}
    format!("{value:.1} {unit}")
}

pub(super) struct Panel {
    data:Value,busy:bool,clearing:bool,attempted:bool,message:String,error:String,
    tx:Sender<Result<Value,String>>,rx:Receiver<Result<Value,String>>,
    #[cfg(test)] actions:Vec<String>,
    #[cfg(test)] clear_rect:egui::Rect,
}
impl Panel {
    pub fn new()->Self {
        let (tx,rx)=crossbeam_channel::unbounded();
        Self{data:Value::Null,busy:false,clearing:false,attempted:false,message:String::new(),error:String::new(),tx,rx,
            #[cfg(test)] actions:vec![],#[cfg(test)] clear_rect:egui::Rect::NOTHING}
    }
    fn send(&mut self,action:&str,ctx:&egui::Context) {
        if self.busy{return;}
        self.busy=true;self.clearing=action=="clear";self.attempted=true;self.error.clear();self.message.clear();
        #[cfg(not(test))] {
            let action=action.to_owned();let tx=self.tx.clone();let ctx=ctx.clone();
            std::thread::spawn(move ||{let _=tx.send(call(&action));ctx.request_repaint();});
        }
        #[cfg(test)] {let _=ctx;self.actions.push(action.into());}
    }
    pub fn refresh(&mut self,ctx:&egui::Context){self.send("status",ctx);}
    pub fn clearing(&self)->bool {self.clearing}
    pub fn poll(&mut self,ctx:&egui::Context)->bool {
        let mut cleared=false;
        while let Ok(reply)=self.rx.try_recv() {
            self.busy=false;self.clearing=false;
            match reply {
                Ok(data)=>{
                    cleared=data["cleared"]==true;
                    if cleared {self.message=format!("Cleared {} of local cache.",size(data["freed_bytes"].as_u64().unwrap_or(0)));}
                    self.data=data;
                },Err(error)=>self.error=error,
            }
        }
        if self.busy {ctx.request_repaint_after(Duration::from_millis(50));}
        cleared
    }
    pub fn ui(&mut self,ui:&mut egui::Ui,can_clear:bool) {
        let label=if self.data.is_null(){"Downloaded cache".into()}else{format!("Downloaded cache · {}",size(self.data["bytes"].as_u64().unwrap_or(0)))};
        egui::CollapsingHeader::new(label).id_salt("asset-downloaded-cache").show(ui,|ui|{
            if !self.attempted {self.refresh(ui.ctx());}
            if let Some(groups)=self.data["groups"].as_array() {
                for group in groups {
                    ui.label(format!("{}: {} · {} files",group["name"].as_str().unwrap_or("Cache"),size(group["bytes"].as_u64().unwrap_or(0)),group["files"]));
                }
                ui.add(egui::Label::new(self.data["root"].as_str().unwrap_or("")).truncate()).on_hover_text(self.data["root"].as_str().unwrap_or(""));
            }
            if self.busy {ui.horizontal(|ui|{ui.spinner();ui.label(if self.clearing{"Closing managed previews and clearing cache…"}else{"Calculating cache size…"});});}
            ui.horizontal_wrapped(|ui|{
                if ui.add_enabled(!self.busy,egui::Button::new("Refresh cache size")).clicked(){self.refresh(ui.ctx());}
                let button=ui.add_enabled(!self.busy&&can_clear,egui::Button::new("Clear downloaded cache"));
                #[cfg(test)] {self.clear_rect=button.rect;}
                if button.clicked(){self.send("clear",ui.ctx());}
            });
            ui.small("Clearing closes managed previews. The catalog, retained packs and LAN archives are kept; viewing an asset again retrieves its files.");
            if !self.message.is_empty(){ui.label(&self.message);}
            if !self.error.is_empty(){ui.colored_label(ui.visuals().error_fg_color,&self.error);}
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clear_is_async_and_errors_leave_accounting_available() {
        let ctx=egui::Context::default();let mut panel=Panel::new();
        panel.send("clear",&ctx);assert!(panel.clearing());assert_eq!(panel.actions,["clear"]);
        panel.send("clear",&ctx);assert_eq!(panel.actions.len(),1);
        panel.tx.send(Err("Cache is in use".into())).unwrap();assert!(!panel.poll(&ctx));
        assert!(!panel.clearing()&&panel.error.contains("in use"));
        panel.refresh(&ctx);panel.tx.send(Ok(serde_json::json!({"bytes":1024,"files":2,"groups":[]}))).unwrap();
        panel.poll(&ctx);assert_eq!(panel.data["bytes"],1024);
        panel.send("clear",&ctx);panel.tx.send(Ok(serde_json::json!({"cleared":true,"freed_bytes":1024,"bytes":0,"groups":[]}))).unwrap();
        assert!(panel.poll(&ctx));assert!(panel.message.contains("1.0 KiB"));
    }
    #[test]
    fn cache_section_renders_sizes_and_disables_delete_during_asset_work() {
        for dark in [true,false] {for width in [360.,1100.] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            ctx.style_mut_of(egui::Theme::Dark,|s|s.animation_time=0.);ctx.style_mut_of(egui::Theme::Light,|s|s.animation_time=0.);
            let mut panel=Panel::new();panel.attempted=true;
            panel.data=serde_json::json!({"bytes":2048,"root":"D:/AssetCLI","groups":[{"name":"Downloaded asset files","bytes":2048,"files":3}]});
            let frame=|panel:&mut Panel,events:Vec<egui::Event>,can_clear:bool|ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,600.))),events,..Default::default()},|ui|panel.ui(ui,can_clear));
            let position=|out:&egui::FullOutput|out.shapes.iter().find_map(|s|match &s.shape{egui::epaint::Shape::Text(t) if t.galley.job.text.starts_with("Downloaded cache")=>Some(t.pos+t.galley.size()/2.),_=>None}).unwrap();
            let click=|pos|vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()},egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}];
            frame(&mut panel,vec![],false);let output=frame(&mut panel,vec![],false);
            frame(&mut panel,click(position(&output)),false);
            let output=frame(&mut panel,vec![],false);
            assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text.contains("2.0 KiB · 3 files"))));
            let rect=panel.clear_rect;assert!(rect.max.x<=width);
            frame(&mut panel,click(rect.center()),false);assert!(panel.actions.is_empty());
            frame(&mut panel,vec![],true);let rect=panel.clear_rect;
            frame(&mut panel,click(rect.center()),true);assert_eq!(panel.actions,["clear"]);
        }}
    }
}
