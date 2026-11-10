use eframe::egui;
use serde_json::Value;
use crossbeam_channel::{Receiver, Sender};
use std::{collections::{HashMap, HashSet}, process::Command,sync::Arc};

pub struct Panel {
    tx: Sender<(String, Result<Value, String>)>, rx: Receiver<(String, Result<Value, String>)>,
    rows: Vec<Value>, detail: Option<Arc<Value>>, kind: String, dismissed: bool,
    name: String, add_ids: String, merge_id: String, selected: HashSet<i64>,
    offset: usize, more: bool, busy: bool, detecting: bool, status: String,
    job_id: Option<String>, pub changed: bool,
    dsn:Option<String>,
    readonly:bool, pending:usize, load_failed:bool, last_poll:std::time::Instant,
    #[cfg(test)] test_dsn: Option<String>,
}

impl Default for Panel {
    fn default() -> Self {
        let (tx,rx)=crossbeam_channel::unbounded();
        Self { tx,rx,rows:vec![],detail:None,kind:"event".into(),dismissed:false,name:String::new(),
            add_ids:String::new(),merge_id:String::new(),selected:HashSet::new(),offset:0,more:false,
            busy:false,detecting:false,status:String::new(),job_id:None,changed:false,dsn:std::env::var("IMAGE_CLI_DSN").ok(), readonly:false,pending:0,load_failed:false,last_poll:std::time::Instant::now(),
            #[cfg(test)] test_dsn:None }
    }
}

fn cli(args: Vec<String>,_progress:Sender<(String,Result<Value,String>)>,_ctx:egui::Context,dsn:Option<String>) -> Result<Value,String> {
    let fallback=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.venv/Scripts/python.exe");
    let python=std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|if fallback.is_file(){fallback.to_string_lossy().into()}else{"python".into()});
    let mut command=Command::new(python);command.args(["-m","image_atlas.cli","--json"]);
    let timeout=if args.iter().any(|a|a=="detect"){3600}else{60};command.args(args);
    if let Some(dsn)=dsn {command.env("IMAGE_CLI_DSN",dsn);}
    if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
    let result=crate::bridge::output(&mut command,std::time::Duration::from_secs(timeout),32*1024*1024)?;
    if !result.status.success()&&result.status.code()!=Some(130){return Err(String::from_utf8_lossy(&result.stderr).trim().into());}
    serde_json::from_slice(&result.stdout).map_err(|e|format!("Could not read collection response: {e}"))
}

fn text<'a>(row: &'a Value,key: &str) -> &'a str {row[key].as_str().unwrap_or("")}

