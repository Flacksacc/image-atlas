//! Asset catalog mode of the shared viewer. The photo viewer remains untouched.
use eframe::egui;
#[cfg(test)]
use postgres::{Client, NoTls};
use std::sync::Arc;
use std::time::{Duration, Instant};
#[path = "asset_search.rs"]
mod asset_search;
#[path = "asset_local.rs"]
mod asset_local;
use std::collections::HashMap;
#[path = "asset_transfer.rs"]
mod asset_transfer;
#[path = "asset_cache.rs"]
mod asset_cache;

#[derive(Clone)]
struct Pack { id:String, title:String, source:String, state:String, description:Option<String>, metadata:String, count:i64, listing_url:Option<String>, local:bool }
#[derive(Clone)]
struct Asset { id:i64, pack_id:String, name:String, path:String, class_name:String, kind:String,
    status:String, size_bytes:i64, thumbnail_source:Option<String>, metadata:String, thumbnail:Option<Arc<egui::ColorImage>>, scan_error:Option<String>, local:bool }

#[derive(Clone)]
struct OpenRequest {asset_id:i64,name:String,pack:String,unreal:bool,download_pack:bool}

fn scan_state_label(state:&str)->&str {
    match state {"complete_with_issues"=>"Scan finished with issues","complete"=>"Scan complete",
        "pending"=>"Not scanned","partial"=>"Scan interrupted / incomplete","failed"=>"Scan failed",other=>other}
}

#[derive(Clone, Copy)]
enum AssetIcon { Mesh, Texture, Material, Blueprint, Animation, Skeleton, Sound, Map, Effects, Data, Interface, Video, Font, File }

fn asset_icon(class_name:&str)->(AssetIcon,&'static str) {
    let name=class_name.rsplit('.').next().unwrap_or(class_name).to_ascii_lowercase();
    if name.contains("skelet") || name.contains("physicsasset") || name.contains("controlrig") {
        if name.contains("mesh"){return (AssetIcon::Mesh,"Skeletal mesh");}
        return (AssetIcon::Skeleton,"Rig / skeleton");
    }
    if name.contains("texture") || ["png","jpg","jpeg","tif","tiff","tga","exr","hdr","psd","psb","svg","webp","aseprite","ase","kra","cube"].contains(&name.as_str()) {return (AssetIcon::Texture,"Texture / image");}
    if name.contains("material") || ["sbsar","spp","sbs"].contains(&name.as_str()) {return (AssetIcon::Material,"Material");}
    if name.contains("niagara") || name.contains("particle") || name=="vdb" {return (AssetIcon::Effects,"Effects");}
    if name.contains("widget") || name.contains("userinterface") {return (AssetIcon::Interface,"Interface");}
    if name.contains("anim") || name.contains("blendspace") || name.contains("poseasset") || name.contains("levelsequence") {return (AssetIcon::Animation,"Animation");}
    if name.contains("sound") || name.contains("metasound") || ["wav","mp3","ogg","flac","aiff","aif","m4a"].contains(&name.as_str()) {return (AssetIcon::Sound,"Audio");}
    if name.contains("blueprint") {return (AssetIcon::Blueprint,"Blueprint");}
    if name=="world" || name=="map" || name.contains("level") || name.contains("landscape") {return (AssetIcon::Map,"World / level");}
    if name.contains("mesh") || ["fbx","obj","gltf","glb","blend","usd","usda","usdc","abc","stl","dae","3ds","ztl","zpr","ma","mb"].contains(&name.as_str()) {return (AssetIcon::Mesh,"Mesh / model");}
    if name.contains("media") || ["mp4","mov","avi"].contains(&name.as_str()) {return (AssetIcon::Video,"Media");}
    if name.contains("font") || ["ttf","otf"].contains(&name.as_str()) {return (AssetIcon::Font,"Font");}
    if name.contains("data") || name.contains("curve") || name.contains("enum") || name.contains("struct") || name=="csv" {return (AssetIcon::Data,"Data");}
    (AssetIcon::File,"Other asset")
}

/// Vector icons stay sharp at any display scale and need no symbol font.
fn asset_type_badge(ui:&mut egui::Ui,class_name:&str) {
    let (icon,label)=asset_icon(class_name);
    ui.horizontal(|ui|{
        let (rect,response)=ui.allocate_exact_size(egui::vec2(18.0,18.0),egui::Sense::hover());
        let color=match icon {
            AssetIcon::Texture|AssetIcon::Material=>egui::Color32::from_rgb(116,198,124),
            AssetIcon::Blueprint|AssetIcon::Data|AssetIcon::Interface=>egui::Color32::from_rgb(100,174,237),
            AssetIcon::Sound|AssetIcon::Animation|AssetIcon::Video=>egui::Color32::from_rgb(224,158,234),
            _=>egui::Color32::from_rgb(228,184,100),
        };
        let stroke=egui::Stroke::new(1.5,color);
        let p=|x:f32,y:f32|rect.min+egui::vec2(x,y);
        let painter=ui.painter();
        let line=|a:(f32,f32),b:(f32,f32)|{painter.line_segment([p(a.0,a.1),p(b.0,b.1)],stroke);};
        match icon {
            AssetIcon::Mesh=>{
                for (a,b) in [((9.,1.),(16.,5.)),((16.,5.),(16.,13.)),((16.,13.),(9.,17.)),((9.,17.),(2.,13.)),((2.,13.),(2.,5.)),((2.,5.),(9.,1.)),((2.,5.),(9.,9.)),((16.,5.),(9.,9.)),((9.,9.),(9.,17.))]{line(a,b);}
            }
            AssetIcon::Material=>{painter.circle_stroke(p(9.,9.),7.,stroke);painter.circle_filled(p(6.,6.),2.,color);line((9.,16.),(14.,11.));}
            AssetIcon::Sound=>{for (x,h) in [(3.,3.),(7.,7.),(11.,5.),(15.,2.)]{line((x,9.-h),(x,9.+h));}}
            AssetIcon::Animation|AssetIcon::Video=>{
                painter.rect_stroke(rect.shrink(1.5),2.,stroke,egui::StrokeKind::Inside);
                painter.add(egui::Shape::convex_polygon(vec![p(6.,4.),p(14.,9.),p(6.,14.)],color,egui::Stroke::NONE));
            }
            AssetIcon::Blueprint|AssetIcon::Skeleton=>{
                for (a,b) in [((4.,4.),(13.,9.)),((13.,9.),(4.,14.))]{line(a,b);}
                for (x,y) in [(4.,4.),(13.,9.),(4.,14.)]{painter.circle_filled(p(x,y),2.5,color);}
            }
            AssetIcon::Effects=>{
                for (a,b) in [((9.,1.),(9.,17.)),((1.,9.),(17.,9.)),((3.,3.),(15.,15.)),((3.,15.),(15.,3.))]{line(a,b);}
            }
            AssetIcon::Font=>{line((3.,16.),(9.,2.));line((9.,2.),(15.,16.));line((5.,11.),(13.,11.));}
            _=>{
                painter.rect_stroke(rect.shrink(1.5),2.,stroke,egui::StrokeKind::Inside);
                match icon {
                    AssetIcon::Texture=>{line((3.,14.),(7.,8.));line((7.,8.),(11.,12.));line((11.,12.),(15.,7.));painter.circle_filled(p(6.,5.),1.5,color);}
                    AssetIcon::Map|AssetIcon::Data=>{line((7.,2.),(7.,16.));line((12.,2.),(12.,16.));line((2.,7.),(16.,7.));line((2.,12.),(16.,12.));}
                    AssetIcon::Interface=>{line((2.,6.),(16.,6.));line((7.,6.),(7.,16.));}
                    _=>{line((5.,6.),(13.,6.));line((5.,10.),(13.,10.));line((5.,14.),(10.,14.));}
                }
            }
        }
        response.on_hover_text(format!("{label}\n{class_name}"));
        ui.add(egui::Label::new(egui::RichText::new(label).small()).truncate())
            .on_hover_text(class_name);
    });
}

