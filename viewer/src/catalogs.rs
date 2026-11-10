use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde_json::Value;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

pub fn cli(args: &[String], catalog: Option<&str>) -> Result<Value, String> {
    let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.venv/Scripts/python.exe");
    let python =
        std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_| fallback.to_string_lossy().into());
    let mut command = Command::new(python);
    command.args(["-m", "image_atlas.cli", "--json"]);
    if let Some(id) = catalog.filter(|s| !s.is_empty()) {
        command.args(["--database", id]);
    }
    let mut private_input=None;
    let mut index=0;
    while index<args.len(){
        if args[index]=="--dsn" {
            let secret=args.get(index+1).ok_or("Missing registration connection")?;
            private_input=Some(format!("{secret}\n").into_bytes());command.arg("--dsn-stdin");index+=2;
        }else{command.arg(&args[index]);index+=1;}
    }
    if let Ok(root) = std::env::var("IMAGE_CLI_MODULE_ROOT") {
        command.env("PYTHONPATH", root);
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let timeout=if args.first().map(String::as_str)==Some("db") && args.iter().any(|a|matches!(a.as_str(),"backup"|"restore")) {3600} else {60};
    let result = if let Some(input)=private_input {crate::bridge::output_with_input(&mut command,std::time::Duration::from_secs(timeout),32*1024*1024,input)?}
        else {crate::bridge::output(&mut command,std::time::Duration::from_secs(timeout),32*1024*1024)?};
    if !result.status.success() && result.status.code() != Some(130) {
        return Err(String::from_utf8_lossy(&result.stderr).into());
    }
    serde_json::from_slice(&result.stdout).map_err(|e| e.to_string())
}

pub fn allowed(path: &Path, catalog: Option<&str>) -> Result<(), String> {
    let Some(id) = catalog.filter(|s| !s.is_empty()) else {
        return Err("Register the catalog and assign folders before opening originals".into());
    };
    let config = std::env::var("IMAGE_CLI_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(&std::env::var("LOCALAPPDATA").unwrap_or_default()).join("image-cli")
        });
    let registry: Value = serde_json::from_slice(
        &std::fs::read(config.join("catalogs.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let record = registry["catalogs"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"].as_str() == Some(id)))
        .ok_or("Catalog no longer registered")?;
    let key = |p: &Path| -> Result<String, String> {
        Ok(p.canonicalize()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .trim_start_matches("\\\\?\\")
            .to_lowercase())
    };
    let candidate = key(path)?;
    for root in record["roots"].as_array().ok_or("No allowed folders")? {
        let stored = root.as_str().unwrap_or("");
        let Ok(root) = key(Path::new(stored)) else {
            continue;
        };
        if root.trim_start_matches("\\\\?\\") != stored.to_lowercase().trim_start_matches("\\\\?\\")
        {
            continue;
        }
        if candidate == root
            || candidate.starts_with(
                &(root.trim_end_matches(['\\', '/']).to_owned() + std::path::MAIN_SEPARATOR_STR),
            )
        {
            return Ok(());
        }
    }
    Err("Original is outside this catalog's allowed folders".into())
}

pub fn readonly(catalog: Option<&str>) -> Result<bool, String> {
    let Some(id) = catalog.filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let config = std::env::var("IMAGE_CLI_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(&std::env::var("LOCALAPPDATA").unwrap_or_default()).join("image-cli")
        });
    let data: Value = serde_json::from_slice(
        &std::fs::read(config.join("catalogs.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let record = data["catalogs"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"].as_str() == Some(id)))
        .ok_or("Catalog is no longer registered")?;
    Ok(record["readonly"].as_bool().unwrap_or(false))
}