impl Panel {
    pub fn with_dsn(dsn:String,readonly:bool)->Self {Self{dsn:Some(dsn),readonly,..Self::default()}}
    #[cfg(debug_assertions)]
    pub fn ready(&self)->bool{!self.busy}
    fn run(&mut self, tag: &str, args: Vec<String>, ctx: &egui::Context) {
        self.pending+=1;self.busy=true;
        let tx=self.tx.clone(); let tag=tag.to_owned(); let ctx=ctx.clone();
        #[cfg(not(test))] let dsn=self.dsn.clone();
        #[cfg(test)] let dsn=self.test_dsn.clone().or_else(||self.dsn.clone());
        std::thread::spawn(move || {let result=cli(args,tx.clone(),ctx.clone(),dsn);let _=tx.send((tag,result));ctx.request_repaint();});
    }
    pub fn refresh(&mut self,ctx: &egui::Context) {
        self.offset=0;self.detail=None;self.selected.clear();self.rows.clear();self.more=false;self.load_failed=false;self.status.clear();
        self.load(false,ctx);
    }
    fn load(&mut self,append:bool,ctx:&egui::Context) {
        let mut args=vec!["collections".into(),"list".into(),"--kind".into(),self.kind.clone(),"--offset".into(),self.offset.to_string()];
        if self.dismissed {args.push("--include-dismissed".into());}
        self.run(if append {"append"} else {"list"},args,ctx);
    }
    fn show(&mut self,id:i64,ctx:&egui::Context) {self.run("detail",vec!["collections".into(),"show".into(),id.to_string(),"--limit".into(),"100".into()],ctx);}
    fn more_members(&mut self,id:i64,offset:usize,ctx:&egui::Context){self.run("members",vec!["collections".into(),"show".into(),id.to_string(),"--limit".into(),"100".into(),"--offset".into(),offset.to_string()],ctx);}
    fn review(&mut self,id:i64,action:&str,ctx:&egui::Context) {
        let mut args=vec!["collections".into(),"review".into(),id.to_string(),action.into()];
        if matches!(action,"rename"|"split") {args.extend(["--name".into(),if action=="split"{format!("{} · split",self.name)}else{self.name.clone()}]);}
        if action=="merge" {args.extend(["--other-id".into(),self.merge_id.clone()]);}
        if action=="add" {
            for id in self.add_ids.split(',').map(str::trim).filter(|s|!s.is_empty()) {args.extend(["--image-id".into(),id.into()]);}
        } else if matches!(action,"remove"|"split") {
            for id in &self.selected {args.extend(["--image-id".into(),id.to_string()]);}
        }
        self.run("review",args,ctx);
    }
    fn receive(&mut self,ctx:&egui::Context) {
        while let Ok((tag,result))=self.rx.try_recv() {
            if tag=="progress" {
                if let Ok(value)=result {let message=text(&value,"text");self.status=message.into();if let Some(id)=message.strip_prefix("Collection job ID: "){self.job_id=Some(id.into());}}
                continue;
            }
            self.pending=self.pending.saturating_sub(1);self.busy=self.pending>0||self.detecting;
            match result {
                Err(error)=>{self.status=error;if tag=="list"||tag=="append"{self.load_failed=true;}if tag=="detect" {self.detecting=false;self.job_id=None;}self.busy=self.pending>0||self.detecting;}
                Ok(value)=>match tag.as_str() {
                    "list"|"append"=>{let rows=value.as_array().cloned().unwrap_or_default();self.more=rows.len()==100;if tag=="list" {self.rows.clear();}self.offset+=rows.len();self.rows.extend(rows);},
                    "members"=>{if let Some(current)=&self.detail {if current["id"]==value["id"]{
                        let mut current=(**current).clone();let offset=value["image_offset"].as_u64().unwrap_or(0) as usize;
                        if let (Some(existing),Some(next))=(current["images"].as_array_mut(),value["images"].as_array()){if existing.len()==offset{existing.extend(next.iter().cloned());}}
                        current["image_more"]=value["image_more"].clone();self.detail=Some(Arc::new(current));
                    }}},
                    "detail"|"review"=>{self.name=text(&value,"name").into();self.kind=text(&value,"kind").into();self.detail=Some(Arc::new(value));self.selected.clear();if tag=="review" {self.changed=true;}},
                    "detect"=>{self.detecting=false;self.job_id=None;self.status=if value["cancelled"].as_bool()==Some(true) {"Detection cancelled; previous collections preserved".into()} else {format!("Detection finished\n{}",crate::bridge::readable(&value))};let completion=self.status.clone();self.changed=true;self.refresh(ctx);self.status=completion;},
                    "jobs"=>{self.job_id=value.as_array().and_then(|a|a.iter().find(|v|text(v,"source").starts_with("collections:"))).map(|v|text(v,"id").into());},
                    "lookup"=>{self.status="Name looked up · © OpenStreetMap contributors".into();self.changed=true;if let Some(id)=self.detail.as_ref().and_then(|v|v["id"].as_i64()) {self.show(id,ctx);}},
                    "cancel"=>self.status="Cancellation requested; waiting for rollback".into(),
                    _=>{}
                }
            }
        }
    }
    pub fn ui(&mut self,ui:&mut egui::Ui,textures:&HashMap<i64,egui::TextureHandle>,requests:&mut Vec<i64>,image_selection:&HashSet<i64>) -> Option<i64> {
        let ctx=ui.ctx().clone();self.receive(&ctx);
        if self.detecting && self.last_poll.elapsed()>std::time::Duration::from_secs(1)&&self.pending<2 {self.last_poll=std::time::Instant::now();self.run("jobs",vec!["jobs".into()],&ctx);}
        let mut browse=None;
        ui.heading("Events, trips, and places").on_hover_text("Automatically suggested collections. Names and manual edits survive redetection.");
        ui.horizontal_wrapped(|ui| {
            for (kind,label) in [("event","Events"),("trip","Trips"),("place","Places")] {
                if ui.add_enabled(!self.busy,egui::Button::new(label).selected(self.kind==kind)).on_hover_text(format!("Browse saved {label}")).clicked() {self.kind=kind.into();self.refresh(&ctx);}
            }
            if ui.add_enabled(!self.busy,egui::Checkbox::new(&mut self.dismissed,"Show dismissed")).on_hover_text("Restore suppressed collections").changed(){self.refresh(&ctx);}
            if ui.add_enabled(!self.busy&&!self.detecting&&!self.readonly,egui::Button::new("Detect collections")).on_hover_text("Analyze indexed metadata locally; no network or original-file access").clicked() {
                self.detecting=true;self.job_id=None;self.last_poll=std::time::Instant::now();self.status="Detecting locally…".into();self.run("detect",vec!["collections".into(),"detect".into()],&ctx);
            }
            if self.detecting {
                ui.spinner();
                if let Some(id)=self.job_id.clone() {if ui.button("Cancel detection").on_hover_text("Roll back this run; previous collections stay available").clicked(){self.run("cancel",vec!["cancel".into(),id],&ctx);}}
            }
        });
        egui::ScrollArea::vertical().id_salt("collection-status").max_height(90.0).show(ui,|ui|{ui.label(&self.status).on_hover_text("Background detection and review status");});
        if self.load_failed {if ui.button("Retry loading collections").on_hover_text("Retry the failed request; this does not redetect or change collections").clicked(){self.refresh(&ctx);}}
        if let Some(c)=self.detail.clone() {
            let id=c["id"].as_i64().unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.busy,egui::Button::new("Back")).on_hover_text("Return to collection list").clicked(){self.refresh(&ctx);}
                ui.text_edit_singleline(&mut self.name).on_hover_text("Editable name; keeps this collection ID");
                if ui.add_enabled(!self.busy&&!self.readonly&&!self.name.trim().is_empty(),egui::Button::new("Save name")).on_hover_text("Keep this name on future detection runs").clicked(){self.review(id,"rename",&ctx);}
                if ui.add_enabled(!self.busy,egui::Button::new("Browse images")).on_hover_text("Filter the main image view; sorting and paging remain available").clicked(){browse=Some(id);}
                for (action,label) in [("confirm","Confirm"),(if text(&c,"status")=="dismissed" {"restore"} else {"dismiss"},if text(&c,"status")=="dismissed" {"Restore"} else {"Dismiss"})] {
                    if ui.add_enabled(!self.busy&&!self.readonly,egui::Button::new(label)).on_hover_text("Save review decision across redetection").clicked(){self.review(id,action,&ctx);}
                }
            });
            ui.collapsing("Why these photos?",|ui| {ui.label(crate::bridge::readable(&c["evidence"])).on_hover_text("Detection evidence; confidence is a heuristic, not a probability");ui.label(format!("Confidence {} · {} → {}",c["confidence"],text(&c,"start_at"),text(&c,"end_at")));});
            ui.horizontal_wrapped(|ui|{
                if ui.add_enabled(!self.busy&&!self.readonly&&!image_selection.is_empty(),egui::Button::new(format!("Add {} selected images",image_selection.len()))).on_hover_text("Select images in the Images tab, then add that selection to this collection").clicked(){self.add_ids=image_selection.iter().map(i64::to_string).collect::<Vec<_>>().join(",");self.review(id,"add",&ctx);}
                egui::ComboBox::from_id_salt("merge-collection").selected_text(self.rows.iter().find(|r|r["id"].to_string()==self.merge_id).map(|r|text(r,"name")).unwrap_or("Choose merge target")).show_ui(ui,|ui|{
                    for row in &self.rows {if row["id"].as_i64()!=Some(id)&&text(row,"kind")==text(&c,"kind") {ui.selectable_value(&mut self.merge_id,row["id"].to_string(),text(row,"name")).on_hover_text("Collection of the same kind; this collection ID is retained");}}
                }).response.on_hover_text("Choose another loaded collection by name instead of typing its ID");
            });
            ui.horizontal_wrapped(|ui| {
                for (action,label) in [("remove","Remove selected"),("split","Split selected")] {if ui.add_enabled(!self.busy&&!self.readonly&&!self.selected.is_empty(),egui::Button::new(label)).on_hover_text("Preserve manual membership boundaries on redetection").clicked(){self.review(id,action,&ctx);}}
                ui.add(egui::TextEdit::singleline(&mut self.add_ids).hint_text("Image IDs: 12, 34").desired_width(140.0)).on_hover_text("Assign existing catalog images to this collection");
                if ui.add_enabled(!self.busy&&!self.readonly&&self.add_ids.split(',').all(|s|s.trim().parse::<i64>().is_ok()),egui::Button::new("Add images")).on_hover_text("Explicit additions survive detection").clicked(){self.review(id,"add",&ctx);}
                ui.add(egui::TextEdit::singleline(&mut self.merge_id).hint_text("Collection ID").desired_width(100.0)).on_hover_text("Existing collection of the same kind");
                if ui.add_enabled(!self.busy&&!self.readonly&&self.merge_id.parse::<i64>().is_ok_and(|other|other!=id),egui::Button::new("Merge")).on_hover_text("Keep this collection ID and combine contents").clicked(){self.review(id,"merge",&ctx);}
            });
            if text(&c,"kind")=="place" {ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.busy&&!self.readonly,egui::Button::new("Look up this place")).on_hover_text("Send this place coordinate to OpenStreetMap Nominatim; images stay local").clicked(){self.run("lookup",vec!["collections".into(),"lookup".into(),id.to_string()],&ctx);}
                ui.hyperlink_to("Lookup policy","https://operations.osmfoundation.org/policies/nominatim/");ui.small("© OpenStreetMap contributors");
            });}
            let children=c["children"].as_array().cloned().unwrap_or_default();
            ui.horizontal_wrapped(|ui| {for child in &children {if ui.add_enabled(!self.busy,egui::Button::new(text(child,"name"))).on_hover_text(format!("Open linked {} #{}",text(child,"kind"),child["id"])).clicked(){if let Some(id)=child["id"].as_i64(){self.show(id,&ctx);}}}});
            let images=c["images"].as_array().map(|items|items.iter().filter(|r|text(r,"source")!="excluded").collect::<Vec<_>>()).unwrap_or_default();
            let route=c["route"].as_array().filter(|a|!a.is_empty()).unwrap_or(&children);
            let points:Vec<_>=if text(&c,"kind")=="trip" {route.iter().filter_map(|r|Some((r["longitude"].as_f64()?,r["latitude"].as_f64()?,text(r,"name").to_string()))).collect()}
                else {images.iter().filter_map(|r|Some((r["gps_lon"].as_f64()?,r["gps_lat"].as_f64()?,format!("Image #{}",r["image_id"])))).collect()};
            ui.collapsing("Locations",|ui|plot(ui,&points));
            egui::ScrollArea::vertical().id_salt("collection-members").max_height((ui.available_height()-44.0).max(60.0)).show_rows(ui,94.0,images.len(),|ui,range| {for index in range {let r=&images[index];let iid=r["image_id"].as_i64().unwrap_or(0);ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),94.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_height(94.0);ui.horizontal(|ui| {
                let mut selected=self.selected.contains(&iid);if ui.checkbox(&mut selected,format!("#{iid}")).on_hover_text("Select for removal or split").changed(){if selected {self.selected.insert(iid);}else{self.selected.remove(&iid);}}
                thumbnail(ui,iid,textures,requests);
                ui.vertical(|ui|{
                    ui.add(egui::Label::new(std::path::Path::new(text(r,"path")).file_name().unwrap_or_default().to_string_lossy()).truncate()).on_hover_text(text(r,"path"));
                    ui.small(format!("{} · confidence {:.2}",text(r,"source"),r["confidence"].as_f64().unwrap_or(0.0))).on_hover_text(crate::bridge::readable(&r["evidence"]));
                });
            });});}});
            if c["image_more"].as_bool()==Some(true)&&ui.add_enabled(!self.busy,egui::Button::new("Load next 100 images")).on_hover_text("Fetch the next member page from the database").clicked(){self.more_members(id,images.len(),&ctx);}
            ui.small(format!("{} of {} images loaded",images.len(),c["image_count"]));
        } else {
            if let Some(id)=timeline(ui,&self.rows) {self.show(id,&ctx);}
            egui::ScrollArea::vertical().id_salt("collection-list").max_height((ui.available_height()-44.0).max(40.0)).show_rows(ui,96.0,self.rows.len(),|ui,range| {for index in range {let c=self.rows[index].clone();ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),96.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_height(96.0);ui.horizontal(|ui| {
                if let Some(id)=c["cover_image_id"].as_i64(){thumbnail(ui,id,textures,requests);}
                ui.vertical(|ui| {if ui.add_enabled(!self.busy,egui::Button::new(text(&c,"name")).truncate()).on_hover_text(format!("{} · {} · confidence {} (heuristic)",c["id"],text(&c,"status"),c["confidence"])).clicked(){self.show(c["id"].as_i64().unwrap_or(0),&ctx);}
                    ui.label(format!("{} images · {} → {}{}",c["image_count"],text(&c,"start_at"),text(&c,"end_at"),if c["stale"].as_bool()==Some(true){" · needs refresh"}else{""})).on_hover_text(crate::bridge::readable(&c["evidence"]));});
            });});}});
            if self.rows.is_empty()&&!self.busy&&!self.load_failed{ui.label("No collections match this kind. Detect collections after mining images.");}
            if self.more&&ui.add_enabled(!self.busy,egui::Button::new("Load more")).on_hover_text("Load the next 100 collections").clicked(){self.load(true,&ctx);}
        }
        browse
    }
}

