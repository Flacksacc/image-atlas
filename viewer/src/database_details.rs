//! Local catalog metadata editor; database inspection and saves use private stdin.
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde_json::{json,Value};
use std::{collections::HashMap,path::Path,process::Command,time::Duration};

#[derive(Clone)]
pub(super) struct Target {pub kind:String,pub id:Option<String>,pub dsn:String,pub name:String}
impl Target {
    pub fn key(&self)->String {format!("{}:{}",self.kind,self.id.as_deref().unwrap_or(&self.dsn))}
    fn json(&self)->Value {json!({"kind":self.kind,"id":self.id,"dsn":self.dsn,"name":self.name})}
}

fn call(target:&Target,action:&str,fields:Value)->Result<Value,String> {
    let python=std::env::var_os("IMAGE_CLI_PYTHON").unwrap_or_else(||Path::new(env!("CARGO_MANIFEST_DIR")).join("../.venv/Scripts/python.exe").into_os_string());
    let mut command=Command::new(python);
    command.args(["-m","image_atlas.viewer_catalogs"]);
    crate::actions::scrub_environment(&mut command);
    if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
    let input=serde_json::to_vec(&json!({"target":target.json(),"action":action,"fields":fields})).map_err(|_|"Cannot prepare database request")?;
    let output=crate::bridge::output_with_input(&mut command,Duration::from_secs(45),4*1024*1024,input)?;
    if !output.status.success(){return Err("Database details service failed. Check image-atlas configuration.".into());}
    let value:Value=serde_json::from_slice(&output.stdout).map_err(|_|"Invalid database details response")?;
    if let Some(error)=value["error"].as_str(){return Err(error.to_owned());}
    Ok(value)
}

struct Reply {serial:u64,target:Target,edit:bool,reset_fields:bool,result:Result<Value,String>}
pub(super) struct Editor {
    open:bool,target:Option<Target>,serial:u64,pending:Option<u64>,loaded:bool,
    name:String,description:String,notes:String,details:Value,status:String,
    applied:HashMap<String,u64>,tx:Sender<Reply>,rx:Receiver<Reply>,
}
impl Editor {
    pub fn new()->Self {
        let (tx,rx)=crossbeam_channel::unbounded();
        Self{open:false,target:None,serial:0,pending:None,loaded:false,name:String::new(),description:String::new(),notes:String::new(),
            details:Value::Null,status:String::new(),applied:HashMap::new(),tx,rx}
    }
    fn send(&mut self,target:Target,action:&str,fields:Value,edit:bool,reset_fields:bool,ctx:&egui::Context) {
        self.serial+=1;let serial=self.serial;
        if edit {self.pending=Some(serial);self.status=if action=="save"{"Saving metadata…"}else{"Loading database details…"}.into();}
        let tx=self.tx.clone();let ctx=ctx.clone();let action=action.to_owned();
        std::thread::spawn(move || {
            let result=call(&target,&action,fields);
            let _=tx.send(Reply{serial,target,edit,reset_fields,result});ctx.request_repaint();
        });
    }
    pub fn load_label(&mut self,target:Target,ctx:&egui::Context) {self.send(target,"metadata",Value::Null,false,false,ctx);}
    pub fn show(&mut self,target:Target,ctx:&egui::Context) {
        self.open=true;self.name=target.name.clone();self.description.clear();self.notes.clear();self.details=Value::Null;self.loaded=false;
        self.target=Some(target.clone());self.send(target,"load",Value::Null,true,true,ctx);
    }
    pub fn poll(&mut self)->Vec<(Target,Value)> {
        let mut changes=vec![];
        while let Ok(reply)=self.rx.try_recv() {
            let current=reply.edit&&self.pending==Some(reply.serial);
            if current {self.pending=None;}
            match reply.result {
                Ok(value)=>{
                    let applied=self.applied.entry(reply.target.key()).or_default();
                    if reply.serial>=*applied {
                        *applied=reply.serial;changes.push((reply.target,value["metadata"].clone()));
                    }
                    if current {
                        if reply.reset_fields {
                            self.name=value["metadata"]["name"].as_str().unwrap_or_default().into();
                            self.description=value["metadata"]["description"].as_str().unwrap_or_default().into();
                            self.notes=value["metadata"]["notes"].as_str().unwrap_or_default().into();
                            if let Some(target)=&mut self.target {target.name=self.name.clone();}
                        }
                        self.loaded=true;
                        if !value["details"].is_null(){self.details=value["details"].clone();}
                        if !value["details_error"].is_null(){self.details=Value::Null;}
                        self.status=value["details_error"].as_str().unwrap_or(if value["details"].is_null(){"Metadata saved"}else{""}).into();
                    }
                },Err(error)=>{if current {self.status=error;}},
            }
        }
        changes
    }
    pub fn ui(&mut self,ctx:&egui::Context) {
        if !self.open {return;}
        let mut open=self.open;let mut save=false;let mut refresh=false;
        let title=format!("Database details — {}",self.target.as_ref().map(|t|t.name.as_str()).unwrap_or("Database"));
        egui::Window::new(title).id(egui::Id::new("shared-database-details")).open(&mut open)
            .default_width(560.).max_width((ctx.content_rect().width()-32.).max(240.)).show(ctx,|ui|{
            egui::ScrollArea::vertical().id_salt("database-details-scroll").max_height(ctx.content_rect().height()*0.75).show(ui,|ui|{
                ui.label("Display name");
                ui.add_enabled(self.pending.is_none(),egui::TextEdit::singleline(&mut self.name).char_limit(120).desired_width(f32::INFINITY));
                ui.label("Description");
                ui.add_enabled(self.pending.is_none(),egui::TextEdit::multiline(&mut self.description).char_limit(2000).desired_rows(2).desired_width(f32::INFINITY));
                ui.label("Notes");
                ui.add_enabled(self.pending.is_none(),egui::TextEdit::multiline(&mut self.notes).char_limit(8000).desired_rows(3).desired_width(f32::INFINITY));
                ui.horizontal_wrapped(|ui|{
                    save=ui.add_enabled(self.loaded&&self.pending.is_none()&&!self.name.trim().is_empty(),egui::Button::new("Save metadata")).clicked();
                    refresh=ui.add_enabled(self.pending.is_none(),egui::Button::new("Refresh details")).clicked();
                    if self.pending.is_some(){ui.spinner();}
                });
                if !self.status.is_empty(){ui.label(&self.status);}
                ui.small("Display metadata is saved locally. Database IDs, connections, access permissions, and contents keep their existing settings.");
                if !self.details.is_null(){ui.separator();show_details(ui,&self.details);}
            });
        });
        self.open=open;
        if let Some(target)=self.target.clone() {
            if save {self.send(target,"save",json!({"name":self.name,"description":self.description,"notes":self.notes}),true,true,ctx);}
            else if refresh {self.send(target,"load",Value::Null,true,!self.loaded,ctx);}
        }
        if self.pending.is_some(){ctx.request_repaint_after(Duration::from_millis(50));}
    }
}

