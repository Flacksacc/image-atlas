//! Remote browsing never opens PostgreSQL or source paths on the client.
use eframe::egui;
use serde_json::{Value,json};
use std::{collections::{HashMap,HashSet,VecDeque},process::Command,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},time::Duration};

pub(crate) const PREVIEW_VERSION:&str="jxl-1024-d2-e7-srgb-imagecodecs2026.10.10-v1";
#[derive(Clone)]
enum Task {Search(Value),Image(i64,bool),Original(i64)}
enum ResultData {Search(Value),Image(i64,bool,egui::ColorImage),Original(String),ImageError(i64,bool,String),Error(String)}
struct Entry {texture:egui::TextureHandle,used:u64}
type Queue=Arc<Mutex<VecDeque<(u64,Task)>>>;

fn execute(task:Task)->Result<ResultData,String>{
    let mut command=Command::new(std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into()));
    crate::actions::scrub_environment(&mut command);
    command.args(["-m","image_atlas.remote_bridge"]);
    if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
    let input=match &task {
        Task::Search(args)=>{command.arg("search");Some(serde_json::to_vec(args).map_err(|e|e.to_string())?)},
        Task::Image(id,preview)=>{command.arg(if *preview{"preview"}else{"thumb"}).arg(id.to_string());None},
        Task::Original(id)=>{command.arg("original").arg(id.to_string());None}
    };
    let out=if let Some(mut input)=input{input.push(b'\n');crate::bridge::output_with_input(&mut command,Duration::from_secs(120),8*1024*1024,input)?}
        else if matches!(&task,Task::Original(_)){crate::bridge::output_detached(&mut command,Duration::from_secs(120),8*1024*1024)?}
        else{crate::bridge::output(&mut command,Duration::from_secs(120),8*1024*1024)?};
    if !out.status.success(){return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());}
    match task {
        Task::Search(_)=>Ok(ResultData::Search(serde_json::from_slice(&out.stdout).map_err(|e|e.to_string())?)),
        Task::Image(id,preview)=>Ok(ResultData::Image(id,preview,crate::decode::parse_pixels(&out.stdout)?)),
        Task::Original(_)=>{let value:Value=serde_json::from_slice(&out.stdout).map_err(|e|e.to_string())?;Ok(ResultData::Original(value["path"].as_str().unwrap_or("").to_owned()))}
    }
}

struct RemoteViewer {
    query:String, mode:String, offset:usize, total:usize, items:Vec<Value>, selected:Option<i64>,
    queue:Queue,active:Arc<AtomicU64>,generation:u64,
    rx:crossbeam_channel::Receiver<(u64,ResultData)>,
    cache:HashMap<(i64,bool),Entry>,pending:HashSet<(i64,bool)>,failed:HashSet<(i64,bool)>,
    frame:u64,message:String,loading:bool,last_image_click:Option<(i64,egui::Pos2,f64)>,
}