pub(super) struct AssetViewer {
    dsn:String, packs:Arc<Vec<Pack>>, assets:Arc<Vec<Asset>>, textures:HashMap<i64,egui::TextureHandle>,
    selected_pack:String, selected_classes:Vec<String>, class_options:Arc<Vec<String>>, type_query:String, query:String, pack_query:String, offset:i64, selected:Option<i64>,
    error:String, total:i64, show_packs:bool, indexed_only:bool, local_only:bool, pack_files:Vec<(String,String,i64)>,
    transfer:Option<asset_transfer::Transfer>, transfer_message:String, transfer_progress:Option<f32>,
    project_path:String, managed_preview:bool, storage_status:String, storage_asset:Option<i64>,
    transfer_check:bool, open_request:Option<OpenRequest>, pending_activation:Option<OpenRequest>,
    loading_asset:Option<OpenRequest>,
    cache:asset_cache::Panel,
    #[cfg(test)] started_open:Vec<OpenRequest>,
    #[cfg(test)] confirmation_rects:(egui::Rect,egui::Rect),
    #[cfg(test)] loading_cancel_rect:egui::Rect,
    worker:Option<asset_search::Worker>, generation:u64, catalog_version:u64, searching:bool,
    pending_search:Option<Instant>, repaint:egui::Context, texture_uploads:usize,
}

impl AssetViewer {
    pub(super) fn open(dsn:String,ctx:egui::Context)->Self {
        let mut app=Self::empty(dsn);app.repaint=ctx;app.refresh();app
    }
    pub(super) fn background(&mut self,ctx:&egui::Context) {
        self.poll_transfer(ctx);self.poll_search(ctx);
        if self.cache.poll(ctx){self.catalog_version+=1;self.refresh();self.storage_asset=None;self.storage_status.clear();}
    }
    #[cfg(test)]
    fn new(dsn:String)->Self {
        let mut app=Self::empty(dsn);
        app.refresh();app
    }
    fn empty(dsn:String)->Self {
        Self{dsn,packs:Arc::new(Vec::new()),assets:Arc::new(Vec::new()),textures:HashMap::new(),
            selected_pack:String::new(),selected_classes:Vec::new(),class_options:Arc::new(Vec::new()),type_query:String::new(),query:String::new(),pack_query:String::new(),offset:0,
            selected:None,error:String::new(),total:0,show_packs:true,indexed_only:false,local_only:false,pack_files:Vec::new(),
            transfer:None,transfer_message:String::new(),transfer_progress:None,project_path:String::new(),managed_preview:true,
            storage_status:String::new(),storage_asset:None,worker:None,generation:0,catalog_version:0,searching:false,
            transfer_check:false,open_request:None,pending_activation:None,
            loading_asset:None,
            cache:asset_cache::Panel::new(),
            #[cfg(test)] started_open:Vec::new(),
            #[cfg(test)] confirmation_rects:(egui::Rect::NOTHING,egui::Rect::NOTHING),
            #[cfg(test)] loading_cancel_rect:egui::Rect::NOTHING,
            pending_search:None,repaint:egui::Context::default(),texture_uploads:0}
    }
    #[cfg(test)]
    fn connect(&self)->Result<Client,String> {
        Client::connect(&self.dsn,NoTls).map_err(|e|e.to_string())
    }
    fn schedule_search(&mut self,debounce:bool) {
        self.generation+=1;
        self.searching=true;
        self.error.clear();
        if let Some(worker)=&self.worker {worker.invalidate(self.generation);}
        self.pending_search=Some(Instant::now()+if debounce{Duration::from_millis(250)}else{Duration::ZERO});
        self.repaint.request_repaint();
    }
    fn refresh(&mut self) {self.schedule_search(false);}
    fn poll_search(&mut self,ctx:&egui::Context) {
        if self.pending_search.is_some_and(|when|Instant::now()>=when) {
            self.pending_search=None;
            let worker=self.worker.get_or_insert_with(||asset_search::Worker::start(self.dsn.clone(),ctx.clone()));
            worker.submit(asset_search::Request{generation:self.generation,catalog_version:self.catalog_version,
                query:self.query.clone(),pack:self.selected_pack.clone(),classes:self.selected_classes.clone(),
                indexed_only:self.indexed_only,local_only:self.local_only,offset:self.offset});
        }
        if let Some(worker)=&self.worker {
            while let Ok(reply)=worker.replies.try_recv() {
                if reply.generation!=self.generation {continue;}
                self.searching=false;
                match reply.result {
                    Ok(result)=>{
                        self.packs=result.packs;self.class_options=result.classes;self.assets=result.assets;
                        self.total=result.total;self.offset=result.offset;self.pack_files=result.files;
                        self.textures.clear();self.error.clear();
                        if !self.assets.iter().any(|a|Some(a.id)==self.selected){self.selected=None;}
                    },Err(error)=>self.error=error,
                }
            }
        }
        if self.searching {ctx.request_repaint_after(Duration::from_millis(16));}
    }
    fn type_filter(&mut self,ui:&mut egui::Ui)->bool {
        let mut changed=false;
        let label=if self.selected_classes.is_empty(){"Any type".to_owned()}
            else{format!("{} types selected",self.selected_classes.len())};
        egui::ComboBox::from_id_salt("asset-types").selected_text(label).width(180.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show_ui(ui,|ui|{
                ui.add(egui::TextEdit::singleline(&mut self.type_query).hint_text("Find types…"));
                if ui.button("All types (clear selection)").clicked(){changed|=!self.selected_classes.is_empty();self.selected_classes.clear();}
                let needle=self.type_query.to_lowercase();
                let options=self.class_options.clone();
                if options.is_empty(){ui.label(if self.searching{"Loading types…"}else{"No asset types available"});}
                egui::ScrollArea::vertical().max_height(300.0).show(ui,|ui|{
                    for class in options.iter().filter(|c|c.to_lowercase().contains(&needle)) {
                        changed|=self.type_checkbox(ui,class).changed();
                    }
                });
            });
        changed
    }
    fn type_checkbox(&mut self,ui:&mut egui::Ui,class:&str)->egui::Response {
        let mut checked=self.selected_classes.iter().any(|c|c==class);
        let response=ui.checkbox(&mut checked,class);
        if response.changed() {
            if checked{self.selected_classes.push(class.to_owned());self.selected_classes.sort();}
            else{self.selected_classes.retain(|c|c!=class);}
        }
        response
    }
    fn texture(&mut self,ctx:&egui::Context,asset:&Asset)->Option<egui::TextureHandle> {
        if !self.textures.contains_key(&asset.id) {
            let pixels=asset.thumbnail.as_ref()?;
            if self.texture_uploads>=4 {ctx.request_repaint();return None;}
            self.texture_uploads+=1;
            let texture=ctx.load_texture(format!("asset-{}",asset.id),pixels.clone(),egui::TextureOptions::LINEAR);
            self.textures.insert(asset.id,texture);
        }
        self.textures.get(&asset.id).cloned()
    }
    fn pack_title(&self,id:&str)->String {
        self.packs.iter().find(|p|p.id==id).map(|p|p.title.clone()).unwrap_or_else(||id.into())
    }

    fn asset_response(&mut self,asset:&Asset,response:egui::Response) {
        if response.clicked(){self.selected=Some(asset.id);}
        if response.double_clicked(){
            let request=OpenRequest{asset_id:asset.id,name:asset.name.clone(),pack:self.pack_title(&asset.pack_id),
                unreal:asset.kind=="unreal",download_pack:!asset.local};
            if self.transfer.is_some() {
                if self.transfer_check {
                    self.loading_asset=Some(request.clone());self.pending_activation=Some(request);
                    self.transfer_message="Checking asset availability…".into();
                }
                else {self.transfer_message="An operation is already running. Wait for it to finish or cancel it before opening another asset.".into();}
            }else{self.activate(request);}
        }
    }

    fn activate(&mut self,request:OpenRequest) {
        if self.cache.clearing(){self.error="Wait for cache deletion to finish before opening another asset.".into();return;}
        if request.download_pack||(request.unreal&&!self.managed_preview&&self.project_path.trim().is_empty()) {
            self.open_request=Some(request);
        }else{self.start_open(request);}
    }

    fn start_open(&mut self,request:OpenRequest) {
        self.loading_asset=Some(request.clone());
        self.error.clear();self.transfer_progress=None;self.transfer_check=false;
        self.transfer_message=if request.download_pack{format!("Downloading entire pack: {}",request.pack)}else{format!("Opening {}…",request.name)};
        #[cfg(not(test))] {
            let project=(request.unreal&&!self.managed_preview&&!self.project_path.trim().is_empty()).then(||self.project_path.trim().to_owned());
            self.transfer=Some(asset_transfer::start_open(request.asset_id,project,request.download_pack,&self.dsn));
        }
        #[cfg(test)] self.started_open.push(request);
    }