pub struct Panel {
    pub context: Value,
    rows: Vec<Value>,
    open: bool,
    name: String,
    roots: String,
    new_roots:String,
    new_catalog:bool,
    job_id:String,
    jobs:Vec<Value>,
    dsn: String,
    path: String,
    status: String,
    busy: bool,
    pending: usize,
    transfer: bool,
    last_poll: std::time::Instant,
    tx: Sender<(String, Result<Value, String>)>,
    rx: Receiver<(String, Result<Value, String>)>,
}
impl Panel {
    pub fn new(context: Value,ctx:&egui::Context) -> Self {
        #[cfg(test)] if context["dsn"].as_str()==Some(""){return Self::with_rows(context,Vec::new());}
        let initial=context["dsn"].as_str().is_none_or(str::is_empty);
        let mut panel=Self::with_rows(context,Vec::new());
        panel.run("list",vec!["db".into(),"list".into()],None,ctx);
        if initial {panel.run("switch",vec!["db".into(),"context".into()],None,ctx);}
        panel
    }
    #[cfg(debug_assertions)]
    pub fn open_manager(&mut self){self.open=true;}
    #[cfg(debug_assertions)]
    pub fn ready(&self)->bool{!self.busy}
    fn with_rows(context: Value, rows: Vec<Value>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let roots = context["roots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Self {
            context,
            rows,
            open: false,
            name: String::new(),
            roots,
            new_roots:String::new(),new_catalog:false,job_id:String::new(),jobs:Vec::new(),
            dsn: String::new(),
            path: String::new(),
            status: String::new(),
            busy: false,
            pending: 0,
            transfer: false,
            last_poll: std::time::Instant::now(),
            tx,
            rx,
        }
    }
    fn run(&mut self, tag: &str, args: Vec<String>, id: Option<String>, ctx: &egui::Context) {
        if matches!(tag, "backup" | "restore") {
            self.transfer = true;
            self.status = "Working…".into();
        }
        self.pending += 1;
        self.busy = true;
        let tx = self.tx.clone();
        let tag = tag.to_owned();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let value = cli(&args, id.as_deref());
            let _ = tx.send((tag, value));
            ctx.request_repaint();
        });
    }
    #[cfg(test)]
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<Value> {
        self.ui_with_selector(ui,true)
    }
    pub fn ui_with_selector(&mut self, ui: &mut egui::Ui,show_selector:bool) -> Option<Value> {
        let ctx = ui.ctx().clone();
        let mut switched = None;
        while let Ok((tag, result)) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            self.busy = self.pending > 0;
            if matches!(tag.as_str(), "backup" | "restore") {
                self.transfer = false;
            }
            match result {
                Err(e) => self.status = e,
                Ok(v) => {
                    if tag == "list" {
                        self.rows = v["catalogs"].as_array().cloned().unwrap_or_default();
                    } else if tag == "jobs" {
                        self.jobs=v.as_array().cloned().unwrap_or_default();
                        self.status=if self.jobs.is_empty(){"No active jobs".into()}else{format!("{} active jobs",self.jobs.len())};
                    } else if tag == "switch" {
                        switched = Some(v);
                    } else {
                        self.status = crate::bridge::readable(&v);
                        self.dsn.clear();
                        if tag == "roots" {
                            let id = self.context["id"].as_str().map(str::to_owned);
                            self.run("switch", vec!["db".into(), "context".into()], id, &ctx);
                        }
                        if matches!(tag.as_str(), "create" | "register" | "restore") {
                            self.run("list", vec!["db".into(), "list".into()], None, &ctx);
                        }
                    }
                }
            }
        }
        if self.transfer {
            if self.last_poll.elapsed() > std::time::Duration::from_secs(1) && self.pending < 2 {
                self.last_poll = std::time::Instant::now();
                let id = self.context["id"].as_str().map(str::to_owned);
                self.run("jobs", vec!["jobs".into()], id, &ctx);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        ui.horizontal_wrapped(|ui|{
            if show_selector {ui.add_enabled_ui(!self.busy&&!self.transfer,|ui|{egui::ComboBox::from_id_salt("catalog-selector").selected_text(self.context["name"].as_str().unwrap_or("Current")).show_ui(ui,|ui|{
                let mut selected=None;
                for row in &self.rows {if ui.selectable_label(row["id"]==self.context["id"],row["name"].as_str().unwrap_or("")).clicked(){selected=row["id"].as_str().map(str::to_owned);}}
                if let Some(id)=selected {self.run("switch",vec!["db".into(),"context".into()],Some(id),&ctx);}
            }).response.on_hover_text("Database for this window; switching clears all image and face caches");});}
            if ui.button("Databases").on_hover_text("Create, register, edit folders, set CLI default, inspect, back up, and restore").clicked(){self.open=!self.open;self.run("list",vec!["db".into(),"list".into()],None,&ctx);}
            if ui.add_enabled(!self.busy&&!self.transfer,egui::Button::new("Reconnect")).on_hover_text("Reconnect this database after an outage; retain the current search").clicked(){
                let id=self.context["id"].as_str().map(str::to_owned);self.run("switch",vec!["db".into(),"context".into()],id,&ctx);
            }
            if self.context["readonly"].as_bool()==Some(true){ui.label("Read-only").on_hover_text("Assign allowed folders before editing or opening originals");}
        });
        if self.context["dsn"].as_str().is_none_or(str::is_empty){
            if !self.status.is_empty(){ui.colored_label(egui::Color32::LIGHT_RED,&self.status);}
            if ui.add_enabled(!self.busy,egui::Button::new("Retry catalog connection")).on_hover_text("Resolve the configured catalog again after correcting its registry or credentials").clicked(){self.run("switch",vec!["db".into(),"context".into()],None,&ctx);}
        }
        if self.open {
            let mut open=self.open;
            egui::Window::new("Database management").open(&mut open).default_width(600.).show(&ctx,|ui|{
            egui::ScrollArea::vertical().max_height(ctx.content_rect().height()*0.75).show(ui,|ui|{
            ui.checkbox(&mut self.new_catalog,"Create/register another database").on_hover_text("Use a separate name and folders; existing catalogs retain their owned folders.");
            ui.label("Name");ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(ui.available_width())).on_hover_text("Friendly name for a new or restored database");
            ui.label("Connection for registration (optional)");ui.add(egui::TextEdit::singleline(&mut self.dsn).password(true).desired_width(ui.available_width())).on_hover_text("Optional PostgreSQL registration connection; sent privately through stdin");
            ui.label("Allowed folders, one per line");egui::ScrollArea::vertical().id_salt("catalog-roots").max_height(110.0).show(ui,|ui|{
                let roots=if self.new_catalog {&mut self.new_roots}else{&mut self.roots};
                ui.add(egui::TextEdit::multiline(roots).desired_rows(4).desired_width(ui.available_width())).on_hover_text("One absolute folder per line. Folders cannot overlap another writable catalog.");
            });
            ui.label("Backup folder or restore archive path");ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(ui.available_width())).on_hover_text("Backup destination folder or a .dump archive to restore into a new database");
            ui.horizontal_wrapped(|ui|{
                for (action,label) in [("create","Create"),("register","Register existing"),("roots","Save folders"),("use","Set CLI default"),("check","Health"),("storage","Storage"),("backup","Back up"),("restore","Restore as new"),("jobs","Jobs"),("cancel","Cancel job")]{
                    if ui.add_enabled((!self.busy||matches!(action,"jobs"|"cancel")) && (!matches!(action,"create"|"register")||self.new_catalog) && (action!="cancel"||!self.job_id.trim().is_empty()),egui::Button::new(label)).on_hover_text(match action{"restore"=>"Create a separate read-only catalog; original remains intact","cancel"=>"Cancel the selected job","roots"=>"No overlapping folders; blocked during active jobs","create"=>"Create another database with its own folders","register"=>"Register an existing database and keep its contents","use"=>"Use this database for future CLI commands","check"=>"Verify identity, schema and image counts","storage"=>"Inspect tables, indexes and thumbnail storage","backup"=>"Create a consistent archive without original images","jobs"=>"List active jobs and select one for cancellation",_=>label}).clicked(){
                        let id=self.context["id"].as_str().map(str::to_owned);
                        let mut args=if matches!(action,"jobs"|"cancel"){vec![action.into()]}else{vec!["db".into(),action.into()]};
                        match action {
                            "create"|"register"=>{args.push(self.name.clone());if action=="register"&&!self.dsn.is_empty(){args.extend(["--dsn".into(),self.dsn.clone()]);}
                                for root in self.new_roots.lines().map(str::trim).filter(|r|!r.is_empty()){args.extend(["--root".into(),root.into()]);}},
                            "roots"=>{args.push(id.clone().unwrap_or_default());if self.roots.trim().is_empty(){args.push("--clear".into());}else{for root in self.roots.lines().map(str::trim).filter(|r|!r.is_empty()){args.extend(["--root".into(),root.into()]);}}},
                            "use"=>args.push(id.clone().unwrap_or_default()),
                            "backup"=>args.push(self.path.clone()),
                            "restore"=>args.extend([self.path.clone(),self.name.clone()]),
                            "cancel"=>args.push(self.job_id.clone()),_=>{}
                        }
                        self.run(action,args,id,&ctx);
                    }
                }
            });
            ui.label("Job ID");ui.add(egui::TextEdit::singleline(&mut self.job_id).desired_width(ui.available_width())).on_hover_text("Select a job below, or enter its ID to cancel it.");
            for job in self.jobs.clone() {let id=job["id"].as_str().unwrap_or_default();if ui.selectable_label(self.job_id==id,crate::bridge::readable(&job)).on_hover_text("Select this job for cancellation").clicked(){self.job_id=id.into();}}
            if ui.add_enabled(!self.busy,egui::Button::new("Refresh catalogs")).on_hover_text("Reload registered databases without switching this window").clicked(){self.run("list",vec!["db".into(),"list".into()],None,&ctx);}
            ui.label(&self.status);
            });
        });
            self.open=open;
        }
        switched
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_catalog_selection_and_connections_are_isolated() {
        let Ok(ids) = std::env::var("IMAGE_CLI_NATIVE_CATALOG_TEST_IDS") else {
            return;
        };
        let ids: Vec<String> = serde_json::from_str(&ids).unwrap();
        let contexts: Vec<Value> = ids
            .iter()
            .map(|id| cli(&["db".into(), "context".into()], Some(id)).unwrap())
            .collect();
        let ctx = egui::Context::default();
        ctx.style_mut_of(egui::Theme::Dark, |style| style.animation_time = 0.0);
        ctx.style_mut_of(egui::Theme::Light, |style| style.animation_time = 0.0);
        let mut panel = Panel::with_rows(contexts[0].clone(), contexts.clone());
        let frame = |panel: &mut Panel, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    panel.ui(ui);
                },
            )
        };
        let position = |output: &egui::FullOutput, label: &str| {
            output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) if t.galley.job.text == label => {
                        Some(t.pos + t.galley.size() / 2.)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("Missing selection label {label}"))
        };
        frame(&mut panel, vec![]);
        let output = frame(&mut panel, vec![]);
        let pos = position(&output, contexts[0]["name"].as_str().unwrap());
        frame(
            &mut panel,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        let output = frame(
            &mut panel,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        let _ = output;
        let output = frame(&mut panel, vec![]);
        let pos = position(&output, contexts[1]["name"].as_str().unwrap());
        frame(
            &mut panel,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            &mut panel,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut switched = None;
        while switched.is_none() && std::time::Instant::now() < deadline {
            let _ = ctx.run_ui(Default::default(), |ui| {
                switched = panel.ui(ui);
            });
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(switched.unwrap()["id"], ids[1]);
        let active = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut paths = vec![];
        for context in &contexts {
            let (tx, rx) = crate::db::start_catalog(
                context["dsn"].as_str().unwrap().into(),
                ctx.clone(),
                active.clone(),
                context["id"].as_str().map(str::to_owned),
                false,
            );
            tx.send(crate::db::Request::LookupImage(1)).unwrap();
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                crate::db::Event::OpenPhoto(photo) => paths.push(photo.path),
                _ => panic!("No pinned image"),
            }
        }
        assert_ne!(paths[0], paths[1]);
        let (model_tx, _model_requests) = crossbeam_channel::unbounded();
        let (model_events, model_rx) = crossbeam_channel::unbounded();
        let mut viewer = crate::Viewer::build_catalog(
            ctx.clone(),
            contexts[0].clone(),
            Some((model_tx, model_rx, active.clone(), true, None)),
        );
        viewer.selected = Some(1);
        viewer.pending_full.insert(1);
        viewer.cached_vector = Some(("old search".into(), vec![1.0; 512]));
        model_events
            .send(crate::model::Event::Vector {
                generation: viewer.generation,
                vector: vec![1.0; 512],
            })
            .unwrap();
        viewer.switch_catalog(ctx.clone(), contexts[1].clone());
        assert!(
            viewer.selected.is_none()
                && viewer.cached_vector.is_none()
                && viewer.pending_full.is_empty()
                && viewer.thumbnails.is_empty()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while viewer.photos.is_empty() && std::time::Instant::now() < deadline {
            viewer.receive(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(viewer.photos[0].path, paths[1]);
        assert!(viewer.cached_vector.is_none() && viewer.model_ready);
        drop(viewer);
        let (tx, rx) = crate::db::start_catalog(
            contexts[0]["dsn"].as_str().unwrap().into(),
            ctx.clone(),
            active.clone(),
            Some(ids[1].clone()),
            false,
        );
        tx.send(crate::db::Request::LookupImage(1)).unwrap();
        assert!(
            matches!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),crate::db::Event::Message(error) if error.contains("identity mismatch"))
        );
        let (tx, rx) = crate::db::start_catalog(
            contexts[1]["dsn"].as_str().unwrap().into(),
            ctx,
            active,
            Some(ids[1].clone()),
            true,
        );
        tx.send(crate::db::Request::SetFavorite {
            id: 1,
            favorite: true,
        })
        .unwrap();
        assert!(
            matches!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),crate::db::Event::Message(error) if error.contains("Database error"))
        );
    }
    #[test]
    fn catalog_manager_renders_in_both_themes_and_delivers_switch_context() {
        for width in [640., 1200.] {
            for theme in [egui::Theme::Light, egui::Theme::Dark] {
                let ctx = egui::Context::default();
                ctx.set_theme(theme);
                ctx.style_mut_of(theme, |style| style.animation_time = 0.0);
                let mut panel = Panel::with_rows(
                    serde_json::json!({"id":"alpha","name":"Alpha","roots":["D:\\Photos"],"readonly":false}),
                    vec![],
                );
                panel.open = true;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 800.),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        panel.ui(ui);
                    },
                );
                output.append(ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 800.),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        panel.ui(ui);
                    },
                ));
                for label in [
                    "Database management",
                    "Create",
                    "Register existing",
                    "Save folders",
                    "Health",
                    "Storage",
                    "Back up",
                    "Restore as new",
                    "Jobs",
                    "Cancel job",
                ] {
                    assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text==label)),"Missing {label} at {width}");
                }
                if let Ok(folder) = std::env::var("IMAGE_CLI_CATALOG_RENDER_DIR") {
                    let tid = |id: egui::TextureId| match id {
                        egui::TextureId::Managed(n) | egui::TextureId::User(n) => n,
                    };
                    let textures:Vec<_>=output.textures_delta.set.iter().map(|(id,delta)| {let egui::ImageData::Color(image)=&delta.image;serde_json::json!({"id":tid(*id),"size":image.size,"pixels":image.pixels.iter().map(|p|p.to_array()).collect::<Vec<_>>()})}).collect();
                    let meshes:Vec<_>=ctx.tessellate(output.shapes,output.pixels_per_point).into_iter().filter_map(|item|if let egui::epaint::Primitive::Mesh(mesh)=item.primitive {Some(serde_json::json!({"texture":tid(mesh.texture_id),"clip":[item.clip_rect.min.x,item.clip_rect.min.y,item.clip_rect.max.x,item.clip_rect.max.y],"indices":mesh.indices,"vertices":mesh.vertices.iter().map(|v|{let c=v.color.to_array();[v.pos.x,v.pos.y,v.uv.x,v.uv.y,c[0] as f32/255.,c[1] as f32/255.,c[2] as f32/255.,c[3] as f32/255.]}).collect::<Vec<_>>()}))}else{None}).collect();
                    std::fs::write(Path::new(&folder).join(format!("native-catalogs-{width}-{theme:?}.json")),serde_json::to_vec(&serde_json::json!({"width":width,"height":800,"textures":textures,"meshes":meshes})).unwrap()).unwrap();
                }
                panel
                    .tx
                    .send((
                        "switch".into(),
                        Ok(serde_json::json!({"id":"beta","name":"Beta","dsn":"private"})),
                    ))
                    .unwrap();
                let mut result = None;
                let _ = ctx.run_ui(Default::default(), |ui| {
                    result = panel.ui(ui);
                });
                assert_eq!(result.unwrap()["id"], "beta");
            }
        }
    }
}