#[cfg(test)]
#[test] fn collection_paging_footer_is_visible(){
    let mut panel=Panel::default();panel.rows=(1..=100).map(|id|serde_json::json!({"id":id,"name":format!("QA {id}"),"kind":"event","image_count":1,"status":"confirmed"})).collect();panel.more=true;
    let ctx=egui::Context::default();let mut position=None;
    for _ in 0..3 {
        let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(640.0,480.0))),..Default::default()},|ui|{panel.ui(ui,&HashMap::new(),&mut vec![],&HashSet::new());});
        position=output.shapes.iter().find_map(|s|match &s.shape{egui::epaint::Shape::Text(t) if t.galley.job.text=="Load more"=>Some((t.pos.y,t.galley.size().y)),_=>None});
    }
    let(y,h)=position.expect("Collection paging button painted");assert!(y+h<=480.0);
}

fn thumbnail(ui:&mut egui::Ui,id:i64,textures:&HashMap<i64,egui::TextureHandle>,requests:&mut Vec<i64>) {
    if let Some(texture)=textures.get(&id) {ui.image((texture.id(),texture.size_vec2()*(80.0/texture.size_vec2().x.max(texture.size_vec2().y)))).on_hover_text(format!("Stored cover thumbnail · image #{id}"));}
    else {requests.push(id);ui.allocate_space(egui::vec2(80.0,80.0));}
}