    pub(super) fn is_loading(&self)->bool {self.loading_asset.is_some()}

    pub(super) fn loading_dialog(&mut self,ctx:&egui::Context) {
        let Some(asset)=self.loading_asset.clone() else {return;};
        let stopping=self.transfer.as_ref().is_some_and(|t|t.cancel.load(std::sync::atomic::Ordering::Relaxed));
        egui::Modal::new(egui::Id::new("asset-loading-progress")).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-40.0).clamp(200.0,460.0));
            ui.horizontal(|ui|{ui.spinner();ui.heading(if stopping{"Cancelling asset loading…"}else{"Loading asset…"});});
            ui.label(egui::RichText::new(&asset.name).strong());
            ui.label(&asset.pack);
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-240.0).clamp(60.0,200.0))
                .show(ui,|ui|{ui.label(&self.transfer_message);});
            if let Some(progress)=self.transfer_progress {
                ui.add(egui::ProgressBar::new(progress).show_percentage());
            }else{ui.label("Working… Progress updates will appear here.");}
            ui.add_space(8.0);
            let button=ui.add_enabled(!stopping,egui::Button::new(if stopping{"Cancelling…"}else{"Cancel loading"}));
            #[cfg(test)] {self.loading_cancel_rect=button.rect;}
            if button.clicked() {
                self.pending_activation=None;
                if let Some(transfer)=&self.transfer {
                    transfer.cancel.store(true,std::sync::atomic::Ordering::Relaxed);
                    self.transfer_message="Cancelling… Waiting for the operation to stop safely.".into();
                }
            }
        });
        // Backdrop clicks and Escape must never dismiss an operation still running.
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn open_confirmation(&mut self,ctx:&egui::Context) {
        let Some(request)=self.open_request.clone() else {return;};
        let mut cancel=false;let mut confirm=false;
        let modal=egui::Modal::new(egui::Id::new("asset-open-confirmation")).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-40.0).clamp(200.0,420.0));
            ui.heading(if request.download_pack{"Download entire asset pack?"}else{"Choose an Unreal project"});
            if request.download_pack {
                ui.label(format!("Download the entire ‘{}’ pack to this computer before opening ‘{}’?",request.pack,request.name));
                ui.label("The full pack can be much larger than an individual asset. The download will show progress and can be cancelled.");
            }
            if request.unreal&&self.managed_preview {
                ui.label("The asset will open in its managed preview studio. No project setup is needed.");
            }else if request.unreal {
                ui.label("Unreal assets open in a target project. Required files will be imported into that project; existing different files are preserved.");
                ui.label("Target .uproject");
                ui.add(egui::TextEdit::singleline(&mut self.project_path).desired_width(ui.available_width()).hint_text("D:\\Projects\\Game\\Game.uproject"));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui|{
                let cancel_button=ui.button("Cancel");
                let confirm_button=ui.add_enabled(!request.unreal||self.managed_preview||!self.project_path.trim().is_empty(),egui::Button::new(
                    if request.download_pack{"Download entire pack and open"}else{"Open in project"}));
                cancel=cancel_button.clicked();confirm=confirm_button.clicked();
                #[cfg(test)] {self.confirmation_rects=(cancel_button.rect,confirm_button.rect);}
            });
        });
        if cancel||modal.should_close(){self.open_request=None;}
        else if confirm {self.open_request=None;self.start_open(request);}
    }

    fn select_pack(&mut self,id:String) {
        if self.selected_pack!=id {self.selected_pack=id;self.pack_files.clear();self.offset=0;self.selected=None;self.refresh();}
    }

    fn indexed_checkbox(&mut self,ui:&mut egui::Ui)->egui::Response {
        ui.checkbox(&mut self.indexed_only,"Indexed only")
            .on_hover_text("Show only assets with indexed status; hide discovered and registry-only assets.")
    }

    fn local_checkbox(&mut self,ui:&mut egui::Ui)->egui::Response {
        ui.checkbox(&mut self.local_only,"Locally stored")
            .on_hover_text("Show source files stored on this computer or configured LAN storage, including SSH archives and shared folders. SSH archive records count even while offline; retrieval checks connectivity and verifies files. Previews do not count.")
    }

    fn pack_panel(&mut self,ui:&mut egui::Ui) {
        ui.heading(format!("Packs ({})",self.packs.len()));
        ui.add(egui::TextEdit::singleline(&mut self.pack_query).hint_text("Find owned packs…"));
        let needle=self.pack_query.to_lowercase();
        let rows:Vec<_>=self.packs.iter().filter(|p|p.title.to_lowercase().contains(&needle)&&(!self.local_only||p.local))
            .map(|p|(p.id.clone(),p.title.clone(),p.state.clone(),p.count,p.local)).collect();
        ui.label(format!("{} shown · {} scanned",rows.len(),self.packs.iter().filter(|p|p.count>0).count()));
        ui.separator();
        if ui.selectable_label(self.selected_pack.is_empty(),"All indexed assets").clicked(){self.select_pack(String::new());}
        egui::ScrollArea::vertical().id_salt("asset-pack-list").show_rows(ui,42.0,rows.len(),|ui,visible|{
            for row in visible {
                let (id,title,state,count,local)=&rows[row];
                let subtitle=if *count>0{format!("{} · {count} assets",scan_state_label(&state))}else{scan_state_label(&state).to_owned()};
                ui.horizontal(|ui|{
                    if *local {local_badge(ui,"This pack has assets stored locally or on configured LAN storage");}
                    if ui.add_sized([ui.available_width(),42.0],egui::Button::new(format!("{title}\n{subtitle}")).selected(self.selected_pack==*id).truncate())
                        .on_hover_text(format!("{title}\n{subtitle}\n{id}")).clicked(){self.select_pack(id.clone());}
                });
            }
        });
    }

    fn details_panel(&mut self,ui:&mut egui::Ui) {
        ui.heading("Details");ui.separator();
        egui::ScrollArea::vertical().id_salt("asset-details").show(ui,|ui|{
            if let Some(asset)=self.assets.iter().find(|a|Some(a.id)==self.selected).cloned(){
                if let Some(texture)=self.texture(ui.ctx(),&asset){
                    let size=texture.size_vec2();let scale=(ui.available_width()/size.x).min(320.0/size.y).min(1.0);
                    ui.image((texture.id(),size*scale));
                }
                ui.heading(&asset.name);if asset.local {local_badge(ui,"Source file is stored locally or on configured LAN storage");ui.label("Stored locally or on LAN");}asset_type_badge(ui,&asset.class_name);ui.label(&asset.class_name);ui.label(&asset.path);
                copyable_identifier(ui,"CLI asset ID",&asset.id.to_string());
                copyable_identifier(ui,"Pack ID",&asset.pack_id);
                ui.label(format!("Pack: {}",self.pack_title(&asset.pack_id)));
                ui.label(format!("Size: {} bytes · Status: {}",asset.size_bytes,asset.status));
                if let Some(issue)=&asset.scan_error {ui.label(format!("Issue: {issue}"));}
                ui.label(format!("Preview: {}",asset.thumbnail_source.as_deref().unwrap_or("unavailable")));
                self.retrieval_controls(ui,asset.id);
                ui.separator();ui.collapsing("Metadata",|ui|{ui.monospace(pretty_json(&asset.metadata));});
            }else if let Some(pack)=self.packs.iter().find(|p|p.id==self.selected_pack){
                ui.heading(&pack.title);
                if pack.local {local_badge(ui,"This pack has assets stored locally or on configured LAN storage");ui.label("Contains assets stored locally or on LAN");}
                copyable_identifier(ui,"Pack ID",&pack.id);
                let source=match pack.source.as_str(){"fab"=>"Fab","uem"=>"Epic Marketplace","epic-launcher"=>"Epic Games Launcher",other=>other};
                let state=scan_state_label(&pack.state);
                ui.label(format!("{source} · {state}"));
                ui.label(format!("{} indexed assets",pack.count));
                if let Some(description)=&pack.description{ui.label(description);}
                if let Some(url)=&pack.listing_url{ui.hyperlink_to("Open Fab listing",url);}
                ui.separator();crate::asset_metadata::show_pack_metadata(ui,&pack.metadata);
                if !self.pack_files.is_empty(){ui.collapsing(format!("Other files ({})",self.pack_files.len()),|ui|{
                    for (path,role,size) in &self.pack_files{ui.label(format!("{path} · {role} · {size} B"));}
                });}
            }else{ui.label("Select an asset or pack to see its details.");}
        });
    }

    fn retrieval_controls(&mut self,ui:&mut egui::Ui,asset_id:i64) {
        if self.storage_asset!=Some(asset_id) {
            self.storage_asset=Some(asset_id);self.storage_status.clear();
            if self.transfer.is_none()&&self.open_request.is_none()&&self.pending_activation.is_none() {
                self.transfer_check=true;self.transfer=Some(asset_transfer::start(asset_id,None,true,&self.dsn));
            }
        }
        if !self.storage_status.is_empty(){ui.label(&self.storage_status);}
        let idle=self.transfer.is_none();
        ui.horizontal_wrapped(|ui|{
            if ui.add_enabled(idle,egui::Button::new("Fetch locally")).clicked(){
                self.transfer_check=false;
                self.error.clear();self.transfer_progress=None;
                self.transfer_message="Preparing retrieval…".into();
                self.transfer=Some(asset_transfer::start(asset_id,None,false,&self.dsn));
            }
            if ui.add_enabled(idle,egui::Button::new("Check storage")).clicked(){
                self.transfer_check=true;
                self.error.clear();
                self.transfer=Some(asset_transfer::start(asset_id,None,true,&self.dsn));
            }
        });
        ui.checkbox(&mut self.managed_preview,"Double-click in managed preview")
            .on_hover_text("Automatically stage the asset in its curated Unreal studio. Separate pack workspaces preserve original references; compatible sessions are reused.");
        ui.add(egui::TextEdit::singleline(&mut self.project_path).hint_text("Optional project for import or manual opening"));
        if ui.add_enabled(idle&&!self.project_path.trim().is_empty(),egui::Button::new("Import into project")).clicked(){
            self.transfer_check=false;
            self.error.clear();self.transfer_progress=None;
            self.transfer_message="Checking project…".into();
            self.transfer=Some(asset_transfer::start(asset_id,Some(self.project_path.trim().into()),false,&self.dsn));
        }
    }

    fn poll_transfer(&mut self,ctx:&egui::Context) {
        let mut finished=false;
        let mut reload=false;
        if let Some(transfer)=&self.transfer {
            loop {
                let message=match transfer.messages.try_recv() {
                    Ok(message)=>message,
                    Err(crossbeam_channel::TryRecvError::Empty)=>break,
                    Err(crossbeam_channel::TryRecvError::Disconnected)=>{
                        if !finished&&self.error.is_empty(){self.error="Asset operation stopped before reporting completion. Retry the asset.".into();}
                        finished=true;break;
                    },
                };
                match message {
                    asset_transfer::Message::Event(event)=>{
                        match event["event"].as_str().unwrap_or("") {
                            "progress"=>{
                                self.transfer_message=format!("{}: {}",event["stage"].as_str().unwrap_or("Working"),event["detail"].as_str().unwrap_or(""));
                                self.transfer_progress=event["total"].as_f64().filter(|v|*v>0.)
                                    .map(|v|(event["completed"].as_f64().unwrap_or(0.)/v).clamp(0.,1.) as f32);
                            },
                            "availability"=>{
                                if event["asset_id"].as_i64()==self.storage_asset {
                                    let names=event["locations"].as_array().map(|items|items.iter().filter_map(|v|v.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                                    self.storage_status=if event["local"].as_bool()==Some(true){"Package cached locally".into()}
                                        else if event["archived"].as_bool()==Some(true){format!("Archived in {names}; connection checked when retrieving")}
                                        else{"No archived source; download this pack to a storage location first.".into()};
                                }
                            },
                            "error"=>{
                                self.error=event["error"].as_str().unwrap_or("Retrieval failed").into();
                                if let Some(report)=event["failure_report"]["report"].as_str(){
                                    self.error.push_str(&format!("\nDetails: {report}"));
                                }
                                if let Some(step)=event["failure_report"]["next_steps"][0].as_str(){
                                    self.error.push_str(&format!("\n{step}"));
                                }
                            },
                            "result"=>{
                                reload=true;
                                self.transfer_message=if event["managed_preview"]==true {
                                    format!("Opened in {} studio",event["category"].as_str().unwrap_or("preview"))
                                }else if event["opened"]==true {
                                    format!("Opened: {}",event["project"].as_str().or(event["open_path"].as_str()).unwrap_or("asset"))
                                }else{format!("Ready: {} files · {}",event["files"],event["project"].as_str().or(event["cache"].as_str()).unwrap_or(""))};
                                if let Some(warnings)=event["warnings"].as_array() {
                                    for warning in warnings.iter().filter_map(|v|v.as_str()) {
                                        self.transfer_message.push_str(&format!("\n{warning}"));
                                    }
                                }
                                self.transfer_progress=Some(1.);
                            },_=>{}
                        }
                    },
                    asset_transfer::Message::Done(result)=>{
                        if let Err(error)=result { if self.error.is_empty(){self.error=error;} }
                        finished=true;
                    }
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if finished {
            self.transfer=None;self.transfer_check=false;self.loading_asset=None;
            self.cache.refresh(ctx);
            if let Some(request)=self.pending_activation.take(){self.activate(request);}
        }
        if reload&&!self.assets.is_empty(){self.catalog_version+=1;self.refresh();}
    }

    fn grid_panel(&mut self,ui:&mut egui::Ui) {
        if self.searching&&!self.assets.is_empty(){ui.label("Searching… Previous results are shown until the search finishes.");}
        ui.horizontal(|ui|{
            let start=if self.total==0{0}else{self.offset+1};
            ui.label(format!("{start}–{} of {} assets",(self.offset+self.assets.len() as i64).min(self.total),self.total));
            if ui.add_enabled(!self.searching&&self.offset>0,egui::Button::new("Previous")).clicked(){self.offset=(self.offset-200).max(0);self.selected=None;self.refresh();}
            if ui.add_enabled(!self.searching&&self.offset+200<self.total,egui::Button::new("Next")).clicked(){self.offset+=200;self.selected=None;self.refresh();}
        });ui.separator();
        if self.assets.is_empty(){
            if self.searching {ui.label("Searching the catalog…");return;}
            let message=if self.selected_pack.is_empty(){"No indexed assets match these filters. Find owned packs in the left panel."}
                else if self.query.is_empty()&&self.selected_classes.is_empty()&&!self.indexed_only&&!self.local_only{"This pack has no indexed assets yet. Its listing details are on the right; scan it to see files."}
                else{"No assets in this pack match these filters."};
            ui.label(message);return;
        }
        let columns=((ui.available_width()/166.0).floor() as usize).max(1);
        let rows=Arc::clone(&self.assets);
        egui::ScrollArea::vertical().id_salt("asset-grid").show_rows(ui,240.0,rows.len().div_ceil(columns),|ui,visible|{
            for row in visible {let chunk=&rows[row*columns..((row+1)*columns).min(rows.len())];ui.horizontal(|ui|{for asset in chunk{
                egui::Frame::group(ui.style()).show(ui,|ui|{
                    ui.vertical(|ui|{
                        ui.set_width(150.0);
                        ui.horizontal(|ui|{
                            if asset.local {local_badge(ui,"Source file stored locally or on LAN");}
                            asset_type_badge(ui,&asset.class_name);
                        });
                        let selected=self.selected==Some(asset.id);
                        if let Some(texture)=self.texture(ui.ctx(),asset){
                            let size=texture.size_vec2();let scale=(140.0/size.x).min(140.0/size.y).min(1.0);
                            let response=ui.add_sized([140.0,140.0],egui::Image::new((texture.id(),size*scale)).sense(egui::Sense::click()))
                                .on_hover_text(format!("{}\n{}\nDouble-click to open",asset.path,asset.class_name));
                            self.asset_response(asset,response);
                        }else {
                            let response=ui.add_sized([140.0,140.0],egui::Button::new(format!("{}\n{}",if asset.kind=="unreal"{"◈"}else{"◇"},asset.class_name)))
                                .on_hover_text("Double-click to open");
                            self.asset_response(asset,response);
                        }
                        let response=ui.add_sized([140.0,36.0],egui::Button::new(&asset.name).selected(selected).truncate())
                            .on_hover_text(format!("{}\nDouble-click to open",asset.name));
                        self.asset_response(asset,response);
                    });
                });
            }});}
        });
    }
}

fn local_badge(ui:&mut egui::Ui,tooltip:&str) {
    let (rect,response)=ui.allocate_exact_size(egui::vec2(18.,18.),egui::Sense::hover());
    let stroke=egui::Stroke::new(1.5,ui.visuals().text_color());
    let p=ui.painter();
    p.rect_stroke(rect.shrink2(egui::vec2(1.,4.)),2.,stroke,egui::StrokeKind::Inside);
    p.line_segment([rect.min+egui::vec2(2.,10.),rect.min+egui::vec2(16.,10.)],stroke);
    p.circle_filled(rect.min+egui::vec2(13.,12.),1.,stroke.color);
    response.on_hover_text(tooltip);
}

fn pretty_json(raw:&str)->String {
    serde_json::from_str::<serde_json::Value>(raw)
        .and_then(|value|serde_json::to_string_pretty(&value)).unwrap_or_else(|_|raw.to_owned())
}

fn copyable_identifier(ui:&mut egui::Ui,label:&str,value:&str) {
    ui.push_id(label,|ui|{
        ui.label(egui::RichText::new(label).strong());
        ui.horizontal_wrapped(|ui|{
            ui.add(egui::Label::new(egui::RichText::new(value).monospace()).wrap());
            if ui.small_button("Copy").on_hover_text(format!("Copy {label}")).clicked(){ui.ctx().copy_text(value.to_owned());}
        });
    });
}

impl eframe::App for AssetViewer {
    fn ui(&mut self,ui:&mut egui::Ui,_frame:&mut eframe::Frame) {
        self.draw(ui);
        self.loading_dialog(ui.ctx());
    }
}
impl AssetViewer {
    pub(super) fn draw(&mut self,ui:&mut egui::Ui) {
        self.texture_uploads=0;
        self.poll_transfer(ui.ctx());
        self.poll_search(ui.ctx());
        if self.cache.poll(ui.ctx()){self.catalog_version+=1;self.refresh();self.storage_asset=None;self.storage_status.clear();}
        ui.add_enabled_ui(!self.is_loading(),|ui|self.draw_body(ui));
        self.open_confirmation(ui.ctx());
    }
    fn draw_body(&mut self,ui:&mut egui::Ui) {
        egui::Panel::top("asset-toolbar").show(ui,|ui|{
            ui.horizontal(|ui|{
                ui.heading("Asset CLI");
                if ui.button("Refresh").clicked(){self.catalog_version+=1;self.refresh();}
                ui.checkbox(&mut self.show_packs,"Packs");
                ui.label(format!("{} owned packs",self.packs.len()));
                if self.searching {ui.spinner();ui.label("Searching…");}
            });
            let mut changed=false;
            ui.horizontal_wrapped(|ui|{
                ui.label("Assets");
                changed|=ui.add(egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search names, paths, metadata…").desired_width(300.0)).changed();
                ui.label("Type");
                changed|=self.type_filter(ui);
                changed|=self.indexed_checkbox(ui).changed();
                changed|=self.local_checkbox(ui).changed();
                if ui.button("Clear filters").clicked(){self.query.clear();self.selected_classes.clear();self.indexed_only=false;self.local_only=false;changed=true;}
            });
            if changed{self.offset=0;self.selected=None;self.schedule_search(true);}
            if !self.error.is_empty(){ui.colored_label(egui::Color32::RED,&self.error);}
            if !self.transfer_message.is_empty(){ui.label(&self.transfer_message);}
            if let Some(progress)=self.transfer_progress {ui.add(egui::ProgressBar::new(progress).show_percentage());}
            if let Some(transfer)=&self.transfer {
                if ui.button("Cancel operation").clicked(){
                    transfer.cancel.store(true,std::sync::atomic::Ordering::Relaxed);
                    self.transfer_message="Cancelling…".into();
                }
            }
            let can_clear=self.transfer.is_none()&&!self.is_loading();
            self.cache.ui(ui,can_clear);
        });
        if self.show_packs{
            egui::Panel::left("asset-packs").default_size(285.0).size_range(220.0..=420.0)
                .show(ui,|ui|self.pack_panel(ui));
        }
        egui::Panel::right("asset-details-panel").default_size(350.0).size_range(280.0..=520.0)
            .show(ui,|ui|self.details_panel(ui));
        self.grid_panel(ui);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn activation_asset(local:bool,unreal:bool)->Asset {
        Asset{id:7,pack_id:"pack".into(),name:"Selected asset".into(),path:"/Game/Selected.Selected".into(),
            class_name:"Texture2D".into(),kind:if unreal{"unreal"}else{"source"}.into(),status:"indexed".into(),size_bytes:10,
            thumbnail_source:None,metadata:"{}".into(),thumbnail:None,scan_error:None,local}
    }
    fn click(pos:egui::Pos2)->Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos),
            egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE},
            egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE}]
    }
    #[test]
    fn double_click_opens_stored_assets_and_confirms_online_assets() {
        for local in [true,false] {
            let ctx=egui::Context::default();let mut app=AssetViewer::empty(String::new());let asset=activation_asset(local,false);
            let mut rect=egui::Rect::NOTHING;
            let _=ctx.run_ui(egui::RawInput{time:Some(0.),..Default::default()},|ui|{
                let response=ui.add_sized([140.,140.],egui::Button::new("Asset"));rect=response.rect;app.asset_response(&asset,response);
            });
            for time in [0.1,0.2] {
                let _=ctx.run_ui(egui::RawInput{time:Some(time),events:click(rect.center()),..Default::default()},|ui|{
                    app.asset_response(&asset,ui.add_sized([140.,140.],egui::Button::new("Asset")));
                });
                assert_eq!(app.selected,Some(7));
                if time==0.1 {assert!(app.open_request.is_none());assert!(app.started_open.is_empty());}
            }
            if local {assert_eq!(app.started_open.len(),1);assert!(!app.started_open[0].download_pack);}
            else {assert!(app.started_open.is_empty());assert!(app.open_request.as_ref().unwrap().download_pack);}
        }
    }
    #[test]
    fn download_confirmation_cancel_and_accept_work_in_both_themes_and_sizes() {
        for dark in [true,false] {for width in [360.,1000.] {for accept in [true,false] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut app=AssetViewer::empty(String::new());
            app.open_request=Some(OpenRequest{asset_id:7,name:"Selected asset".into(),pack:"An entire asset pack".into(),unreal:false,download_pack:true});
            for _ in 0..2 {
                let out=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,700.))),..Default::default()},|ui|app.open_confirmation(ui.ctx()));
                assert!(!out.shapes.is_empty());
            }
            assert!(app.started_open.is_empty());
            let rect=if accept{app.confirmation_rects.1}else{app.confirmation_rects.0};
            assert!(rect.min.x>=0.&&rect.max.x<=width,"confirmation button out of viewport: {rect:?}");
            let _=ctx.run_ui(egui::RawInput{events:click(rect.center()),..Default::default()},|ui|app.open_confirmation(ui.ctx()));
            assert!(app.open_request.is_none());
            assert_eq!(app.started_open.len(),usize::from(accept));
            if accept {assert!(app.started_open[0].download_pack);assert_eq!(app.started_open[0].asset_id,7);}
        }}}
    }
    #[test]
    fn stored_unreal_uses_managed_studio_without_project_prompt() {
        let mut app=AssetViewer::empty(String::new());
        assert!(app.managed_preview);
        app.activate(OpenRequest{asset_id:7,name:"Mesh".into(),pack:"Pack".into(),unreal:true,download_pack:false});
        assert!(app.open_request.is_none());assert_eq!(app.started_open.len(),1);
        assert!(!app.started_open[0].download_pack);
    }
    #[test]
    fn unreal_requires_project_and_escape_never_downloads() {
        let ctx=egui::Context::default();let mut app=AssetViewer::empty(String::new());
        app.managed_preview=false;
        let request=OpenRequest{asset_id:7,name:"Mesh".into(),pack:"Pack".into(),unreal:true,download_pack:false};
        app.activate(request.clone());assert!(app.open_request.is_some());assert!(app.started_open.is_empty());
        for _ in 0..2 {let _=ctx.run_ui(egui::RawInput::default(),|ui|app.open_confirmation(ui.ctx()));}
        let _=ctx.run_ui(egui::RawInput{events:vec![egui::Event::Key{key:egui::Key::Escape,physical_key:None,pressed:true,repeat:false,modifiers:egui::Modifiers::NONE}],..Default::default()},|ui|app.open_confirmation(ui.ctx()));
        assert!(app.open_request.is_none());assert!(app.started_open.is_empty());
        app.project_path="D:/Project/Game.uproject".into();app.activate(request);
        assert_eq!(app.started_open.len(),1);assert!(!app.started_open[0].download_pack);
    }
    #[test]
    fn availability_check_does_not_swallow_pending_double_click() {
        let ctx=egui::Context::default();let mut app=AssetViewer::empty(String::new());
        let (sender,messages)=crossbeam_channel::unbounded();
        app.transfer=Some(asset_transfer::Transfer{messages,cancel:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))});
        app.transfer_check=true;
        app.pending_activation=Some(OpenRequest{asset_id:7,name:"Selected".into(),pack:"Pack".into(),unreal:false,download_pack:false});
        sender.send(asset_transfer::Message::Done(Ok(()))).unwrap();app.poll_transfer(&ctx);
        assert_eq!(app.started_open.len(),1);assert!(app.pending_activation.is_none());
    }
    fn wait_search(app:&mut AssetViewer) {
        let deadline=Instant::now()+Duration::from_secs(120);
        let ctx=egui::Context::default();
        while app.searching {
            app.poll_search(&ctx);
            assert!(Instant::now()<deadline,"search timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn multitype_filter_uses_catalog_options_and_exact_or_matching() {
        let Ok(dsn)=std::env::var("ASSET_CLI_TEST_DSN") else {return;};
        let mut app=AssetViewer::new(dsn);
        wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert!(app.class_options.len()>1);
        let options=app.class_options.clone();
        app.selected_classes=options.iter().take(2).cloned().collect();
        app.indexed_only=true;
        let expected:i64=app.connect().unwrap().query_one(
            "SELECT count(*) FROM assets WHERE class_name=ANY($1) AND scan_status='indexed'",&[&app.selected_classes]).unwrap().get(0);
        app.refresh();wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert_eq!(app.total,expected);
        assert!(app.assets.iter().all(|a|app.selected_classes.contains(&a.class_name)&&a.status=="indexed"));
        assert!(Arc::ptr_eq(&options,&app.class_options),"ordinary searches should reuse catalog/type options");
    }
    #[test]
    fn slow_search_keeps_filters_interactive_in_both_themes() {
        let (started,starts)=crossbeam_channel::bounded(1);
        let (release,releases)=crossbeam_channel::bounded(1);
        let mut app=AssetViewer::empty(String::new());
        app.worker=Some(asset_search::Worker::with_loader(egui::Context::default(),move |_,_| {
            started.send(()).unwrap();releases.recv().unwrap();Err("Test search failure".into())
        }));
        app.refresh();app.poll_search(&egui::Context::default());
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(app.searching);
        for dark in [true,false] {for width in [650.0,1250.0] {
            let ctx=egui::Context::default();
            ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut rect=egui::Rect::NOTHING;
            let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,850.0))),..Default::default()},|ui|{
                app.poll_search(ui.ctx());ui.spinner();ui.label("Searching…");app.type_filter(ui);
                rect=app.type_checkbox(ui,"StaticMesh").rect;app.grid_panel(ui);
            });
            assert!(!output.shapes.is_empty());
            let pos=rect.center();
            let input=egui::RawInput{events:vec![egui::Event::PointerMoved(pos),
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE},
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE}],..Default::default()};
            let _=ctx.run_ui(input,|ui|{app.poll_search(ui.ctx());ui.spinner();ui.label("Searching…");app.type_filter(ui);app.type_checkbox(ui,"StaticMesh");app.grid_panel(ui);});
            assert!(app.selected_classes.contains(&"StaticMesh".into()));
            app.selected_classes.clear();
            assert!(app.searching);
        }}
        release.send(()).unwrap();wait_search(&mut app);
        assert_eq!(app.error,"Test search failure");
    }
    #[test]
    fn retrieval_controls_render_in_both_themes_and_narrow_panels() {
        for dark in [true,false] {
            for width in [280.0,520.0] {
                let mut app=AssetViewer::empty(String::new());
                app.storage_asset=Some(7); // Suppress background lookup in a rendering test.
                app.storage_status="Archived in archive; connection checked when retrieving".into();
                let ctx=egui::Context::default();
                ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
                let output=ctx.run_ui(egui::RawInput{
                    screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,600.0))),
                    ..Default::default()
                },|ui|app.retrieval_controls(ui,7));
                assert!(!output.shapes.is_empty());
                assert!(app.transfer.is_none());
                assert!(app.project_path.is_empty());
            }
        }
    }
    #[test]
    fn loading_dialog_blocks_background_and_waits_for_cancel_completion() {
        for dark in [true,false] {for width in [360.,1000.] {
            let ctx=egui::Context::default();ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut app=AssetViewer::empty(String::new());
            app.start_open(OpenRequest{asset_id:7,name:"Tank animation".into(),pack:"Vehicles".into(),unreal:true,download_pack:false});
            app.pending_activation=app.loading_asset.clone();
            let (sender,messages)=crossbeam_channel::unbounded();
            let cancel=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            app.transfer=Some(asset_transfer::Transfer{messages,cancel:cancel.clone()});
            let mut background_clicks=0;
            let background=std::cell::Cell::new(egui::Rect::NOTHING);
            let mut frame=|app:&mut AssetViewer,events:Vec<egui::Event>|{
                ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,650.))),events,..Default::default()},|ui|{
                    let response=ui.button("Background action");background.set(response.rect);
                    if response.clicked(){background_clicks+=1;}
                    app.poll_transfer(&ctx);app.loading_dialog(&ctx);
                })
            };
            frame(&mut app,vec![]);let output=frame(&mut app,vec![]);
            assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text=="Loading asset…")));
            let backdrop=egui::pos2(10.,620.);
            frame(&mut app,click(background.get().center()));
            frame(&mut app,click(backdrop));
            frame(&mut app,vec![egui::Event::Key{key:egui::Key::Escape,physical_key:None,pressed:true,repeat:false,modifiers:egui::Modifiers::NONE}]);
            assert!(app.is_loading());assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
            sender.send(asset_transfer::Message::Event(serde_json::json!({"event":"progress","stage":"Downloading","completed":1,"total":4,"detail":"Tank"}))).unwrap();
            frame(&mut app,vec![]);assert_eq!(app.transfer_progress,Some(0.25));
            let rect=app.loading_cancel_rect;
            assert!(rect.min.x>=0. && rect.max.x<=width);
            frame(&mut app,click(rect.center()));
            assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));assert!(app.is_loading());
            assert!(app.pending_activation.is_none());
            let output=frame(&mut app,vec![]);
            assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text=="Cancelling asset loading…")));
            sender.send(asset_transfer::Message::Done(Err("Operation cancelled".into()))).unwrap();
            frame(&mut app,vec![]);assert!(!app.is_loading());assert!(app.error.contains("cancelled"));
            drop(frame);assert_eq!(background_clicks,0);
        }}
    }
    #[test]
    fn stopped_worker_cannot_leave_loading_dialog_stuck() {
        let mut app=AssetViewer::empty(String::new());
        app.start_open(OpenRequest{asset_id:7,name:"Asset".into(),pack:"Pack".into(),unreal:true,download_pack:false});
        let (sender,messages)=crossbeam_channel::unbounded();
        app.transfer=Some(asset_transfer::Transfer{messages,cancel:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))});
        drop(sender);app.poll_transfer(&egui::Context::default());
        assert!(!app.is_loading());assert!(app.transfer.is_none());assert!(app.error.contains("stopped"));
    }
    #[test]
    fn transfer_events_update_status_and_completion_without_blocking() {
        let mut app=AssetViewer::empty(String::new());
        app.storage_asset=Some(7);
        let (sender,messages)=crossbeam_channel::unbounded();
        app.transfer=Some(asset_transfer::Transfer{messages,cancel:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))});
        sender.send(asset_transfer::Message::Event(serde_json::json!({"event":"progress","stage":"Retrieving files","completed":2,"total":4,"detail":"Mesh.uasset"}))).unwrap();
        app.poll_transfer(&egui::Context::default());
        assert_eq!(app.transfer_progress,Some(0.5));
        assert!(app.transfer_message.contains("Mesh.uasset"));
        sender.send(asset_transfer::Message::Event(serde_json::json!({"event":"availability","asset_id":7,"archived":true,"local":false,"locations":["archive"]}))).unwrap();
        sender.send(asset_transfer::Message::Event(serde_json::json!({"event":"result","files":4,"cache":"D:/cache"}))).unwrap();
        sender.send(asset_transfer::Message::Done(Ok(()))).unwrap();
        app.poll_transfer(&egui::Context::default());
        assert!(app.transfer.is_none());
        assert_eq!(app.transfer_progress,Some(1.0));
        assert!(app.storage_status.contains("archive"));
        assert!(app.transfer_message.contains("D:/cache"));
    }
    #[test]
    fn separate_asset_database_loads_and_decodes_a_real_preview() {
        let Ok(dsn)=std::env::var("ASSET_CLI_TEST_DSN") else {return;};
        let mut app=AssetViewer::new(dsn);
        wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert!(!app.assets.is_empty());
        let preview=app.assets.iter().find_map(|a|a.thumbnail.as_ref()).expect("no asset thumbnails");
        assert!(preview.size[0]>0 && preview.size[1]>0);
        for dark in [true,false] {for width in [900.0,1600.0] {
            let ctx=egui::Context::default();
            ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            app.textures.clear();app.texture_uploads=0;
            let start=Instant::now();
            let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,850.0))),..Default::default()},|ui|{
                egui::Panel::top("filters").show(ui,|ui|{app.type_filter(ui);});
                egui::Panel::left("packs").default_size(240.0).show(ui,|ui|app.pack_panel(ui));
                egui::Panel::right("details").default_size(280.0).show(ui,|ui|app.details_panel(ui));
                app.grid_panel(ui);
            });
            assert!(!output.shapes.is_empty());
            assert!(app.texture_uploads<=4);
            assert!(app.textures.len()<app.assets.len(),"offscreen grid thumbnails should not be uploaded");
            let first=start.elapsed();
            let warm=Instant::now();
            for _ in 0..3 {
                app.texture_uploads=0;
                let _=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,850.0))),..Default::default()},|ui|{
                    egui::Panel::top("filters").show(ui,|ui|{app.type_filter(ui);});
                    egui::Panel::left("packs").default_size(240.0).show(ui,|ui|app.pack_panel(ui));
                    egui::Panel::right("details").default_size(280.0).show(ui,|ui|app.details_panel(ui));
                    app.grid_panel(ui);
                });
            }
            eprintln!("Real asset grid: dark={dark}, width={width}, first={first:?}, warm average={:?}",warm.elapsed()/3);
        }}
    }
    #[test]
    fn indexed_filter_counts_and_pages_only_indexed_assets() {
        let Ok(dsn)=std::env::var("ASSET_CLI_TEST_DSN") else {return;};
        let mut app=AssetViewer::new(dsn);
        wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        let all_total=app.total;
        let expected:i64=app.connect().unwrap().query_one(
            "SELECT count(*) FROM assets WHERE scan_status='indexed'",&[]).unwrap().get(0);
        app.indexed_only=true;
        app.offset=i64::MAX;
        app.refresh();
        wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert_eq!(app.total,expected);
        assert_eq!(app.offset,if expected==0{0}else{((expected-1)/200)*200});
        assert_eq!(app.assets.len() as i64,(expected-app.offset).min(200));
        assert!(app.assets.iter().all(|a|a.status=="indexed"));
        app.selected_classes=vec!["__asset_cli_nonexistent_class__".into()];
        app.refresh();
        wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert_eq!(app.total,0);
        assert!(app.assets.is_empty());
        app.selected_classes.clear();app.indexed_only=false;app.offset=0;app.refresh();
        wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert_eq!(app.total,all_total);
    }
    #[test]
    fn local_filter_checks_real_files_and_pages_with_other_filters() {
        let Ok(dsn)=std::env::var("ASSET_CLI_TEST_DSN") else {return;};
        let mut db=Client::connect(&dsn,NoTls).unwrap();
        // Session-local tables leave the user's catalog and sequences untouched.
        for table in ["packs","assets","archive_files","asset_file_cache"] {
            db.batch_execute(&format!("CREATE TEMP TABLE {table} (LIKE public.{table} INCLUDING DEFAULTS)")).unwrap();
        }
        let root=std::env::temp_dir().join(format!("asset-local-search-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file=root.join("fixture.uasset");std::fs::write(&file,b"asset").unwrap();
        let path=file.to_string_lossy().to_string();
        db.batch_execute("INSERT INTO packs(id,title,source,archive_revision) VALUES('test','Fixture','fab','current');").unwrap();
        db.execute("INSERT INTO assets(id,pack_id,object_path,name,class_name,kind,source_path,size_bytes,scan_status,search_text) SELECT n,'test','file:'||n,'Fixture '||n,'StaticMesh','unreal',$1,5,'indexed','fixture' FROM generate_series(1,205) n",&[&path]).unwrap();
        db.batch_execute("INSERT INTO assets(id,pack_id,object_path,name,class_name,kind,metadata) VALUES
            (206,'test','file:cache','Cached','Texture2D','unreal','{\"relative_path\":\"cache.uasset\"}'),
            (207,'test','file:stale','Stale','Texture2D','unreal','{\"relative_path\":\"stale.uasset\"}'),
            (208,'test','file:missing','Missing','StaticMesh','unreal','{}');
            INSERT INTO archive_files(revision_id,path,size_bytes,sha256,location,stored_path) VALUES
            ('current','cache.uasset',5,'valid','__test_remote__','cache.uasset'),
            ('current','stale.uasset',5,'new','__test_remote__','stale.uasset');").unwrap();
        db.execute("INSERT INTO asset_file_cache(revision_id,path,local_path,sha256) VALUES ('current','cache.uasset',$1,'valid'),('current','stale.uasset',$1,'old')",&[&path]).unwrap();
        let local=asset_local::load(&mut db).unwrap();
        assert_eq!(local.ids.len(),206);
        assert!(local.packs.contains("test"));
        assert!(!local.ids.contains(&207));assert!(!local.ids.contains(&208));
        let latest=std::sync::atomic::AtomicU64::new(0);
        let mut request=asset_search::Request{local_only:true,offset:i64::MAX,..Default::default()};
        let result=asset_search::load(&mut db,&request,Arc::new(vec![]),Arc::new(vec![]),&local,&latest).unwrap();
        assert_eq!(result.total,206);assert_eq!(result.offset,200);assert_eq!(result.assets.len(),6);
        assert!(result.assets.iter().all(|a|a.local));
        request.classes=vec!["StaticMesh".into()];request.indexed_only=true;request.pack="test".into();request.query="fixture".into();
        // The pack query also loads its file list.
        db.batch_execute("CREATE TEMP TABLE pack_files (LIKE public.pack_files INCLUDING DEFAULTS)").unwrap();
        let result=asset_search::load(&mut db,&request,Arc::new(vec![]),Arc::new(vec![]),&local,&latest).unwrap();
        assert_eq!(result.total,205);assert_eq!(result.assets.len(),5);
        std::fs::remove_file(file).unwrap();
        assert!(asset_local::load(&mut db).unwrap().ids.is_empty());
        std::fs::remove_dir(root).unwrap();
        // SSH archives count from the current published revision without touching
        // an unavailable host; obsolete revisions and online-only assets do not.
        db.batch_execute("INSERT INTO packs(id,title,source,archive_revision) VALUES('lan','LAN Fixture','fab','lan-current');
            INSERT INTO packs(id,title,source,archive_revision) VALUES('unscanned','Unscanned LAN','fab','unscanned-current');
            INSERT INTO assets(id,pack_id,object_path,name,class_name,kind,scan_status,search_text,metadata) VALUES
            (209,'lan','file:lan','Fixture LAN','StaticMesh','unreal','indexed','fixture','{\"relative_path\":\"Content/lan.uasset\"}'),
            (210,'lan','file:old','Fixture old','StaticMesh','unreal','indexed','fixture','{\"relative_path\":\"Content/old.uasset\"}'),
            (211,'lan','file:online','Fixture online','StaticMesh','unreal','indexed','fixture','{}');
            INSERT INTO archive_files(revision_id,path,size_bytes,sha256,location,stored_path) VALUES
            ('lan-current','Content/lan.uasset',5,'valid','__test_lan__','packs/fixture/Content/lan.uasset'),
            ('unscanned-current','Content/new.uasset',5,'new','__test_lan__','packs/unscanned/Content/new.uasset'),
            ('lan-old','Content/old.uasset',5,'old','__test_lan__','packs/old/Content/old.uasset');").unwrap();
        let config=serde_json::json!({"locations":{"__test_lan__":{"kind":"sftp","alias":"unavailable-test-host","remote":"D:/assets"}}});
        let stored=asset_local::load_with_config(&mut db,&config).unwrap();
        assert_eq!(stored.ids,vec![209]);assert!(stored.packs.contains("lan"));assert!(stored.packs.contains("unscanned"));
        request.pack="lan".into();request.offset=0;
        let result=asset_search::load(&mut db,&request,Arc::new(vec![]),Arc::new(vec![]),&stored,&latest).unwrap();
        assert_eq!(result.total,1);assert_eq!(result.assets[0].id,209);assert!(result.assets[0].local);
        assert!(asset_local::load_with_config(&mut db,&serde_json::Value::Null).unwrap().ids.is_empty());
    }
    #[test]
    fn configured_lan_archive_appears_in_stored_search_results() {
        let Ok(pack)=std::env::var("ASSET_CLI_TEST_STORED_PACK") else {return;};
        let dsn=std::env::var("ASSET_CLI_TEST_DSN").unwrap();
        let mut app=AssetViewer::new(dsn);
        app.selected_pack=pack.clone();app.local_only=true;app.refresh();wait_search(&mut app);
        assert!(app.error.is_empty(),"{}",app.error);
        assert!(app.total>0);assert!(app.assets.iter().all(|a|a.local&&a.pack_id==pack));
        assert!(app.packs.iter().any(|p|p.id==pack&&p.local));
    }
    #[test]
    fn local_filter_and_badge_render_and_toggle_in_both_themes() {
        for dark in [true,false] {for width in [280.,650.] {
            let mut app=AssetViewer::empty(String::new());
            let ctx=egui::Context::default();
            ctx.set_visuals(if dark{egui::Visuals::dark()}else{egui::Visuals::light()});
            let mut rect=egui::Rect::NOTHING;
            let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,400.))),..Default::default()},|ui|{
                rect=app.local_checkbox(ui).rect;local_badge(ui,"Locally stored");
            });
            assert!(!output.shapes.is_empty());
            let pos=rect.center();
            let _=ctx.run_ui(egui::RawInput{events:vec![egui::Event::PointerMoved(pos),
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE},
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE}],..Default::default()},|ui|{app.local_checkbox(ui);});
            assert!(app.local_only);
        }}
    }
    #[test]
    fn indexed_checkbox_toggles_with_pointer_input() {
        let Ok(dsn)=std::env::var("ASSET_CLI_TEST_DSN") else {return;};
        let mut app=AssetViewer::new(dsn);
        let ctx=egui::Context::default();
        let mut rect=egui::Rect::NOTHING;
        let _=ctx.run_ui(egui::RawInput::default(),|ui|{
            rect=app.indexed_checkbox(ui).rect;
        });
        for expected in [true,false] {
            let pos=rect.center();
            let input=egui::RawInput{events:vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:egui::Modifiers::NONE},
                egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:egui::Modifiers::NONE},
            ],..Default::default()};
            let mut changed=false;
            let _=ctx.run_ui(input,|ui|{
                changed=app.indexed_checkbox(ui).changed();
            });
            assert!(changed);
            assert_eq!(app.indexed_only,expected);
        }
    }
}