impl RemoteViewer {
    fn new(ctx:egui::Context)->Self{
        crate::install_fonts(&ctx);
        let queue:Queue=Arc::new(Mutex::new(VecDeque::new()));
        let active=Arc::new(AtomicU64::new(0));
        let(tx,rx)=crossbeam_channel::bounded(32);
        for _ in 0..2 {
            let queue=queue.clone();let active=active.clone();let tx=tx.clone();let ctx=ctx.clone();
            std::thread::spawn(move || loop {
                if Arc::strong_count(&queue)==1{break;}
                let next=queue.lock().unwrap().pop_front();
                if let Some((generation,task))=next {
                    if active.load(Ordering::Acquire)!=generation{continue;}
                    let image_key=match &task{Task::Image(id,p)=>Some((*id,*p)),_=>None};
                    let result=execute(task).unwrap_or_else(|error|if let Some((id,p))=image_key{ResultData::ImageError(id,p,error)}else{ResultData::Error(error)});
                    if active.load(Ordering::Acquire)==generation {
                        if tx.send((generation,result)).is_err(){break;}ctx.request_repaint();
                    }
                }else{std::thread::sleep(Duration::from_millis(20));}
            });
        }
        let mut app=Self{query:String::new(),mode:"combined".into(),offset:0,total:0,items:vec![],selected:None,
            queue,active,generation:0,rx,cache:HashMap::new(),pending:HashSet::new(),failed:HashSet::new(),
            frame:0,message:String::new(),loading:false,last_image_click:None};
        #[cfg(debug_assertions)] if let Ok(query)=std::env::var("IMAGE_CLI_QA_QUERY"){app.query=query;app.mode="text".into();}
        app.search();app
    }
    fn search(&mut self){
        self.generation+=1;self.active.store(self.generation,Ordering::Release);
        self.queue.lock().unwrap().clear();self.pending.clear();self.failed.clear();self.cache.clear();self.items.clear();self.selected=None;self.last_image_click=None;self.loading=true;
        self.queue.lock().unwrap().push_front((self.generation,Task::Search(json!({"q":self.query,"mode":self.mode,"limit":100,"offset":self.offset}))));
    }
    fn want(&mut self,id:i64,preview:bool,priority:bool){
        let key=(id,preview);
        if self.cache.contains_key(&key)||self.failed.contains(&key){return;}
        if self.pending.insert(key){
            let mut queue=self.queue.lock().unwrap();
            if queue.len()<200 {if priority{queue.push_front((self.generation,Task::Image(id,preview)));}else{queue.push_back((self.generation,Task::Image(id,preview)));}}
            else{self.pending.remove(&key);}
        }else if priority {
            let mut queue=self.queue.lock().unwrap();
            if let Some(pos)=queue.iter().position(|(_,task)|matches!(task,Task::Image(i,p) if *i==id&&*p==preview)){
                if let Some(task)=queue.remove(pos){queue.push_front(task);}
            }
        }
    }
    fn select_image(&mut self, id:i64, double_clicked:bool,ctx:&egui::Context) {
        self.selected=Some(id);
        self.last_image_click=ctx.input(|input|input.pointer.interact_pos().map(|position|(id,position,input.time)));
        self.want(id,false,true);
        if self.items.iter().any(|item|item["id"].as_i64()==Some(id)&&item["has_preview"]==true) {
            self.want(id,true,true);
        }
        if double_clicked {self.open_original(id);}
    }
    fn open_original(&mut self,id:i64) {
        let mut queue=self.queue.lock().unwrap();
        if !queue.iter().any(|(_,task)|matches!(task,Task::Original(queued) if *queued==id)) {
            queue.push_front((self.generation,Task::Original(id)));
        }
    }
    fn receive(&mut self,ctx:&egui::Context){
        for(generation,result)in self.rx.try_iter().take(12){
            if generation!=self.generation{continue;}
            match result {
                ResultData::Search(value)=>{self.total=value["total"].as_u64().unwrap_or(0)as usize;self.items=value["items"].as_array().cloned().unwrap_or_default();self.loading=false;self.message=format!("{} · {} results",value["catalog_name"].as_str().unwrap_or("Remote catalog"),self.total);},
                ResultData::Image(id,preview,pixels)=>{self.pending.remove(&(id,preview));let texture=ctx.load_texture(format!("remote-{id}-{preview}"),pixels,egui::TextureOptions::LINEAR);self.cache.insert((id,preview),Entry{texture,used:self.frame});},
                ResultData::Original(path)=>self.message=format!("Downloaded and opened {path}"),
                ResultData::ImageError(id,preview,error)=>{self.pending.remove(&(id,preview));self.failed.insert((id,preview));self.message=error;},
                ResultData::Error(error)=>{self.message=error;self.loading=false;}
            }
        }
        while self.cache.values().map(|e|e.texture.size()[0]*e.texture.size()[1]*4).sum::<usize>()>256*1024*1024 {
            if let Some(key)=self.cache.iter().filter(|((id,_),_)|Some(*id)!=self.selected).min_by_key(|(_,entry)|entry.used).map(|(key,_)|*key){self.cache.remove(&key);}else{break;}
        }
    }
}