fn plot(ui:&mut egui::Ui,points:&[(f64,f64,String)]) {
    if points.is_empty(){return;}
    ui.label("Photographed locations · longitude / latitude").on_hover_text("Local coordinate plot; connecting lines do not represent roads");
    let (rect,response)=ui.allocate_exact_size(egui::vec2(ui.available_width().min(600.0),140.0),egui::Sense::hover());
    ui.painter().rect_filled(rect,4.0,ui.visuals().extreme_bg_color);
    let min_x=points.iter().map(|p|p.0).fold(f64::INFINITY,f64::min);let max_x=points.iter().map(|p|p.0).fold(f64::NEG_INFINITY,f64::max);
    let min_y=points.iter().map(|p|p.1).fold(f64::INFINITY,f64::min);let max_y=points.iter().map(|p|p.1).fold(f64::NEG_INFINITY,f64::max);
    let mut previous=None;
    let step=points.len().div_ceil(512).max(1);
    for (lon,lat,label) in points.iter().step_by(step).chain(points.last().into_iter().filter(|_|(points.len()-1)%step!=0)) {
        let p=egui::pos2(rect.left()+10.0+(if max_x==min_x {0.5}else{(lon-min_x)/(max_x-min_x)}) as f32*(rect.width()-20.0),rect.bottom()-10.0-(if max_y==min_y {0.5}else{(lat-min_y)/(max_y-min_y)}) as f32*(rect.height()-20.0));
        if let Some(last)=previous {ui.painter().line_segment([last,p],egui::Stroke::new(1.0,egui::Color32::LIGHT_BLUE));}previous=Some(p);
        ui.painter().circle_filled(p,4.0,egui::Color32::GOLD);
        if response.hover_pos().is_some_and(|pos|pos.distance(p)<8.0){response.clone().on_hover_text(format!("{label}\n{lat:.5}, {lon:.5}\n{} images/visits at this coordinate",points.iter().filter(|v|v.0==*lon&&v.1==*lat).count()));}
    }
}