fn show_details(ui:&mut egui::Ui,details:&Value) {
    let bytes=|key:&str|crate::format_bytes(details[key].as_i64().unwrap_or(0));
    ui.strong(format!("Database size: {}",bytes("database_bytes")));
    ui.label(format!("Table data: {} · Indexes: {}",bytes("table_data_bytes"),bytes("index_bytes")));
    ui.small("Database size includes stored previews and indexes. Downloaded packs and original image files are stored separately.");
    for (label,key) in [("Database","database"),("Type","kind"),("Host","host"),("Port","port"),("User","username"),("PostgreSQL","server_version"),("Encoding","encoding"),("Catalog ID","catalog_id")] {
        if let Some(value)=details[key].as_str(){ui.label(format!("{label}: {value}"));}
    }
    if let Some(readonly)=details["readonly"].as_bool(){ui.label(if readonly{"Catalog access: read-only"}else{"Catalog access: writable"});}
    if let Some(roots)=details["roots"].as_array(){ui.collapsing("Image folders",|ui|{for root in roots {if let Some(root)=root.as_str(){ui.label(root);}}});}
    if let Some(tables)=details["tables"].as_array(){ui.collapsing(format!("Tables ({})",tables.len()),|ui|{
        for table in tables {
            ui.label(format!("{} · {} · ~{} rows",table["name"].as_str().unwrap_or("?"),crate::format_bytes(table["total_bytes"].as_i64().unwrap_or(0)),table["estimated_rows"]));
        }
        ui.small("Row estimates follow PostgreSQL statistics; they may lag recent changes.");
    });}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_metadata_reply_cannot_replace_a_saved_name_or_another_form() {
        let target=Target{kind:"assets".into(),id:None,dsn:"private".into(),name:"Before".into()};
        let mut editor=Editor::new();editor.pending=Some(2);
        for (serial,name) in [(2,"After"),(1,"Before")] {
            editor.tx.send(Reply{serial,target:target.clone(),edit:true,reset_fields:true,result:Ok(json!({"metadata":{"name":name}}))}).unwrap();
        }
        let changes=editor.poll();assert_eq!(changes.len(),1);assert_eq!(editor.name,"After");assert_eq!(changes[0].1["name"],"After");
    }
    #[test]
    fn metadata_and_storage_details_render_in_both_themes() {
        for dark in [true,false] {for width in [650.,1250.] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut editor=Editor::new();editor.open=true;editor.loaded=true;editor.name="Library".into();
            editor.details=json!({"database_bytes":123456789,"table_data_bytes":100000000,"index_bytes":10000000,"database":"assets","host":"localhost","tables":[]});
            for _ in 0..2 {
                let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,850.))),..Default::default()},|_|editor.ui(&ctx));
                assert!(!output.shapes.is_empty());
            }
        }}
    }
}