impl RemoteViewer {
    fn content(&mut self,ui:&mut egui::Ui){
        ui.painter().rect_filled(ui.max_rect(),0.,ui.visuals().panel_fill);
        let ctx=ui.ctx().clone();self.frame+=1;self.receive(&ctx);
        if let Some(id)=crate::double_clicked_image(&ctx,&mut self.last_image_click){
            if self.items.iter().any(|item|item["id"].as_i64()==Some(id)){
                self.open_original(id);ui.disable();
            }
        }
        #[cfg(debug_assertions)]
        if let Ok(path)=std::env::var("IMAGE_CLI_VIEWER_SCREENSHOT"){
            if self.frame==1{ctx.set_theme(if std::env::var("IMAGE_CLI_QA_THEME").as_deref()==Ok("Light"){egui::Theme::Light}else{egui::Theme::Dark});}
            for event in ctx.input(|i|i.events.clone()){
                if let egui::Event::Screenshot{image,..}=event{
                    let pixels:Vec<u8>=image.pixels.iter().flat_map(|p|p.to_array()).collect();
                    let _=image::save_buffer(&path,&pixels,image.size[0]as u32,image.size[1]as u32,image::ColorType::Rgba8);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            if let Some(id)=self.items.first().and_then(|i|i["id"].as_i64()){
                if self.cache.contains_key(&(id,false))&&self.selected.is_none(){self.selected=Some(id);self.want(id,true,true);}
                if self.cache.contains_key(&(id,true)){ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));}
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        ui.heading("Remote images");
        let mut submit=false;
        ui.horizontal_wrapped(|ui|{
            let edit=ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search images").desired_width(360.));
            submit|=edit.lost_focus()&&ui.input(|i|i.key_pressed(egui::Key::Enter));
            egui::ComboBox::from_id_salt("remote-mode").selected_text(&self.mode).show_ui(ui,|ui|{for mode in ["combined","text","visual"]{ui.selectable_value(&mut self.mode,mode.into(),mode);}});
            submit|=ui.button("Search").clicked();
            if ui.add_enabled(self.offset>0&&!self.loading,egui::Button::new("Previous")).clicked(){self.offset=self.offset.saturating_sub(100);self.search();}
            if ui.add_enabled(self.offset+100<self.total&&!self.loading,egui::Button::new("Next")).clicked(){self.offset+=100;self.search();}
            if ui.button("Dark").clicked(){ctx.set_theme(egui::Theme::Dark);}
            if ui.button("Light").clicked(){ctx.set_theme(egui::Theme::Light);}
        });
        if submit{self.offset=0;self.search();}
        ui.label(&self.message);if self.loading{ui.spinner();}
        if self.selected.is_some() {
            let maximum=(ui.available_width()*0.5).max(180.);
            egui::Panel::right("remote-image-preview").default_size(420.).min_size(180.).max_size(maximum)
                .show(ui,|ui|self.preview_panel_ui(ui));
        }
        let columns=((ui.available_width()/180.).floor()as usize).max(1);
        let rows=self.items.len().div_ceil(columns);
        egui::ScrollArea::vertical().show_rows(ui,210.,rows,|ui,range|{
            let start=range.start*columns;let end=(range.end*columns).min(self.items.len());
            let candidates=self.items[start..(end+24).min(self.items.len())].to_vec();
            let relevant:HashSet<i64>=candidates.iter().filter_map(|i|i["id"].as_i64()).chain(self.selected).collect();
            let mut removed=Vec::new();
            self.queue.lock().unwrap().retain(|(_,task)|{
                if let Task::Image(id,preview)=task {if !relevant.contains(id){removed.push((*id,*preview));return false;}}
                true
            });
            for key in removed{self.pending.remove(&key);}
            // Queue small images first. Large previews follow only after small images arrive.
            for item in &candidates{if let Some(id)=item["id"].as_i64(){self.want(id,false,false);}}
            for item in &candidates{if let Some(id)=item["id"].as_i64(){if item["has_preview"]==true&&self.cache.contains_key(&(id,false)){self.want(id,true,false);}}}
            for row in range{ui.horizontal(|ui|{for col in 0..columns{
                let index=row*columns+col;let Some(item)=self.items.get(index).cloned()else{break;};let Some(id)=item["id"].as_i64()else{continue;};
                ui.allocate_ui(egui::vec2(170.,200.),|ui|{
                    if let Some(entry)=self.cache.get_mut(&(id,false)){
                        entry.used=self.frame;
                        let response=ui.add(egui::Image::new((entry.texture.id(),entry.texture.size_vec2())).max_size(egui::vec2(160.,120.)).sense(egui::Sense::click()));
                        if response.clicked()||response.double_clicked(){self.select_image(id,response.double_clicked(),ui.ctx());}
                    }else{let response=ui.add_sized([160.,120.],egui::Button::new("Loading thumbnail…"));if response.clicked()||response.double_clicked(){self.select_image(id,response.double_clicked(),ui.ctx());}}
                    let path=item["path"].as_str().unwrap_or("");let name=path.rsplit(['\\','/']).next().unwrap_or(path);
                    let response=ui.button(name).on_hover_text(path);
                    if response.clicked()||response.double_clicked(){self.select_image(id,response.double_clicked(),ui.ctx());}
                    ui.label(format!("{} × {} · {}",item["width"],item["height"],item["format"].as_str().unwrap_or("")));
                });
            }});}
        });
    }

    fn preview_panel_ui(&mut self,ui:&mut egui::Ui){
        let Some(id)=self.selected else{return;};
        let mut close=false;
        ui.horizontal(|ui|{ui.heading("Image preview");close=ui.small_button("Close").clicked();});
        egui::ScrollArea::vertical().id_salt("remote-selected-image").show(ui,|ui|{
            ui.small("Double-click to open the original in your default app.");
            if ui.button("Download original and open").clicked(){self.open_original(id);}
            if let Some(item)=self.items.iter().find(|i|i["id"].as_i64()==Some(id)).cloned(){
                ui.add(egui::Label::new(item["path"].as_str().unwrap_or("")).wrap());
                ui.label(format!("{} × {} · {} · {} stars",item["width"],item["height"],
                    crate::format_bytes(item["size_bytes"].as_i64().unwrap_or(0)),item["rating"]));
                if item["has_preview"]!=true || self.failed.contains(&(id,true)) {
                    ui.small("Database preview unavailable; showing the thumbnail if available.");
                    if self.failed.contains(&(id,true))&&ui.button("Retry preview").clicked(){
                        self.failed.remove(&(id,true));self.want(id,true,true);
                    }
                }else if self.pending.contains(&(id,true)){
                    ui.horizontal(|ui|{ui.spinner();ui.label("Loading database preview…");});
                }
                let tags=item["tags"].as_array().map(|a|a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                let labels=item["labels"].as_array().map(|a|a.iter().filter_map(|v|v["label"].as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                if !tags.is_empty(){ui.label(format!("Tags: {tags}"));}
                if !labels.is_empty(){ui.label(format!("Labels: {labels}"));}
            }
            let key=(id,self.cache.contains_key(&(id,true)));
            if let Some(entry)=self.cache.get_mut(&key){
                entry.used=self.frame;
                let size=entry.texture.size_vec2();
                let scale=(ui.available_width().max(1.)/size.x).min(ui.available_height().max(100.)/size.y).min(1.);
                let response=ui.add(egui::Image::new((entry.texture.id(),size*scale)).sense(egui::Sense::click()));
                if response.double_clicked(){self.open_original(id);}
            }
        });
        if close||ui.input(|i|i.key_pressed(egui::Key::Escape)){self.selected=None;}
    }

}

pub fn run()->eframe::Result{
    #[allow(unused_mut)] let mut size=[1200.,800.];
    #[cfg(debug_assertions)]{if let(Ok(w),Ok(h))=(std::env::var("IMAGE_CLI_QA_WIDTH"),std::env::var("IMAGE_CLI_QA_HEIGHT")){size=[w.parse().unwrap_or(1200.),h.parse().unwrap_or(800.)];}}
    eframe::run_native("Remote ImageAtlas",eframe::NativeOptions{viewport:egui::ViewportBuilder::default().with_inner_size(size),..Default::default()},Box::new(|cc|Ok(Box::new(RemoteViewer::new(cc.egui_ctx.clone())))))
}

impl eframe::App for RemoteViewer {
    fn ui(&mut self,ui:&mut egui::Ui,_frame:&mut eframe::Frame){self.content(ui);}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->RemoteViewer{
        let(_,rx)=crossbeam_channel::unbounded();
        RemoteViewer{query:String::new(),mode:"combined".into(),offset:0,total:1,
            items:vec![json!({"id":1,"path":"G:/Marketing/smores.jpg","width":6000,"height":4000,"format":"JPEG","has_preview":true})],selected:None,
            queue:Arc::new(Mutex::new(VecDeque::new())),active:Arc::new(AtomicU64::new(1)),generation:1,
            rx,cache:HashMap::new(),pending:HashSet::new(),failed:HashSet::new(),frame:0,message:String::new(),loading:false,last_image_click:None}
    }
    #[test]
    fn selection_promotes_preview_and_search_cancels_old_queue(){
        let mut app=fixture();app.want(1,true,false);app.want(2,true,false);app.want(2,true,true);
        assert!(matches!(app.queue.lock().unwrap().front(),Some((1,Task::Image(2,true)))));
        app.search();assert_eq!(app.queue.lock().unwrap().len(),1);assert_eq!(app.active.load(Ordering::Acquire),2);assert!(app.pending.is_empty());
    }
    #[test]
    fn remote_controls_layout_in_both_themes_and_sizes(){
        for theme in [egui::Theme::Dark,egui::Theme::Light]{for width in [640.,1200.]{
            let ctx=egui::Context::default();ctx.set_theme(theme);let mut app=fixture();
            let result=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.))),..Default::default()},|ui|{
                app.content(ui);
            });
            assert!(!result.shapes.is_empty());
        }}
    }
    #[test]
    fn selected_preview_is_docked_and_double_click_queues_one_original(){
        for theme in [egui::Theme::Dark,egui::Theme::Light]{for width in [640.,1200.]{
            let ctx=egui::Context::default();ctx.set_theme(theme);let mut app=fixture();
            let thumbnail=ctx.load_texture("remote-test-thumb",egui::ColorImage::filled([160,100],egui::Color32::WHITE),Default::default());
            app.cache.insert((1,false),Entry{texture:thumbnail,used:0});
            let preview=ctx.load_texture("remote-test-preview",egui::ColorImage::filled([1024,683],egui::Color32::LIGHT_BLUE),Default::default());
            let preview_id=preview.id();app.cache.insert((1,true),Entry{texture:preview,used:0});
            let frame=|app:&mut RemoteViewer,events|ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.))),events,..Default::default()},|ui|app.content(ui));
            let output=frame(&mut app,vec![]);
            let position=output.shapes.iter().find_map(|shape|match &shape.shape{egui::epaint::Shape::Text(text) if text.galley.job.text=="smores.jpg"=>Some(text.pos+text.galley.size()/2.),_=>None}).unwrap();
            let click=|app:&mut RemoteViewer|{
                frame(app,vec![egui::Event::PointerMoved(position),egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
                frame(app,vec![egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
            };
            click(&mut app);assert_eq!(app.selected,Some(1));
            assert!(!app.queue.lock().unwrap().iter().any(|(_,task)|matches!(task,Task::Original(_))));
            let output=frame(&mut app,vec![]);
            let primitives=ctx.tessellate(output.shapes,output.pixels_per_point);
            let bounds=primitives.iter().find_map(|shape|match &shape.primitive{egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id==preview_id=>Some(mesh.calc_bounds()),_=>None}).expect("Stored remote preview rendered");
            assert!(bounds.min.x>width*0.45&&bounds.max.x<=width);
            click(&mut app);
            assert_eq!(app.queue.lock().unwrap().iter().filter(|(_,task)|matches!(task,Task::Original(1))).count(),1);
        }}
    }
    #[test]
    fn filename_click_opens_preview_and_original_button_queues_download(){
        let ctx=egui::Context::default();let mut app=fixture();
        let texture=ctx.load_texture("fixture-thumb",egui::ColorImage::filled([160,107],egui::Color32::WHITE),Default::default());
        app.cache.insert((1,false),Entry{texture,used:0});
        let frame=|app:&mut RemoteViewer,events|ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1200.,800.))),events,..Default::default()},|ui|app.content(ui));
        let locate=|out:&egui::FullOutput,label:&str|out.shapes.iter().find_map(|shape|if let egui::epaint::Shape::Text(text)=&shape.shape{if text.galley.job.text==label{return Some(text.pos+egui::vec2(3.,3.));}None}else{None}).expect(label);
        let first=frame(&mut app,vec![]);let pos=locate(&first,"smores.jpg");
        let _=frame(&mut app,vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
        let _=frame(&mut app,vec![egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
        assert_eq!(app.selected,Some(1));
        let out=frame(&mut app,vec![]);let pos=locate(&out,"Download original and open");
        let _=frame(&mut app,vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
        let _=frame(&mut app,vec![egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
        assert!(app.queue.lock().unwrap().iter().any(|(_,task)|matches!(task,Task::Original(1))));
    }
    #[test]
    fn preview_protocol_rejects_unbounded_or_truncated_pixels(){
        assert!(crate::decode::parse_pixels(b"1025 1\n").is_err());
        assert!(crate::decode::parse_pixels(b"1 1\n\0\0\0").is_err());
        assert_eq!(crate::decode::parse_pixels(b"1 1\n\0\0\0\xff").unwrap().size,[1,1]);
    }
}