fn timeline(ui:&mut egui::Ui,rows:&[Value])->Option<i64> {
    let ranges:Vec<_>=rows.iter().take(20).filter_map(|c| {
        let parse=|key|chrono::NaiveDateTime::parse_from_str(&text(c,key).replace(' ',"T"),"%Y-%m-%dT%H:%M:%S%.f").ok().map(|d|d.and_utc().timestamp() as f64);
        Some((c,parse("start_at")?,parse("end_at")?))
    }).collect();
    if ranges.is_empty(){return None;}
    ui.small("Collection timeline").on_hover_text("Date ranges for the first 20 loaded collections; click a bar to review");
    let min=ranges.iter().map(|r|r.1).fold(f64::INFINITY,f64::min);
    let max=ranges.iter().map(|r|r.2).fold(f64::NEG_INFINITY,f64::max);
    let (rect,_)=ui.allocate_exact_size(egui::vec2(ui.available_width().min(800.0),ranges.len() as f32*18.0+10.0),egui::Sense::hover());
    ui.painter().rect_filled(rect,4.0,ui.visuals().extreme_bg_color);
    let mut clicked=None;
    for (n,(c,start,end)) in ranges.iter().enumerate() {
        let x=rect.left()+5.0+((*start-min)/(max-min).max(1.0)) as f32*(rect.width()-10.0);
        let width=((end-start)/(max-min).max(1.0)) as f32*(rect.width()-10.0);
        let bar=egui::Rect::from_min_size(egui::pos2(x,rect.top()+5.0+n as f32*18.0),egui::vec2(width.max(8.0),14.0));
        ui.painter().rect_filled(bar,2.0,egui::Color32::LIGHT_BLUE);
        if ui.interact(bar,ui.id().with(("collection-time",n)),egui::Sense::click()).on_hover_text(format!("{} · {} images\n{} → {}",text(c,"name"),c["image_count"],text(c,"start_at"),text(c,"end_at"))).clicked(){clicked=c["id"].as_i64();}
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_click_renames_through_shared_python_service() -> anyhow::Result<()> {
        let Ok(dsn)=std::env::var("IMAGE_CLI_TEST_DSN") else {return Ok(());};
        anyhow::ensure!(dsn.ends_with("/imagecli_test"),"Use the isolated test database");
        let mut client=postgres::Client::connect(&dsn,postgres::NoTls)?;
        let root=format!("native-collection-click-{}",std::process::id());
        let cid:i64=client.query_one("INSERT INTO collections(kind,suggested_name,scope_key) VALUES ('event','Before',$1) RETURNING id",&[&root])?.get(0);
        let outcome=(||->anyhow::Result<()> {
            let ctx=egui::Context::default();
            let mut panel=Panel::default();panel.test_dsn=Some(dsn.clone());
            let value=cli(vec!["collections".into(),"show".into(),cid.to_string()],panel.tx.clone(),ctx.clone(),Some(dsn.clone())).map_err(anyhow::Error::msg)?;
            panel.detail=Some(Arc::new(value));panel.name="After native click".into();
            let frame=|panel:&mut Panel,events:Vec<egui::Event>|ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(640.0,800.0))),events,..Default::default()},|ui|{panel.ui(ui,&HashMap::new(),&mut Vec::new(),&HashSet::new());});
            let output=frame(&mut panel,Vec::new());
            let pos=output.shapes.iter().find_map(|s|match &s.shape {egui::epaint::Shape::Text(t) if t.galley.job.text=="Save name"=>Some(t.pos+t.galley.size()/2.0),_=>None}).ok_or_else(||anyhow::anyhow!("Save name button missing"))?;
            frame(&mut panel,vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
            frame(&mut panel,vec![egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);
            while panel.busy&&std::time::Instant::now()<deadline {panel.receive(&ctx);std::thread::sleep(std::time::Duration::from_millis(10));}
            anyhow::ensure!(!panel.busy,"Native review timed out");
            let name:Option<String>=client.query_one("SELECT user_name FROM collections WHERE id=$1",&[&cid])?.get(0);
            anyhow::ensure!(name.as_deref()==Some("After native click"),"Native click did not persist: {}",panel.status);
            Ok(())
        })();
        client.execute("DELETE FROM collections WHERE id=$1",&[&cid])?;
        outcome
    }
    #[test]
    fn collections_render_at_supported_widths() {
        for width in [640.0,1200.0] {
            let mut panel=Panel::default();
            panel.detail=Some(Arc::new(serde_json::json!({"id":1,"kind":"trip","name":"Our outing","status":"suggested","confidence":0.7,
                "start_at":"2026-06-01T10:00:00","end_at":"2026-06-01T14:00:00","evidence":{"reason":"Photographed journey"},
                "children":[],"images":[{"image_id":1,"path":"photo.jpg","source":"automatic","confidence":0.7,"gps_lat":30,"gps_lon":-81,"evidence":{"reason":"GPS"}}]})));
            panel.name="Our outing".into();
            if let Ok(path)=std::env::var("IMAGE_CLI_COLLECTION_RENDER_DATA") {
                panel.detail=Some(Arc::new(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()));
                panel.kind=text(panel.detail.as_ref().unwrap(),"kind").into();
                panel.name=text(panel.detail.as_ref().unwrap(),"name").into();
            }
            let ctx=egui::Context::default();
            ctx.style_mut_of(egui::Theme::Dark,|style|style.visuals=egui::Visuals::dark());
            let mut textures=HashMap::new();
            let thumbnail=egui::ColorImage::filled([40,40],egui::Color32::from_rgb(60,100,150));
            if let Some(images)=panel.detail.as_ref().and_then(|c|c["images"].as_array()) {
                for row in images {if let Some(id)=row["image_id"].as_i64() {textures.insert(id,ctx.load_texture(format!("image-{id}"),thumbnail.clone(),Default::default()));}}
            }
            let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.0))),..Default::default()},|ui|{
                assert!(panel.ui(ui,&textures,&mut Vec::new(),&HashSet::new()).is_none());
            });
            assert!(!output.shapes.is_empty());
            if let Ok(folder)=std::env::var("IMAGE_CLI_COLLECTION_RENDER_DIR") {
                let id=|t:egui::TextureId|match t {egui::TextureId::Managed(n)|egui::TextureId::User(n)=>n};
                let textures:Vec<_>=output.textures_delta.set.iter().map(|(tid,delta)| {
                    let egui::ImageData::Color(image)=&delta.image;
                    serde_json::json!({"id":id(*tid),"size":image.size,"pixels":image.pixels.iter().map(|p|p.to_array()).collect::<Vec<_>>()})
                }).collect();
                let meshes:Vec<_>=ctx.tessellate(output.shapes,output.pixels_per_point).into_iter().filter_map(|item| {
                    if let egui::epaint::Primitive::Mesh(mesh)=item.primitive {Some(serde_json::json!({"texture":id(mesh.texture_id),"clip":[item.clip_rect.min.x,item.clip_rect.min.y,item.clip_rect.max.x,item.clip_rect.max.y],"indices":mesh.indices,
                        "vertices":mesh.vertices.iter().map(|v|{let c=v.color.to_array();[v.pos.x,v.pos.y,v.uv.x,v.uv.y,c[0] as f32/255.0,c[1] as f32/255.0,c[2] as f32/255.0,c[3] as f32/255.0]}).collect::<Vec<_>>()}))}else{None}
                }).collect();
                std::fs::write(std::path::Path::new(&folder).join(format!("native-collections-{width}.json")),serde_json::to_vec(&serde_json::json!({"width":width,"height":800,"textures":textures,"meshes":meshes})).unwrap()).unwrap();
            }
        }
    }
}
