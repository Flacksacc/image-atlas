mod db;
mod bridge;
mod people;
mod decode;
mod explore;
mod model;
mod connection;
mod visuals;
mod actions;
mod settings;
mod collections;
mod catalogs;
mod assets;
mod asset_metadata;
mod catalog_switcher;
mod database_details;
mod remote;

use chrono::{Datelike, Local, Months, NaiveDate};
use crossbeam_channel::{Receiver, Sender};
use db::{DayStat, FaceSample, FacetValue, FolderMonth, GeoBin, Group, Photo, Proposal, SavedView, ScatterBin};
use explore::{FilterChip, GroupBy, SortKey, SortSpec};
use serde::{Deserialize, Serialize};
use eframe::egui;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum ImageAction {
    BrowseFolder, SameDay, NearbyDays, Reveal, OpenFolder, OpenOriginal,
    OpenWith, OpenEditor(usize), CopyPath, Favorite, Rate(i16),
    Compare, Select, RetryPreview, Similar, ExactDuplicates, ExpandStack, NearDuplicates, SameCamera, SameBurst, SameEvent,
    SamePerson, SameSource, SimilarColor, SetReview(&'static str), AddTag, ProposeMove, Details,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ViewMode { Grid, Table, Filmstrip, Stacks }

impl Default for ViewMode { fn default() -> Self { Self::Grid } }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ExploreVisual { Folders, Gps, SizePixels, FolderMonths }

impl Default for ExploreVisual { fn default() -> Self { Self::Folders } }

#[derive(Serialize, Deserialize)]
struct SavedState {
    search: String,
    visual_source: Option<i64>,
    folder: Option<String>,
    from_day: Option<String>,
    to_day: Option<String>,
    relevance: f64,
    filters: Vec<FilterChip>,
    sort: SortSpec,
    show_missing: bool,
    #[serde(default)]
    view_mode: ViewMode,
    #[serde(default)]
    explore_visual: ExploreVisual,
    #[serde(default)]
    map_metric: visuals::Metric,
    #[serde(default)]
    show_calendar: bool,
    #[serde(default)]
    calendar_year: Option<i32>,
    #[serde(default)]
    calendar_month: Option<u32>,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Images,
    People,
    Proposals,
    Collections,
}

fn format_bytes(bytes: i64) -> String {
    let mut value = bytes.max(0) as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    unreachable!()
}

fn format_metric(metric: visuals::Metric, tile: &visuals::Tile) -> String {
    if metric == visuals::Metric::Count { format!("{} images", tile.count) }
    else { format_bytes(metric.value(tile)) }
}

fn sort_icon_button(ui: &mut egui::Ui, key: SortKey, label: &str, selected: bool) -> egui::Response {
    let font = egui::FontId::proportional(14.0);
    let color = ui.visuals().text_color();
    let galley = ui.painter().layout_no_wrap(label.into(),font.clone(),color);
    let (rect,response)=ui.allocate_exact_size(egui::vec2(galley.size().x+29.0,24.0),egui::Sense::click());
    let visuals=ui.style().interact_selectable(&response,selected);
    ui.painter().rect_filled(rect,3.0,visuals.bg_fill);
    let stroke=egui::Stroke::new(1.3,visuals.fg_stroke.color);
    let icon=egui::Rect::from_min_size(rect.min+egui::vec2(5.0,4.0),egui::vec2(16.0,16.0));
    let at=|x:f32,y:f32| icon.min+egui::vec2(x,y);
    let line=|a:egui::Pos2,b:egui::Pos2| { ui.painter().line_segment([a,b],stroke); };
    let outline=|r:egui::Rect| { ui.painter().rect_stroke(r,1.0,stroke,egui::StrokeKind::Inside); };
    match key {
        SortKey::Relevance | SortKey::Rating => {
            let points=(0..10).map(|n| { let angle=-std::f32::consts::FRAC_PI_2+n as f32*std::f32::consts::PI/5.0; let radius=if n%2==0 { 7.5 } else { 3.5 }; icon.center()+egui::vec2(angle.cos(),angle.sin())*radius }).collect();
            ui.painter().add(egui::Shape::closed_line(points,stroke));
        }
        SortKey::DateTaken => { outline(icon.shrink(1.0)); line(at(1.0,5.0),at(15.0,5.0)); for x in [5.0,10.0] { line(at(x,0.0),at(x,3.0)); } for y in [8.0,11.0] { for x in [4.0,8.0,12.0] { ui.painter().circle_filled(at(x,y),0.8,stroke.color); } } }
        SortKey::Filename => { ui.painter().text(icon.center(),egui::Align2::CENTER_CENTER,"A",egui::FontId::monospace(16.0),stroke.color); }
        SortKey::Bytes => { outline(icon.shrink(1.0)); outline(egui::Rect::from_min_max(at(4.0,2.0),at(11.0,6.0))); line(at(4.0,11.0),at(12.0,11.0)); }
        SortKey::Pixels => { outline(icon.shrink(1.0)); ui.painter().arrow(at(4.0,12.0),egui::vec2(8.0,-8.0),stroke); }
        SortKey::Folder => { ui.painter().add(egui::Shape::closed_line(vec![at(1.0,3.0),at(6.0,3.0),at(8.0,6.0),at(15.0,6.0),at(15.0,14.0),at(1.0,14.0)],stroke)); }
        SortKey::Camera => { outline(egui::Rect::from_min_max(at(1.0,4.0),at(15.0,14.0))); ui.painter().circle_stroke(at(8.0,9.0),3.0,stroke); line(at(5.0,2.0),at(11.0,2.0)); }
        SortKey::Faces => { ui.painter().circle_stroke(icon.center(),7.0,stroke); for x in [5.5,10.5] { ui.painter().circle_filled(at(x,6.0),1.0,stroke.color); } line(at(5.0,11.0),at(11.0,11.0)); }
        SortKey::Duplicates => { outline(egui::Rect::from_min_max(at(1.0,1.0),at(11.0,11.0))); outline(egui::Rect::from_min_max(at(5.0,5.0),at(15.0,15.0))); }
        SortKey::Random => { ui.painter().arrow(at(1.0,3.0),egui::vec2(14.0,10.0),stroke); ui.painter().arrow(at(1.0,13.0),egui::vec2(14.0,-10.0),stroke); }
        _ => { outline(icon.shrink(1.0)); }
    }
    ui.painter().galley(rect.min+egui::vec2(25.0,(24.0-galley.size().y)/2.0),galley,visuals.fg_stroke.color);
    response
}

fn sorting_toolbar(ui: &mut egui::Ui, sort: &mut SortSpec) -> bool {
    let previous = *sort;
    ui.horizontal_wrapped(|ui| {
        ui.strong("Sort images").on_hover_text("Sorting applies to the entire filtered result set, including pages not loaded yet.");
        for (key, label) in [
            (SortKey::Relevance, "Match"), (SortKey::DateTaken, "Date"),
            (SortKey::Filename, "Name"), (SortKey::Bytes, "Size"),
            (SortKey::Pixels, "Pixels"), (SortKey::Folder, "Folder"),
            (SortKey::Camera, "Camera"), (SortKey::Rating, "Rating"),
            (SortKey::Faces, "Faces"), (SortKey::Duplicates, "Copies"),
            (SortKey::Random, "Shuffle"),
        ] {
            if sort_icon_button(ui,key,label,sort.primary == key)
                .on_hover_text(format!("Sort by {}. Click the active icon again to reverse the order. {}", key.label(), key.tooltip())).clicked() {
                if sort.primary == key { sort.primary_desc = !sort.primary_desc; }
                else { sort.primary = key; sort.primary_desc = !matches!(key, SortKey::Filename | SortKey::Path | SortKey::Folder | SortKey::Camera | SortKey::Format); }
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Order");
        egui::ComboBox::from_id_salt("sort-primary").selected_text(sort.primary.label())
            .show_ui(ui, |ui| { for key in SortKey::ALL { ui.selectable_value(&mut sort.primary, key, key.label()).on_hover_text(key.tooltip()); } })
            .response.on_hover_text("Choose any sort field. The icon row provides shortcuts to common choices.");
        if ui.button(if sort.primary_desc { "Descending" } else { "Ascending" })
            .on_hover_text("Reverse the primary order: newest/oldest, largest/smallest, or Z–A/A–Z.").clicked() { sort.primary_desc = !sort.primary_desc; }
        ui.label("then").on_hover_text("The secondary field breaks ties within the primary field.");
        egui::ComboBox::from_id_salt("sort-secondary").selected_text(sort.secondary.label())
            .show_ui(ui, |ui| { for key in SortKey::ALL { ui.selectable_value(&mut sort.secondary, key, key.label()).on_hover_text(key.tooltip()); } })
            .response.on_hover_text("Choose a tie breaker. Image ID provides a final stable order.");
        if ui.small_button(if sort.secondary_desc { "Desc" } else { "Asc" })
            .on_hover_text("Reverse the secondary sort direction.").clicked() { sort.secondary_desc = !sort.secondary_desc; }
        ui.label("Group by");
        egui::ComboBox::from_id_salt("image-grouping").selected_text(sort.group_by.label())
            .show_ui(ui, |ui| { for group in GroupBy::ALL { ui.selectable_value(&mut sort.group_by, group, group.label()).on_hover_text(group.tooltip()); } })
            .response.on_hover_text(sort.group_by.tooltip());
        if ui.small_button("Reset").on_hover_text("Restore relevance/newest order with date as tie breaker and no grouping.").clicked() { *sort = SortSpec::default(); }
    });
    previous != *sort
}

fn photo_tooltip(photo: &Photo) -> String {
    format!("{}\n{} · {} × {} · {}\nDate: {} · Camera: {}\nRating: {} stars · Review: {}{}{}",
        photo.path, format_bytes(photo.size_bytes), photo.width, photo.height,
        photo.format.as_deref().unwrap_or("Unknown format"), photo.capture_day.as_deref().unwrap_or("Unknown"),
        photo.camera_model.as_deref().unwrap_or("Unknown"), photo.rating, photo.review_state,
        photo.image_group.as_ref().filter(|group| !group.is_empty()).map(|group| format!("\nGroup: {group}")).unwrap_or_default(),
        photo.collection_names.as_ref().map(|names|format!("\nCollections: {names}")).unwrap_or_default())
}

// Docking the preview can move the clicked tile before the second click arrives.
// Keep the first target so a double-click still opens that image after reflow.
fn double_clicked_image(ctx: &egui::Context, previous: &mut Option<(i64, egui::Pos2, f64)>) -> Option<i64> {
    let (id, position, time) = (*previous)?;
    let delay = ctx.options(|options| options.input_options.max_double_click_delay);
    if ctx.input(|input| input.time - time > delay) { *previous = None; return None; }
    let clicked = ctx.input(|input| input.pointer.button_double_clicked(egui::PointerButton::Primary)
        && input.pointer.interact_pos().is_some_and(|point| point.distance(position) <= 6.0));
    if clicked { *previous = None; Some(id) } else { None }
}

#[derive(Debug, PartialEq, Eq)]
struct GroupRow { label: String, start: usize, end: usize, heading: bool }

fn grouped_rows(photos: &[Photo], columns: usize) -> Vec<GroupRow> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start < photos.len() {
        let label = photos[start].image_group.as_deref().unwrap_or("Unknown");
        let end = start + photos[start..].iter().take_while(|p| p.image_group.as_deref().unwrap_or("Unknown") == label).count();
        for row_start in (start..end).step_by(columns.max(1)) {
            rows.push(GroupRow { label: label.into(), start: row_start, end: (row_start+columns.max(1)).min(end), heading: row_start == start });
        }
        start = end;
    }
    rows
}

struct CachedTexture {
    texture: egui::TextureHandle,
    used: u64,
}

pub struct Viewer {
    catalog_panel: catalogs::Panel,
    external_catalog_selector:bool,
    db_tx: Sender<db::Request>,
    db_rx: Receiver<db::Event>,
    decode_tx: Sender<decode::Job>,
    decode_rx: Receiver<decode::Decoded>,
    decode_cancel:Arc<std::sync::atomic::AtomicBool>,
    model_tx: Sender<model::Request>,
    model_rx: Receiver<model::Event>,
    photos: Vec<Photo>,
    folder_tree: visuals::FolderTree,
    map_tiles: Vec<visuals::Tile>,
    days: Vec<DayStat>,
    facet_values: Vec<FacetValue>,
    geo_bins: Vec<GeoBin>,
    scatter_bins: Vec<ScatterBin>,
    folder_months: Vec<FolderMonth>,
    explore_visual: ExploreVisual,
    chart_drag: Option<egui::Pos2>,
    total_results: i64,
    map_root: Option<String>,
    from_day: Option<String>,
    to_day: Option<String>,
    map_metric: visuals::Metric,
    timeline_drag: Option<usize>,
    show_calendar: bool,
    calendar_year: i32,
    calendar_month: u32,
    cached_vector: Option<(String, Vec<f32>)>,
    visual_source: Option<i64>,
    phase: u8,
    relevance: f64,
    filters: Vec<FilterChip>,
    filter_field: String,
    filter_value: String,
    filter_exclude: bool,
    sort: SortSpec,
    visual_complete: bool,
    page_loading: bool,
    refresh_span:i64,
    page_error: bool,
    facets_loading: bool,
    show_other: bool,
    thumbnails: HashMap<i64, CachedTexture>,
    full_images: HashMap<i64, CachedTexture>,
    document_artboards: HashMap<i64, u32>,
    face_images: HashMap<i64, CachedTexture>,
    pending_faces: HashSet<i64>,
    pending_thumbs: HashSet<i64>,
    missing_thumbs: HashSet<i64>,
    pending_full: HashSet<i64>,
    content_keys:HashMap<i64,Option<String>>,
    face_keys:HashMap<i64,Option<String>>,
    failed_full:HashMap<i64,String>,
    failed_faces:HashMap<i64,String>,
    last_displayed:Option<i64>,
    requested_details:Option<i64>,
    requested_open:Option<i64>,
    requested_people:Option<i64>,
    dialog_saving:bool,
    tag_saving:bool,
    group_detail:Option<i64>,
    group_faces:Vec<FaceSample>,
    selected_faces:HashSet<i64>,
    group_more:bool,
    group_loading:bool,
    groups_loading:bool,
    groups_error:bool,
    proposal_loading:bool,
    proposal_error:bool,
    identity_search:String,
    person_confirmation:Option<db::Request>,
    face_undo:Option<String>,
    source_revisions:HashMap<i64,Option<String>>,
    source_paths:HashMap<i64,String>,
    face_paths:HashMap<i64,String>,
    face_target:Option<i64>,
    people_manager:bool,
    person_edit:Option<i64>,
    person_name:String,
    person_merge:Option<i64>,
    saved_confirmation:Option<(String,Option<serde_json::Value>)>,
    sheet_destination:String,
    export_rx:Option<Receiver<Result<String,String>>>,
    proposal_status:String,
    proposal_search:String,
    proposal_more:bool,
    groups: Vec<Group>,
    proposals: Vec<Proposal>,
    collections_panel: collections::Panel,
    tab: Tab,
    search: String,
    search_changed: Instant,
    search_sent: bool,
    generation: u64,
    model_ready: bool,
    model_error: Option<String>,
    visual_pending: bool,
    model_queue_pending:bool,
    visual_results: Option<usize>,
    last_stats_request: Instant,
    show_missing: bool,
    selected: Option<usize>,
    last_image_click: Option<(i64, egui::Pos2, f64)>,
    view_mode: ViewMode,
    selected_ids: HashSet<i64>,
    contact_photos: Vec<Photo>,
    contact_sheet_open: bool,
    compare_ids: Vec<Photo>,
    preview_photo: Option<Photo>,
    zoom: f32,
    active_generation: Arc<AtomicU64>,
    tag_name: String,
    tag_group: Option<i64>,
    tag_person: Option<i64>,
    known_people: Vec<db::Person>,
    message: String,
    settings: settings::Settings,
    editor_entry: String,
    saved_views: Vec<SavedView>,
    save_view_name: String,
    dialog_photo: Option<Photo>,
    dialog_kind: Option<&'static str>,
    dialog_tag: String,
    dialog_destination: String,
    dialog_reason: String,
    details: Option<serde_json::Value>,
    person_choices: Vec<db::Person>,
    frame: u64,
    frame_durations: VecDeque<f32>,
    ui_p95_ms: f32,
    embedded: i64,
    indexed: i64,
    stats_known: bool,
    catalog_revision: String,
    #[cfg(debug_assertions)]
    screenshot_path: Option<PathBuf>,
    #[cfg(debug_assertions)]
    screenshot_requested: bool,
    #[cfg(debug_assertions)]
    screenshot_expected_folder: Option<String>,
}

type VisionState = (Sender<model::Request>, Receiver<model::Event>, Arc<AtomicU64>, bool, Option<String>);

fn install_fonts(ctx:&egui::Context) {
        #[cfg(windows)] {
            let folder=PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(||"C:\\Windows".into())).join("Fonts");
            for name in ["YuGothM.ttc","meiryo.ttc","msyh.ttc"] {if let Ok(bytes)=std::fs::read(folder.join(name)){
                let mut fonts=egui::FontDefinitions::default();fonts.font_data.insert("system-unicode".into(),Arc::new(egui::FontData::from_owned(bytes)));
                for family in [egui::FontFamily::Proportional,egui::FontFamily::Monospace]{fonts.families.entry(family).or_default().push("system-unicode".into());}ctx.set_fonts(fonts);break;
            }}
        }
}

pub fn run()->eframe::Result {
    if std::env::var("IMAGE_CLI_REMOTE").as_deref()==Ok("1"){return remote::run();}
    #[allow(unused_mut)]
    let mut viewport=egui::ViewportBuilder::default().with_inner_size([1200.0,800.0]);
    #[cfg(debug_assertions)] if std::env::var_os("IMAGE_CLI_VIEWER_SCREENSHOT").is_some(){viewport=viewport.with_active(false);}
    #[cfg(debug_assertions)] if let (Ok(width),Ok(height))=(std::env::var("IMAGE_CLI_QA_WIDTH"),std::env::var("IMAGE_CLI_QA_HEIGHT")){if let (Ok(width),Ok(height))=(width.parse::<f32>(),height.parse::<f32>()){viewport=viewport.with_inner_size([width,height]);}}
    eframe::run_native("ImageAtlas",eframe::NativeOptions{
        viewport,..Default::default()
    },Box::new(|cc|Ok(Box::new(catalog_switcher::Window::new(cc.egui_ctx.clone())))))
}

impl Viewer {
    pub fn new(ctx: egui::Context) -> Self {
        install_fonts(&ctx);
        let context=std::env::var("IMAGE_CLI_VIEWER_CONTEXT").ok().and_then(|value|serde_json::from_str::<serde_json::Value>(&value).ok())
            .map(|mut context|{context["dsn"]=serde_json::Value::String(std::env::var("IMAGE_CLI_DSN").unwrap_or_default());context})
            .unwrap_or_else(||serde_json::json!({"name":"Loading catalog…","dsn":""}));
        Self::with_catalog(ctx,context)
    }

    fn with_catalog(ctx:egui::Context,context:serde_json::Value)->Self {
        Self::build_catalog(ctx,context,None)
    }

    fn switch_catalog(&mut self,ctx:egui::Context,context:serde_json::Value) {
        let external_catalog_selector=self.external_catalog_selector;
        self.active_generation.fetch_add(1,Ordering::Relaxed);
        // The text encoder contains no catalog data; keep its GPU model loaded.
        let vision=(self.model_tx.clone(),self.model_rx.clone(),self.active_generation.clone(),self.model_ready,self.model_error.clone());
        let same_catalog=self.catalog_panel.context["id"]==context["id"];
        let state=if same_catalog{Some(self.saved_state())}else{None};
        *self=Self::build_catalog(ctx,context,Some(vision));
        self.external_catalog_selector=external_catalog_selector;
        if let Some(state)=state{self.restore_state(state);}
    }

    fn build_catalog(ctx:egui::Context,context:serde_json::Value,vision:Option<VisionState>)->Self {
        let dsn=context["dsn"].as_str().unwrap_or_default().to_owned();
        let readonly=context["readonly"].as_bool().unwrap_or(false);
        let catalog_id=context["id"].as_str().map(str::to_owned);
        let (model_tx,model_rx,active_generation,model_ready,model_error)=vision.unwrap_or_else(|| {
            let active=Arc::new(AtomicU64::new(0));
            let(tx,rx)=model::start(ctx.clone(),active.clone(),context["vision_python"].as_str().map(str::to_owned));
            (tx,rx,active,false,None)
        });
        let generation=active_generation.load(Ordering::Relaxed);
        let (db_tx, db_rx) = db::start_catalog(dsn.clone(), ctx.clone(), active_generation.clone(),catalog_id.clone(),readonly);
        let (decode_tx, decode_rx,decode_cancel) = decode::start_catalog(ctx.clone(),catalog_id.clone());
        let _ = db_tx.send(db::Request::Explore(explore::Request {
            generation,
            phase: 0,
            query: String::new(),
            folder: None,
            from_day: None,
            to_day: None,
            vector: None,
            source_image_id: None,
            show_missing: false,
            offset: 0,
            cursor: None,
            include_facets: true,
            relevance: 0.22,
            filters: Vec::new(),
            sort: SortSpec::default(),
            need_unique: false,
            need_geo: false,
            need_scatter: false,
            need_folder_month: false,
            page_limit:200, heatmap_year: Local::now().year(),
            duplicate_stacks: false,
        }));
        let _ = db_tx.send(db::Request::PeopleAll);
        let _ = db_tx.send(db::Request::Stats);
        let _ = db_tx.send(db::Request::SavedViews);
        if !readonly {let _ = db_tx.send(db::Request::BackfillVisuals);}
        let mut viewer = Self {
            catalog_panel: catalogs::Panel::new(context,&ctx),
            external_catalog_selector:false,
            db_tx,
            db_rx,
            decode_tx,
            decode_rx,decode_cancel,
            model_tx,
            model_rx,
            photos: Vec::new(),
            folder_tree: visuals::FolderTree::default(),
            map_tiles: Vec::new(),
            days: Vec::new(),
            facet_values: Vec::new(),
            geo_bins: Vec::new(),
            scatter_bins: Vec::new(),
            folder_months: Vec::new(),
            explore_visual: ExploreVisual::Folders,
            chart_drag: None,
            total_results: 0,
            map_root: None,
            from_day: None,
            to_day: None,
            map_metric: visuals::Metric::Bytes,
            timeline_drag: None,
            show_calendar: false,
            calendar_year: Local::now().year(),
            calendar_month: Local::now().month(),
            cached_vector: None,
            visual_source: None,
            phase: 0,
            relevance: 0.22,
            filters: Vec::new(),
            filter_field: "format".into(),
            filter_value: String::new(),
            filter_exclude: false,
            sort: SortSpec::default(),
            visual_complete: false,
            page_loading: true,
            refresh_span:200,
            page_error: false,
            facets_loading: true,
            show_other: false,
            thumbnails: HashMap::new(),
            full_images: HashMap::new(),
            document_artboards: HashMap::new(),
            face_images: HashMap::new(),
            pending_faces: HashSet::new(),
            pending_thumbs: HashSet::new(),
            missing_thumbs: HashSet::new(),
            pending_full: HashSet::new(),
            content_keys:HashMap::new(),face_keys:HashMap::new(),failed_full:HashMap::new(),failed_faces:HashMap::new(),last_displayed:None,
            requested_details:None,requested_open:None,requested_people:None,
            dialog_saving:false,tag_saving:false,group_detail:None,group_faces:Vec::new(),selected_faces:HashSet::new(),group_more:false,group_loading:false,groups_loading:false,groups_error:false,proposal_loading:false,proposal_error:false,identity_search:String::new(),person_confirmation:None,face_undo:None,source_revisions:HashMap::new(),source_paths:HashMap::new(),face_paths:HashMap::new(),face_target:None,
            people_manager:false,person_edit:None,person_name:String::new(),person_merge:None,
            saved_confirmation:None,sheet_destination:String::new(),export_rx:None,
            proposal_status:"pending".into(),proposal_search:String::new(),proposal_more:false,
            groups: Vec::new(),
            proposals: Vec::new(),
            collections_panel: collections::Panel::with_dsn(dsn,readonly),
            tab: Tab::Images,
            search: String::new(),
            search_changed: Instant::now(),
            search_sent: true,
            generation,
            model_ready,
            model_error,
            visual_pending: false,
            model_queue_pending:false,
            visual_results: None,
            last_stats_request: Instant::now(),
            show_missing: false,
            selected: None,
            last_image_click: None,
            view_mode: ViewMode::Grid,
            selected_ids: HashSet::new(),
            contact_photos: Vec::new(),
            contact_sheet_open: false,
            compare_ids: Vec::new(),
            preview_photo: None,
            active_generation,
            zoom: 1.0,
            tag_name: String::new(),
            tag_group: None,
            tag_person: None,
            known_people: Vec::new(),
            message: "Loading catalog…".into(),
            settings: settings::Settings::load(),
            editor_entry: String::new(),
            saved_views: Vec::new(),
            save_view_name: String::new(),
            dialog_photo: None,
            dialog_kind: None,
            dialog_tag: String::new(),
            dialog_destination: String::new(),
            dialog_reason: String::new(),
            details: None,
            person_choices: Vec::new(),
            frame: 0,
            frame_durations: VecDeque::with_capacity(600),
            ui_p95_ms: 0.0,
            embedded: 0,
            indexed: 0,
            stats_known: false,
            catalog_revision: String::new(),
            #[cfg(debug_assertions)]
            screenshot_path: std::env::var_os("IMAGE_CLI_VIEWER_SCREENSHOT").map(PathBuf::from),
            #[cfg(debug_assertions)]
            screenshot_requested: false,
            #[cfg(debug_assertions)]
            screenshot_expected_folder: std::env::var("IMAGE_CLI_VIEWER_SCREENSHOT_AFTER_FOLDER")
                .ok(),
        };
        #[cfg(debug_assertions)]
        if let Ok(folder) = std::env::var("IMAGE_CLI_VIEWER_INITIAL_FOLDER") {
            if !folder.trim().is_empty() {
                viewer.map_root = Some(folder);
                viewer.submit_search();
            }
        }
        if let Ok(query) = std::env::var("IMAGE_CLI_VIEWER_INITIAL_QUERY") {
            if !query.trim().is_empty() {
                viewer.search = query;
                viewer.submit_search();
            }
        }
        #[cfg(debug_assertions)]
        if viewer.screenshot_path.is_some() {
            if let Ok(value) = std::env::var("IMAGE_CLI_VIEWER_SCREENSHOT_GROUP") {
                if let Ok(group) = serde_json::from_str::<GroupBy>(&format!("\"{value}\"")) { viewer.sort.group_by = group; }
            }
            if let Ok(value) = std::env::var("IMAGE_CLI_VIEWER_SCREENSHOT_REPORT") {
                if let Ok(report) = serde_json::from_str::<ExploreVisual>(&format!("\"{value}\"")) { viewer.explore_visual = report; }
            }
            viewer.submit_search();
        }
        if let Some(error)=viewer.settings.error.as_ref(){viewer.message=error.clone();}
        #[cfg(debug_assertions)] if viewer.screenshot_path.is_some(){
            if std::env::var("IMAGE_CLI_QA_THEME").as_deref()==Ok("Light"){ctx.set_theme(egui::Theme::Light);}else{ctx.set_theme(egui::Theme::Dark);}
            match std::env::var("IMAGE_CLI_QA_TAB").as_deref(){
                Ok("People")=>{viewer.tab=Tab::People;let _=viewer.db_tx.send(db::Request::Groups);},
                Ok("Collections")=>{viewer.tab=Tab::Collections;viewer.collections_panel.refresh(&ctx);},
                Ok("Proposals")=>{viewer.tab=Tab::Proposals;viewer.request_proposals(false);},
                Ok("Databases")=>viewer.catalog_panel.open_manager(),
                _=>{},
            }
            if let Ok(mode)=std::env::var("IMAGE_CLI_QA_VIEW"){if let Ok(mode)=serde_json::from_str::<ViewMode>(&format!("\"{mode}\"")){viewer.view_mode=mode;}}
        }
        viewer
    }

    fn receive(&mut self, ctx: &egui::Context) {
        if let Some(result)=self.export_rx.as_ref().and_then(|rx|rx.try_recv().ok()){self.message=result.unwrap_or_else(|error|format!("Contact sheet export failed: {error}"));self.export_rx=None;}
        while let Ok(event) = self.db_rx.try_recv() {
            match event {
                db::Event::ExplorePage {
                    generation,
                    phase,
                    offset,
                    photos,
                } if generation == self.generation && phase == self.phase => {
                    for photo in &photos{self.track_content(photo.id,photo.sha256.clone());}
                    if offset == 0 {
                        let active=self.selected.and_then(|index|self.photos.get(index)).cloned().or_else(||self.preview_photo.clone());
                        self.photos = photos;
                        self.selected=active.as_ref().and_then(|photo|self.photos.iter().position(|p|p.id==photo.id));
                        self.preview_photo=if self.selected.is_none(){active}else{None};
                        self.page_loading = false;
                        if phase == 1 {
                            self.visual_results = Some(1000);
                            self.visual_pending = false;
                        }
                    } else if offset as usize == self.photos.len() {
                        self.photos.extend(photos);
                        self.page_loading = false;
                    }
                }
                db::Event::ExploreFacets {
                    generation,
                    phase,
                    total,
                    tree,
                    days,
                    values,
                    geo,
                    scatter,
                } if generation == self.generation && phase == self.phase => {
                    self.facets_loading = false;
                    self.total_results = total;
                    if self.message=="Loading catalog…"{self.message=format!("{total} images ready");}
                    self.folder_tree = tree;
                    self.days = days;
                    self.facet_values = values;
                    self.geo_bins = geo;
                    self.scatter_bins = scatter;
                    self.days.sort_unstable_by(|a, b| a.day.cmp(&b.day));
                    self.refresh_map_tiles();
                    if phase == 1 && !self.visual_complete {
                        self.phase = 2;
                        self.page_loading = true;
                        self.facets_loading = true;
                        self.request_explore(0, true);
                    } else if phase == 2 {
                        self.visual_complete = true;
                        self.visual_pending = false;
                    }
                }
                db::Event::ExploreError {
                    generation,
                    phase,
                    error,
                } if generation == self.generation && phase == self.phase => {
                    self.page_loading = false;
                    self.page_error = true;
                    self.facets_loading = false;
                    self.visual_pending = false;
                    self.message = format!("Search error: {error}");
                    self.photos.clear();self.total_results=0;self.days.clear();self.folder_tree=visuals::FolderTree::default();self.map_tiles.clear();self.geo_bins.clear();self.scatter_bins.clear();self.folder_months.clear();self.facet_values.clear();
                }
                db::Event::BackfillDone => self.refresh_search(),
                db::Event::Thumb(id, bytes,revision) => {
                    if self.content_keys.get(&id).is_some_and(|key|key!=&revision){continue;}
                    self.track_content(id,revision.clone());
                    if self
                        .decode_tx
                        .try_send(decode::Job::Thumb(id, bytes,revision))
                        .is_err()
                    {
                        self.pending_thumbs.remove(&id);
                    }
                }
                db::Event::Preview(id,bytes,revision) => {
                    if self.source_revisions.get(&id)!=Some(&revision){continue;}
                    if let Some(bytes)=bytes {
                        if self.decode_tx.try_send(decode::Job::Preview(id,bytes,revision)).is_err(){self.pending_full.remove(&id);}
                    }else{self.pending_full.remove(&id);self.failed_full.insert(id,"JPEG XL preview unavailable; rescan to backfill".into());}
                },
                db::Event::ThumbMissing(id) => {
                    self.pending_thumbs.remove(&id);
                    self.missing_thumbs.insert(id);
                }
                db::Event::OpenPhoto(photo) if self.requested_open==Some(photo.id) => {
                    self.requested_open=None;self.track_content(photo.id,photo.sha256.clone());
                    self.selected = None;
                    self.preview_photo = Some(photo);
                    self.zoom = 1.0;
                    self.tab = Tab::Images;
                }
                db::Event::Groups(groups) => {self.groups=groups;self.groups_loading=false;self.groups_error=false;},
                db::Event::RequestFailed(kind,error)=>{self.message=error;match kind {"groups"=>{self.groups_loading=false;self.groups_error=true;},"faces"=>self.group_loading=false,"proposals"=>{self.proposal_loading=false;self.proposal_error=true;},_=>{}}},
                db::Event::People(people) => {
                    self.known_people=people;
                    if self.person_edit.is_some_and(|id|!self.known_people.iter().any(|p|p.id==id)){self.person_edit=None;}
                    if self.tag_person.is_some_and(|id|!self.known_people.iter().any(|p|p.id==id&&!p.ignored)){self.tag_person=None;}
                },
                db::Event::ProposalPage{status,search,after,rows} if status==self.proposal_status&&search==self.proposal_search=>{
                    self.proposal_loading=false;self.proposal_error=false;self.proposal_more=rows.len()==100;
                    if after.is_none(){self.proposals=rows;}else if self.proposals.last().map(|p|p.id)==after{self.proposals.extend(rows);}
                },
                db::Event::ProposalsChanged=>self.request_proposals(false),
                db::Event::SavedViews(views) => self.saved_views = views,
                db::Event::Details(details) if self.requested_details==details["id"].as_i64() => self.details = Some(details),
                db::Event::GroupFaces{group,offset,samples} if self.group_detail==Some(group) => {
                    self.group_loading=false;if offset==0 {self.group_faces.clear();}
                    if offset as usize==self.group_faces.len(){self.group_more=samples.len()==100;self.group_faces.extend(samples);}
                },
                db::Event::FacesEdited(ids)=>self.selected_faces.retain(|id|!ids.contains(id)),
                db::Event::FaceUndoReady(snapshot)=>{self.face_undo=Some(snapshot);self.message="Face detections removed. Undo is available in People until this window or database closes.".into();},
                db::Event::FacesRestored=>{self.face_undo=None;self.message="Removed face detections restored".into();},
                db::Event::PersonMerged(source,target)=>{
                    for filter in &mut self.filters{if filter.field=="person_id"&&filter.value==source.to_string(){filter.value=target.to_string();}}
                    if self.tag_person==Some(source){self.tag_person=Some(target);}
                    if self.face_target==Some(source){self.face_target=Some(target);}
                },
                db::Event::Saved(kind,id)=>{
                    if kind=="person"{self.tag_saving=false;if self.tag_group==Some(id){self.tag_group=None;}}
                    if kind=="tag"||kind=="proposal"{self.dialog_saving=false;}
                    if self.dialog_photo.as_ref().is_some_and(|p|p.id==id)&&self.dialog_kind==Some(if kind=="tag"{"tag"}else{"move"}) {
                        self.dialog_saving=false;self.dialog_photo=None;self.dialog_kind=None;self.dialog_tag.clear();self.dialog_destination.clear();self.dialog_reason.clear();
                    }
                },
                db::Event::SaveFailed(kind,_id,error)=>{if kind=="person"{self.tag_saving=false;}if kind=="tag"||kind=="proposal"{self.dialog_saving=false;}self.message=error;},
                db::Event::PhotosUpdated{ids,photos}=>{
                    for photo in &photos{self.track_content(photo.id,photo.sha256.clone());}
                    if let Some(active)=self.preview_photo.as_ref(){if ids.contains(&active.id){self.preview_photo=photos.iter().find(|p|p.id==active.id).cloned();}}
                    self.compare_ids.retain_mut(|p|if !ids.contains(&p.id){true}else if let Some(updated)=photos.iter().find(|u|u.id==p.id){*p=updated.clone();true}else{false});
                    self.contact_photos.retain_mut(|p|if !ids.contains(&p.id){true}else if let Some(updated)=photos.iter().find(|u|u.id==p.id){*p=updated.clone();true}else{false});
                },
                db::Event::SelectedPhotos(photos) => { self.contact_photos = photos; self.contact_sheet_open = true; },
                db::Event::PeopleForImage(id,names) if self.requested_people==Some(id) => {
                    self.person_choices = names;
                    if self.person_choices.is_empty() { self.message = "No tagged people in this image".into(); }
                },
                db::Event::FolderMonths { generation, phase, rows }
                    if generation == self.generation && phase == self.phase => self.folder_months = rows,
                db::Event::Message(message) => {
                    if message.starts_with("Database unavailable")||message.starts_with("Unsupported database connection")||message.contains("Catalog identity mismatch"){
                        self.groups_loading=false;self.groups_error=true;self.group_loading=false;self.proposal_loading=false;self.proposal_error=true;
                    }
                    self.message=message;
                },
                db::Event::CatalogChanged => {
                    self.refresh_search();
                    if let Some(group)=self.group_detail {let _=self.db_tx.send(db::Request::GroupFaces{group,offset:0});}
                },
                db::Event::Stats {
                    total,
                    embedded,
                    revision,
                } => {
                    let catalog_changed = self.stats_known
                        && (total != self.indexed
                            || embedded != self.embedded
                            || revision != self.catalog_revision);
                    self.indexed = total;
                    self.embedded = embedded;
                    self.catalog_revision = revision;
                    self.stats_known = true;
                    if catalog_changed {
                        self.refresh_search();
                        if self.tab==Tab::People{let _=self.db_tx.send(db::Request::Groups);if self.people_manager{let _=self.db_tx.send(db::Request::PeopleAll);}}
                        if let Some(group)=self.group_detail{let _=self.db_tx.send(db::Request::GroupFaces{group,offset:0});}
                    }
                }
                _ => {}
            }
        }
        while let Ok(event) = self.model_rx.try_recv() {
            match event {
                model::Event::Ready => {
                    self.model_ready = true;
                    self.model_error = None;
                    self.message = "Visual search ready".into();

                }
                model::Event::Vector { generation, vector } if generation == self.generation => {
                    self.cached_vector = Some((self.search.clone(), vector));
                    self.phase = 1;
                    self.page_loading = true;
                    self.page_error = false;
                    self.facets_loading = true;
                    self.request_explore(0, true);
                }
                model::Event::Error(error) => {
                    self.visual_pending = false;
                    self.model_error = Some(error.clone());
                    self.message = error;
                }
                _ => {}
            }
        }
        for done in self.decode_rx.try_iter().take(16) {
            let keys=if done.face{&self.face_keys}else if done.full&&self.source_revisions.contains_key(&done.id){&self.source_revisions}else{&self.content_keys};
            if keys.get(&done.id).cloned().unwrap_or(None)!=done.revision{continue;}
            if done.full {
                self.pending_full.remove(&done.id);
            } else if done.face {
                self.pending_faces.remove(&done.id);
            } else {
                self.pending_thumbs.remove(&done.id);
            }
            match done.image {
                Ok(pixels) => {
                    if done.full
                        && self.full_images.get(&done.id).is_some_and(|old| {
                            old.texture.size()[0].max(old.texture.size()[1])
                                >= pixels.size[0].max(pixels.size[1])
                        })
                    {
                        continue;
                    }
                    let handle = ctx.load_texture(
                        format!("{}-{}", if done.full { "full" } else { "thumb" }, done.id),
                        pixels,
                        egui::TextureOptions::LINEAR,
                    );
                    let cache = CachedTexture {
                        texture: handle,
                        used: self.frame,
                    };
                    if done.full {
                        self.full_images.insert(done.id, cache);
                    } else if done.face {
                        self.face_images.insert(done.id, cache);
                    } else {
                        self.thumbnails.insert(done.id, cache);
                    }
                }
                Err(error) => {
                    if done.full {self.failed_full.insert(done.id,error.clone());}
                    else if done.face {self.failed_faces.insert(done.id,error.clone());}
                    else {self.missing_thumbs.insert(done.id);}
                    self.message=format!("Image #{}: {error}",done.id);
                },
            }
        }
        if !self.decode_rx.is_empty() {
            ctx.request_repaint();
        }
        self.trim_caches();
    }

    fn readonly(&self)->bool {self.catalog_panel.context["readonly"].as_bool().unwrap_or(false)}

    fn refresh_search(&mut self) {
        let photo=self.selected.and_then(|index|self.photos.get(index)).cloned().or_else(||self.preview_photo.clone());
        self.begin_search(self.photos.len().max(200) as i64);self.preview_photo=photo;
        let mut ids:HashSet<i64>=self.compare_ids.iter().chain(&self.contact_photos).map(|p|p.id).collect();
        if let Some(photo)=&self.preview_photo{ids.insert(photo.id);}if !ids.is_empty(){let _=self.db_tx.send(db::Request::RefreshPhotos(ids.into_iter().collect()));}
    }

    fn record_displayed(&mut self,id:i64) {
        if self.last_displayed!=Some(id) {self.last_displayed=Some(id);if !self.readonly(){let _=self.db_tx.try_send(db::Request::Viewed(id));}}
    }

    fn submit_search(&mut self) {self.begin_search(200);}

    fn begin_search(&mut self,span:i64) {
        self.refresh_span=span;
        self.generation += 1;
        self.active_generation
            .store(self.generation, Ordering::Relaxed);
        self.search_sent = true;
        self.selected = None;
        self.preview_photo = None;
        self.visual_results = None;
        self.visual_complete = false;
        let query = self.search.clone();
        if self
            .cached_vector
            .as_ref()
            .is_some_and(|(text, _)| text != &query)
        {
            self.cached_vector = None;
        }
        self.phase = if self.visual_source.is_some() || (!query.trim().is_empty() && self.cached_vector.is_some()) {
            1
        } else {
            0
        };
        self.page_loading = true;
        self.page_error = false;
        self.facets_loading = true;
        self.request_explore(0, true);
        self.visual_pending =
            !query.trim().is_empty() && self.visual_source.is_none() && self.model_error.is_none() && self.phase == 0;
        self.model_queue_pending=false;
        if self.visual_pending {
            self.model_queue_pending = self.model_tx.try_send(model::Request::Encode {
                generation: self.generation,
                text: query,
            }).is_err();
        }
    }

    fn request_explore(&self, offset: i64, include_facets: bool) {
        let vector = if self.phase >= 1 && self.visual_source.is_none() {
            self.cached_vector
                .as_ref()
                .map(|(_, vector)| vector.clone())
        } else {
            None
        };
        let _ = self.db_tx.send(db::Request::Explore(explore::Request {
            generation: self.generation,
            phase: self.phase,
            query: self.search.clone(),
            folder: self.map_root.clone(),
            from_day: self.from_day.clone(),
            to_day: self.to_day.clone(),
            vector,
            source_image_id: self.visual_source,
            show_missing: self.show_missing,
            offset,
            cursor: if offset > 0 { self.photos.last().map(|photo| explore::PageCursor {
                id: photo.id, primary: photo.cursor_primary.clone(),
                secondary: photo.cursor_secondary.clone(), group: photo.image_group.clone(),
            }) } else { None },
            include_facets,
            relevance: self.relevance,
            filters: self.filters.clone(),
            sort: self.sort,
            need_unique: self.explore_visual == ExploreVisual::Folders
                && matches!(self.map_metric, visuals::Metric::UniqueBytes | visuals::Metric::DuplicateWaste),
            need_geo: self.explore_visual == ExploreVisual::Gps,
            need_scatter: self.explore_visual == ExploreVisual::SizePixels,
            need_folder_month: self.explore_visual == ExploreVisual::FolderMonths,
            page_limit:if offset==0{self.refresh_span}else{200}, heatmap_year: self.calendar_year,
            duplicate_stacks: self.view_mode == ViewMode::Stacks,
        }));
    }

    fn refresh_map_tiles(&mut self) {
        self.map_tiles = self
            .folder_tree
            .tiles(self.map_root.as_deref(), self.map_metric);
        if !self
            .map_tiles
            .iter()
            .any(|tile| matches!(tile.kind, visuals::TileKind::Other))
        {
            self.show_other = false;
        }
    }

    fn request_thumbs(&mut self, ids: &[i64]) {
        let ids: Vec<i64> = ids
            .iter()
            .copied()
            .filter(|id| {
                !self.thumbnails.contains_key(id)
                    && !self.missing_thumbs.contains(id)
                    && self.pending_thumbs.insert(*id)
            })
            .collect();
        for batch in ids.chunks(64) {
            let _ = self.db_tx.send(db::Request::Thumbs(batch.to_vec()));
        }
    }

    fn track_content(&mut self,id:i64,revision:Option<String>){
        if self.content_keys.get(&id).is_some_and(|old|old!=&revision){self.thumbnails.remove(&id);self.full_images.remove(&id);self.source_revisions.remove(&id);self.pending_full.remove(&id);self.pending_thumbs.remove(&id);self.missing_thumbs.remove(&id);self.failed_full.remove(&id);}
        self.content_keys.insert(id,revision);
    }

    fn request_full(&mut self, photo: &Photo, edge: u32) {
        self.request_image(photo, edge, false);
    }

    fn request_database_preview(&mut self, photo: &Photo) {
        self.request_image(photo, 1024, true);
    }

    fn request_image(&mut self, photo: &Photo, edge: u32, cached_only: bool) {
        if self.content_keys.get(&photo.id).is_some_and(|key|key!=&photo.sha256){return;}
        if let Some(document)=&photo.document && !cached_only {
            if document["preview_available"].as_bool()==Some(false) {
                self.failed_full.insert(photo.id,document["preview_reason"].as_str().unwrap_or("Document metadata indexed; preview unavailable").to_owned());
                return;
            }
        }
        let is_document=photo.format.as_deref().is_some_and(|format|matches!(format,"PSD"|"PSB"|"AI"));
        let edge=if is_document{edge.min(2048)}else{edge.min(1024)};
        let pages=photo.document.as_ref().and_then(|doc|doc["page_count"].as_u64()).unwrap_or(1).max(1) as u32;
        let page=if cached_only {0} else {(*self.document_artboards.get(&photo.id).unwrap_or(&0)).min(pages-1)};
        if self.source_paths.get(&photo.id).is_some_and(|path|path!=&photo.path){self.failed_full.remove(&photo.id);self.pending_full.remove(&photo.id);self.full_images.remove(&photo.id);}
        self.source_paths.insert(photo.id,photo.path.clone());
        let revision=Some(format!("{}|{}|{}",photo.sha256.as_deref().unwrap_or(""),photo.path,page));
        if self.source_revisions.get(&photo.id).is_some_and(|old|old!=&revision) {
            self.full_images.remove(&photo.id);self.pending_full.remove(&photo.id);self.failed_full.remove(&photo.id);
        }
        self.source_revisions.insert(photo.id,revision.clone());
        if page==0 {
            if !self.full_images.contains_key(&photo.id)&&!self.failed_full.contains_key(&photo.id)&&self.pending_full.insert(photo.id){
                if self.db_tx.send(db::Request::Preview(photo.id,revision)).is_err(){self.pending_full.remove(&photo.id);}
            }
            return;
        }
        let available = self.full_images.get(&photo.id).is_some_and(|image| {
            let size = image.texture.size();
            size[0].max(size[1]) >= edge.min(photo.width.max(photo.height) as u32) as usize
        });
        if !photo.missing && !self.failed_full.contains_key(&photo.id)
            && !available
            && self.pending_full.insert(photo.id)
            && self
                .decode_tx
                .try_send(decode::Job::Full(
                    photo.id,
                    PathBuf::from(&photo.path),
                    edge,page,revision,
                ))
                .is_err()
        {
            self.pending_full.remove(&photo.id);
        }
    }

    fn request_face(&mut self, sample: &FaceSample) {
        if self.face_paths.get(&sample.face_id).is_some_and(|path|path!=&sample.path){self.failed_faces.remove(&sample.face_id);self.pending_faces.remove(&sample.face_id);self.face_images.remove(&sample.face_id);}
        self.face_paths.insert(sample.face_id,sample.path.clone());
        let revision=Some(format!("{}|{}",sample.sha256.as_deref().unwrap_or(""),sample.path));
        if self.face_keys.get(&sample.face_id).is_some_and(|key|key!=&revision){self.face_images.remove(&sample.face_id);self.failed_faces.remove(&sample.face_id);self.pending_faces.remove(&sample.face_id);}
        self.face_keys.insert(sample.face_id,revision.clone());
        if self.failed_faces.contains_key(&sample.face_id){return;}
        if !self.face_images.contains_key(&sample.face_id)
            && self.pending_faces.insert(sample.face_id)
            && self
                .decode_tx
                .try_send(decode::Job::Face(
                    sample.face_id,
                    PathBuf::from(&sample.path),
                    sample.box_xywh,revision,
                ))
                .is_err()
        {
            self.pending_faces.remove(&sample.face_id);
        }
    }

    fn trim_caches(&mut self) {
        while self.thumbnails.len() > 512 {
            if let Some(id) = self
                .thumbnails
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| *k)
            {
                self.thumbnails.remove(&id);
            }
        }
        let active_limit=96;
        while self.full_images.len() > active_limit {
            if let Some(id) = self
                .full_images
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| *k)
            {
                self.full_images.remove(&id);
            }
        }
        while self
            .full_images
            .values()
            .map(|v| {
                let size = v.texture.size();
                size[0] * size[1] * 4
            })
            .sum::<usize>()
            > 256 * 1024 * 1024
            && self.full_images.len() > 1
        {
            if let Some(id) = self
                .full_images
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| *k)
            {
                self.full_images.remove(&id);
            }
        }
        while self.face_images.len() > 256 {
            if let Some(id) = self
                .face_images
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| *k)
            {
                self.face_images.remove(&id);
            }
        }
    }

    fn image_menu(ui: &mut egui::Ui, photo: &Photo, editors: &[PathBuf],readonly:bool) -> Option<ImageAction> {
        let mut picked = None;
        for (label, action) in [
            ("Browse this folder", ImageAction::BrowseFolder),
            ("Show same day", ImageAction::SameDay),
            ("Show nearby week", ImageAction::NearbyDays),
            ("Reveal in Explorer", ImageAction::Reveal),
            ("Open containing folder", ImageAction::OpenFolder),
            ("Open original", ImageAction::OpenOriginal),
            ("Open with…", ImageAction::OpenWith),
            ("Copy path", ImageAction::CopyPath),
            (if photo.favorite { "Remove favorite" } else { "Favorite" }, ImageAction::Favorite),
            ("Toggle selection", ImageAction::Select),
            ("Retry thumbnail / preview",ImageAction::RetryPreview),
            ("Pin for comparison", ImageAction::Compare),
            ("Find visually similar", ImageAction::Similar),
            ("Find exact duplicates", ImageAction::ExactDuplicates),
            ("Show all exact copies", ImageAction::ExpandStack),
            ("Find near duplicates", ImageAction::NearDuplicates),
            ("Show same camera", ImageAction::SameCamera),
            ("Show same indexed source", ImageAction::SameSource),
            ("Find similar dominant colors", ImageAction::SimilarColor),
            ("Show same burst (±10 s)", ImageAction::SameBurst),
            ("Show detected event", ImageAction::SameEvent),
            ("Show tagged people", ImageAction::SamePerson),
            ("Mark reviewed", ImageAction::SetReview("reviewed")),
            ("Flag for review", ImageAction::SetReview("flagged")),
            ("Reject", ImageAction::SetReview("rejected")),
            ("Add tag…", ImageAction::AddTag),
            ("Propose move…", ImageAction::ProposeMove),
            ("Image properties…", ImageAction::Details),
        ] {
            let mutating=matches!(action,ImageAction::Favorite|ImageAction::Rate(_)|ImageAction::SetReview(_)|ImageAction::AddTag|ImageAction::ProposeMove);
            let needs_original=matches!(action,ImageAction::Reveal|ImageAction::OpenOriginal|ImageAction::OpenWith|ImageAction::OpenEditor(_));
            if ui.add_enabled(!(readonly&&mutating)&&(!photo.missing||!needs_original),
                egui::Button::new(label)).clicked() {
                picked = Some(action);
            }
        }
        for (index, editor) in editors.iter().enumerate() {
            let name = editor.file_stem().unwrap_or_default().to_string_lossy();
            if ui.add_enabled(!photo.missing, egui::Button::new(format!("Open in {name}"))).clicked() {
                picked = Some(ImageAction::OpenEditor(index));
            }
        }
        ui.add_enabled_ui(!readonly,|ui|{ui.menu_button("Rate", |ui| {
            for rating in 0..=5 {
                if ui.button(format!("{rating} stars")).clicked() {
                    picked = Some(ImageAction::Rate(rating));
                    ui.close();
                }
            }
        });
        });
        if picked.is_some() { ui.close(); }
        picked
    }

    fn apply_image_action(&mut self, photo: &Photo, action: ImageAction, ctx: &egui::Context) {
        if self.readonly()&&matches!(action,ImageAction::Favorite|ImageAction::Rate(_)|ImageAction::SetReview(_)|ImageAction::AddTag|ImageAction::ProposeMove){self.message="Catalog is read-only".into();return;}
        if matches!(action,ImageAction::Reveal|ImageAction::OpenFolder|ImageAction::OpenOriginal|ImageAction::OpenWith|ImageAction::OpenEditor(_)) {
            if let Err(error)=catalogs::allowed(if matches!(action,ImageAction::OpenFolder){std::path::Path::new(&photo.path).parent().unwrap_or(std::path::Path::new(&photo.path))}else{std::path::Path::new(&photo.path)},self.catalog_panel.context["id"].as_str()) {self.message=error;return;}
        }
        let path = std::path::Path::new(&photo.path);
        let result = match action {
            ImageAction::BrowseFolder => {
                if let Some(parent) = path.parent() {
                    self.map_root = Some(parent.to_string_lossy().into_owned());
                    self.tab = Tab::Images;
                    self.submit_search();
                }
                Ok(())
            }
            ImageAction::SameDay | ImageAction::NearbyDays => {
                if let Some(day) = photo.capture_day.as_deref().and_then(|day| NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()) {
                    let span = if matches!(action, ImageAction::NearbyDays) { 7 } else { 0 };
                    self.from_day = Some((day - chrono::Duration::days(span)).to_string());
                    self.to_day = Some((day + chrono::Duration::days(span)).to_string());
                    self.tab = Tab::Images;
                    self.submit_search();
                }
                Ok(())
            }
            ImageAction::Reveal => actions::reveal(path),
            ImageAction::OpenFolder => path.parent().ok_or("No parent folder".into()).and_then(actions::open_folder),
            ImageAction::OpenOriginal => actions::open_original(path),
            ImageAction::OpenWith => actions::open_with(path),
            ImageAction::OpenEditor(index) => self.settings.editors.get(index)
                .ok_or("Editor is no longer configured".into())
                .and_then(|editor| actions::open_editor(editor, path)),
            ImageAction::CopyPath => { ctx.copy_text(photo.path.clone()); Ok(()) },
            ImageAction::Favorite => { let _ = self.db_tx.send(db::Request::SetFavorite { id: photo.id, favorite: !photo.favorite }); Ok(()) },
            ImageAction::Rate(rating) => { let _ = self.db_tx.send(db::Request::SetRating { id: photo.id, rating }); Ok(()) },
            ImageAction::Select => { if !self.selected_ids.insert(photo.id){self.selected_ids.remove(&photo.id);} Ok(()) },
            ImageAction::RetryPreview=>{self.failed_full.remove(&photo.id);self.missing_thumbs.remove(&photo.id);self.thumbnails.remove(&photo.id);self.pending_thumbs.remove(&photo.id);self.request_thumbs(&[photo.id]);Ok(())},
            ImageAction::Compare => {
                if !self.compare_ids.iter().any(|p| p.id == photo.id) {
                    if self.compare_ids.len() == 4 { self.compare_ids.remove(0); }
                    self.compare_ids.push(photo.clone());
                }
                Ok(())
            }
            ImageAction::Similar => {
                self.search.clear();
                self.visual_source = Some(photo.id);
                self.submit_search();
                Ok(())
            }
            ImageAction::ExactDuplicates => {
                self.visual_source = None;
                self.search.clear();
                self.filters.push(FilterChip { field: "duplicate_of".into(),
                    value: photo.id.to_string(), exclude: false });
                self.submit_search();
                Ok(())
            }
            ImageAction::ExpandStack => {
                self.visual_source = None;
                self.search.clear();
                self.map_root = None;
                self.from_day = None;
                self.to_day = None;
                self.filters.clear();
                self.filters.push(FilterChip { field: "same_hash_of".into(),
                    value: photo.id.to_string(), exclude: false });
                self.view_mode = ViewMode::Grid;
                self.submit_search();
                Ok(())
            }
            ImageAction::NearDuplicates => {
                self.visual_source = None;
                self.search.clear();
                self.filters.push(FilterChip { field: "near_of".into(),
                    value: photo.id.to_string(), exclude: false });
                self.submit_search();
                Ok(())
            }
            ImageAction::SameCamera => {
                if let Some(camera) = &photo.camera_model {
                    self.filters.push(FilterChip { field: "camera".into(), value: camera.clone(), exclude: false });
                    self.submit_search();
                } else { self.message = "This image has no camera model".into(); }
                Ok(())
            }
            ImageAction::SameSource => {
                self.filters.push(FilterChip { field: "root".into(), value: photo.root_path.clone(), exclude: false });
                self.submit_search(); Ok(())
            }
            ImageAction::SimilarColor => {
                if let Some(color) = photo.dominant_rgb {
                    self.filters.push(FilterChip { field: "dominant_color".into(),
                        value: format!("{color:06x}"), exclude: false });
                    self.submit_search();
                } else { self.message = "No color summary is indexed for this image".into(); }
                Ok(())
            }
            ImageAction::SameBurst | ImageAction::SameEvent => {
                if photo.capture_at.is_some() {
                    let field = if matches!(action, ImageAction::SameBurst) { "burst_of" } else { "event_of" };
                    self.filters.push(FilterChip { field: field.into(), value: photo.id.to_string(), exclude: false });
                    self.submit_search();
                } else { self.message = "This image has no camera timestamp".into(); }
                Ok(())
            }
            ImageAction::SamePerson => { self.requested_people=Some(photo.id);let _ = self.db_tx.send(db::Request::PeopleForImage(photo.id)); Ok(()) }
            ImageAction::SetReview(state) => {
                let _ = self.db_tx.send(db::Request::SetReview { id: photo.id, state: state.into() });
                Ok(())
            }
            ImageAction::AddTag => {
                if self.dialog_saving {self.message="Previous tag or proposal save is still pending".into();return;}
                self.dialog_photo = Some(photo.clone()); self.dialog_kind = Some("tag"); Ok(())
            }
            ImageAction::ProposeMove => {
                if self.dialog_saving {self.message="Previous tag or proposal save is still pending".into();return;}
                self.dialog_photo = Some(photo.clone()); self.dialog_kind = Some("move"); Ok(())
            }
            ImageAction::Details => { self.requested_details=Some(photo.id);self.details=None;let _ = self.db_tx.send(db::Request::Details(photo.id)); Ok(()) }
        };
        if let Err(error) = result { self.message = error; }
    }

    fn saved_state(&self) -> serde_json::Value {
        serde_json::to_value(SavedState {
            search: self.search.clone(), folder: self.map_root.clone(),
            visual_source: self.visual_source,
            from_day: self.from_day.clone(), to_day: self.to_day.clone(),
            relevance: self.relevance, filters: self.filters.clone(), sort: self.sort,
            show_missing: self.show_missing,
            view_mode: self.view_mode, explore_visual: self.explore_visual,
            map_metric: self.map_metric,
            show_calendar: self.show_calendar,
            calendar_year: Some(self.calendar_year),
            calendar_month: Some(self.calendar_month),
        }).expect("saved view is serializable")
    }

    fn restore_state(&mut self, state: serde_json::Value) {
        match serde_json::from_value::<SavedState>(state) {
            Ok(state) => {
                self.search = state.search;
                self.visual_source = state.visual_source;
                self.map_root = state.folder;
                self.from_day = state.from_day;
                self.to_day = state.to_day;
                self.relevance = state.relevance;
                self.filters = state.filters;
                self.sort = state.sort;
                self.show_missing = state.show_missing;
                self.view_mode = state.view_mode;
                self.explore_visual = state.explore_visual;
                self.map_metric = state.map_metric;
                self.show_calendar = state.show_calendar;
                self.calendar_year = state.calendar_year.unwrap_or(Local::now().year());
                self.calendar_month = state.calendar_month.unwrap_or(Local::now().month()).clamp(1,12);
                self.submit_search();
            }
            Err(error) => self.message = format!("Invalid saved view: {error}"),
        }
    }

    fn images_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Search photos");
            let input = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(ui.available_width().min(420.0).max(150.0))
                    .hint_text("dog, beach sunset, a person's name..."),
            ).on_hover_text("Search filenames, paths, tags, named people, and image content; other filters remain active");
            if input.changed() {
                self.visual_source = None;
                self.search_changed = Instant::now();
                self.search_sent = false;
            }
            if ui
                .checkbox(&mut self.show_missing, "Show missing").on_hover_text("Include indexed images whose originals are missing; cached thumbnails remain available")
                .changed()
            {
                self.submit_search();
            }
            ui.label(format!("{} {}", self.total_results,
                if self.view_mode == ViewMode::Stacks { "stacks" } else { "images" })).on_hover_text("Total filtered results, including pages not loaded yet");
            if self.embedded < self.indexed {
                ui.add(egui::Label::new(format!("Visual index: {}/{}",self.embedded,self.indexed)).extend()).on_hover_text("Images with compatible visual-search embeddings / indexed images");
            }
        });
        if !self.search.trim().is_empty() || self.visual_source.is_some() {
            if let Some(error) = &self.model_error {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    format!("Visual search unavailable: {error}"),
                );
            if ui.button("Retry visual search").on_hover_text("Restart the model after a failed or timed-out request").clicked(){
                    let _=self.model_tx.try_send(model::Request::Retry);self.model_error=None;self.model_ready=false;self.cached_vector=None;self.submit_search();
                }
            } else if !self.model_ready {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.label("Loading OpenCLIP; your search will run when it is ready…");
                });
            } else if self.visual_pending {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.label("Searching image content…");
                });
            } else if self.visual_results.is_some() {
                ui.label(if self.visual_complete {
                    "Visual search complete · counts cover all matches above the relevance cutoff"
                } else {
                    "Showing first visual matches · counts and charts are partial"
                });
            }
            if ui.add(egui::Slider::new(&mut self.relevance, 0.08..=0.45).text("Relevance")).changed() {
                self.submit_search();
            }
        }
        if sorting_toolbar(ui, &mut self.sort) { self.submit_search(); }
        let previous_view = self.view_mode;
        ui.horizontal_wrapped(|ui| {
            ui.label("View");
            ui.selectable_value(&mut self.view_mode, ViewMode::Grid, "Grid").on_hover_text("Browse thumbnails; right-click for image actions");
            ui.selectable_value(&mut self.view_mode, ViewMode::Table, "Details").on_hover_text("Browse sortable image metadata in aligned columns");
            ui.selectable_value(&mut self.view_mode, ViewMode::Filmstrip, "Filmstrip").on_hover_text("Browse large images with a thumbnail strip and Left/Right navigation");
            ui.selectable_value(&mut self.view_mode, ViewMode::Stacks, "Duplicate stacks").on_hover_text("Group exact copies; click a stack to expand all copies");
            if !self.selected_ids.is_empty() {
                ui.label(format!("{} selected", self.selected_ids.len()));
                if ui.small_button("Contact sheet").on_hover_text("Preview and export the selected images as a PNG sheet").clicked() {
                    let _ = self.db_tx.send(db::Request::LookupImages(self.selected_ids.iter().copied().collect()));
                }
                if ui.small_button("Clear selection").on_hover_text("Remove all selected images; individual images can also be toggled from their menus").clicked() { self.selected_ids.clear(); }
            }
            if !self.compare_ids.is_empty() {
                ui.label(format!("{} pinned for comparison", self.compare_ids.len()));
                if ui.small_button("Clear comparison").on_hover_text("Unpin all comparison images").clicked() { self.compare_ids.clear(); }
            }
        });
        if previous_view != self.view_mode
            && (previous_view == ViewMode::Stacks || self.view_mode == ViewMode::Stacks) {
            self.submit_search();
        }
        ui.collapsing("Filters, editors and saved searches", |ui| {
        egui::ScrollArea::vertical().id_salt("auxiliary-tools").max_height((ui.available_height()*0.35).clamp(90.0,180.0)).show(ui,|ui|{
        ui.collapsing(format!("Filters ({})", self.filters.len()), |ui| {
            let fields = [
                ("Format", "format"), ("Camera", "camera"), ("Camera make", "camera_make"),
                ("Lens", "lens"), ("Content label", "label"),
                ("Person ID", "person_id"), ("Tag", "tag"), ("Source", "root"),
                ("Collection ID", "collection"),
                ("Direct folder", "folder_direct"), ("Review state", "review"),
                ("Date source", "date_source"), ("Embedding status", "embedding_status"),
                ("Face status", "face_status"), ("Minimum bytes", "size_min"),
                ("Label status", "labels_status"),
                ("Maximum bytes", "size_max"), ("Minimum pixels", "pixels_min"),
                ("Maximum pixels", "pixels_max"), ("Minimum rating", "rating_min"),
                ("Minimum faces", "faces_min"), ("Maximum faces", "faces_max"),
                ("Modified from YYYY-MM-DD", "modified_from"),
                ("Modified to YYYY-MM-DD", "modified_to"),
                ("Indexed from YYYY-MM-DD", "indexed_from"),
                ("Indexed to YYYY-MM-DD", "indexed_to"),
                ("Brightness min 0–1", "brightness_min"), ("Brightness max 0–1", "brightness_max"),
                ("Sharpness min", "sharpness_min"), ("Sharpness max", "sharpness_max"),
                ("Aspect min", "aspect_min"), ("Aspect max", "aspect_max"),
                ("Latitude min", "lat_min"), ("Latitude max", "lat_max"),
                ("Longitude min", "lon_min"), ("Longitude max", "lon_max"),
                ("Dominant color #RRGGBB", "dominant_color"),
                ("Orientation", "orientation"), ("Favorite true/false", "favorite"),
                ("Has GPS true/false", "gps"), ("Alpha true/false", "alpha"),
                ("Animated true/false", "animated"), ("Missing true/false", "missing"),
                ("RAW true/false", "raw"), ("High bit depth true/false", "high_bit_depth"),
                ("Faces true/false", "faces"), ("Generated true/false", "generated"),
                ("Exact duplicates true/false", "exact_duplicates"),
            ];
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("filter-field")
                    .selected_text(fields.iter().find(|(_, key)| *key == self.filter_field)
                        .map(|(label, _)| *label).unwrap_or("Field"))
                    .show_ui(ui, |ui| {
                        for (label, key) in fields { ui.selectable_value(&mut self.filter_field, key.to_owned(), label).on_hover_text(format!("Filter by {label}")); }
                    }).response.on_hover_text("Choose the field and enter a value with the displayed units");
                if self.filter_field=="person_id"{
                    egui::ComboBox::from_id_salt("person-filter").selected_text(self.known_people.iter().find(|p|p.id.to_string()==self.filter_value).map(|p|format!("{} (#{})",p.name,p.id)).unwrap_or_else(||"Choose person".into())).show_ui(ui,|ui|{for p in self.known_people.iter().filter(|p|!p.ignored){ui.selectable_value(&mut self.filter_value,p.id.to_string(),format!("{} (#{})",p.name,p.id)).on_hover_text("Filter by this stable identity, including after a rename");}}).response.on_hover_text("Select an existing identity; the saved filter stores its ID");
                }else{ui.add(egui::TextEdit::singleline(&mut self.filter_value).hint_text("Value")).on_hover_text("Dates: YYYY-MM-DD; booleans: true/false; sizes: bytes; resolution: pixels");}
                ui.checkbox(&mut self.filter_exclude, "Exclude").on_hover_text("Exclude matching images instead of including them");
                if ui.button("Add filter").on_hover_text("Validate and apply this filter to images and reports").clicked() && !self.filter_value.trim().is_empty() {
                    let chip=FilterChip{field:self.filter_field.clone(),value:self.filter_value.trim().to_owned(),exclude:self.filter_exclude};
                    match explore::validate_filters(std::slice::from_ref(&chip)){Ok(())=>{self.filters.push(chip);self.filter_value.clear();self.submit_search();},Err(error)=>self.message=format!("Invalid filter: {error}")};
                }
            });
            let mut remove = None;
            let mut toggle = None;
            ui.horizontal_wrapped(|ui| {
                for (index, chip) in self.filters.iter().enumerate() {
                    let prefix = if chip.exclude { "Exclude" } else { "Include" };
                    let response = ui.button(format!("{prefix} {}: {} ×", chip.field, chip.value)).on_hover_text("Click to remove this filter; right-click to toggle include/exclude");
                    if response.clicked() { remove = Some(index); }
                    response.context_menu(|ui| { if ui.button("Toggle include/exclude").clicked() { toggle = Some(index); ui.close(); } });
                }
            });
            if let Some(index) = remove { self.filters.remove(index); self.submit_search(); }
            if let Some(index) = toggle { self.filters[index].exclude = !self.filters[index].exclude; self.submit_search(); }
        });
        ui.collapsing("Quick filters", |ui| {
            let mut add = None;
            for (field, heading) in [("format", "Formats"), ("camera", "Cameras"), ("review", "Review states")] {
                ui.label(heading);
                let mut values: Vec<_> = self.facet_values.iter().filter(|value| value.field == field).collect();
                values.sort_unstable_by(|a, b| b.count.cmp(&a.count));
                ui.horizontal_wrapped(|ui| {
                    for value in values.into_iter().take(12) {
                        let response = ui.button(format!("{} ({})", value.value, value.count)).on_hover_text("Include this value; the count covers the filtered result set. Right-click to exclude it.");
                        if response.clicked() { add = Some(FilterChip { field: field.into(), value: value.value.clone(), exclude: false }); }
                        response.context_menu(|ui| {
                            if ui.button("Exclude").clicked() {
                                add = Some(FilterChip { field: field.into(), value: value.value.clone(), exclude: true });
                                ui.close();
                            }
                        });
                    }
                });
            }
            if let Some(chip) = add { self.filters.push(chip); self.submit_search(); }
        });
        ui.collapsing("Favorite editors", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.editor_entry)
                    .desired_width(ui.available_width().min(360.0)).hint_text("Editor executable path")).on_hover_text("Absolute path of an existing editor executable");
                if ui.button("Add executable").on_hover_text("Add an existing .exe or .com editor; image paths are passed as one argument").clicked() {
                    let path = PathBuf::from(self.editor_entry.trim());
                    if actions::valid_editor(&path) && !self.settings.editors.contains(&path) {
                        self.settings.editors.push(path);
                        if let Err(error) = self.settings.save() { self.message = error; }
                        self.editor_entry.clear();
                    } else { self.message = "Choose an existing editor executable".into(); }
                }
            });
            let mut remove = None;
            for (index, path) in self.settings.editors.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::Label::new(path.display().to_string()).truncate()).on_hover_text(path.display().to_string());
                    if ui.small_button("Remove").on_hover_text("Remove this editor from favorites").clicked() { remove = Some(index); }
                });
            }
            if let Some(index) = remove {
                self.settings.editors.remove(index);
                if let Err(error) = self.settings.save() { self.message = error; }
            }
        });
        ui.collapsing("Saved searches", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.save_view_name)
                    .hint_text("Name this search")).on_hover_text("A saved view stores the query, filters, sorting, grouping, report, and calendar state");
                if ui.add_enabled(!self.readonly(),egui::Button::new("Save current view")).on_hover_text("Save this search, sorting, grouping, and report configuration").clicked() {
                    let name = self.save_view_name.trim().to_owned();
                    if !name.is_empty() {
                        let state=self.saved_state();
                        if self.saved_views.iter().any(|v|v.name==name){self.saved_confirmation=Some((name,Some(state)));}
                        else{let _=self.db_tx.send(db::Request::SaveView{name,state});}
                    }
                }
            });
            let mut load = None;
            let mut delete = None;
            for view in &self.saved_views {
                ui.horizontal_wrapped(|ui| {
                    if ui.add(egui::Button::new(&view.name).truncate()).on_hover_text("Restore this saved query and its display configuration").clicked() { load = Some(view.state.clone()); }
                    if ui.add_enabled(!self.catalog_panel.context["readonly"].as_bool().unwrap_or(false),egui::Button::new("Delete").small()).on_hover_text("Delete this saved view after confirmation").clicked() { delete = Some(view.name.clone()); }
                });
            }
            if let Some(state) = load { self.restore_state(state); }
            if let Some(name) = delete { self.saved_confirmation=Some((name,None)); }
        });
        });
        });
        ui.separator();
        if self.view_mode==ViewMode::Filmstrip {
            self.results_ui(ui);
            ui.collapsing("Reports",|ui|{ui.set_min_height(130.0);self.visual_ui(ui);self.timeline_ui(ui);});
            return;
        }
        let content_height = (ui.available_height() - if self.show_calendar { 210.0 } else { 165.0 }).max(if ui.available_width()<720.0 {280.0}else{100.0});
        if ui.available_width() < 720.0 {
            let width = ui.available_width();
            let map_height = (content_height * 0.45).clamp(130.0, 220.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, map_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.visual_ui(ui),
            );
            ui.allocate_ui_with_layout(
                egui::vec2(width, content_height - map_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.results_ui(ui),
            );
        } else {
            let map_width = (ui.available_width() * 0.28).clamp(220.0, 480.0);
            let grid_width = (ui.available_width() - map_width - ui.spacing().item_spacing.x * 2.0 - 1.0).max(220.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(map_width, content_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.visual_ui(ui),
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(grid_width, content_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.results_ui(ui),
                );
            });
        }
        ui.separator();
        self.timeline_ui(ui);
    }

    fn results_ui(&mut self, ui: &mut egui::Ui) {
        match self.view_mode {
            ViewMode::Grid => self.grid_ui(ui),
            ViewMode::Table => self.table_ui(ui),
            ViewMode::Filmstrip => self.filmstrip_ui(ui),
            ViewMode::Stacks => self.grid_ui(ui),
        }
    }

    fn image_click_action(&mut self, index: usize, double_clicked: bool, ctx: &egui::Context) -> Option<ImageAction> {
        let photo = self.photos.get(index)?.clone();
        self.selected = Some(index);
        self.preview_photo = None;
        self.zoom = 1.0;
        self.record_displayed(photo.id);
        self.last_image_click = ctx.input(|input| input.pointer.interact_pos().map(|position| (photo.id, position, input.time)));
        double_clicked.then_some(ImageAction::OpenOriginal)
    }

    fn table_ui(&mut self, ui: &mut egui::Ui) {
        let usable=(ui.available_width()-if self.sort.group_by!=GroupBy::None{106.0}else{0.0}-48.0).max(480.0);
        let widths=[usable*0.29,usable*0.14,usable*0.10,usable*0.14,usable*0.09,usable*0.10,usable*0.14];
        if ui.available_width()<usable+48.0{ui.small("Scroll horizontally for all columns").on_hover_text("Shift + wheel or the bottom horizontal scrollbar");}
        egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).id_salt("table-horizontal").show(ui,|ui|{
        ui.horizontal(|ui| {
            if self.sort.group_by!=GroupBy::None{ui.add_sized([100.0,24.0],egui::Label::new(self.sort.group_by.label())).on_hover_text(self.sort.group_by.tooltip());}
            for (label,key,width) in [("Name",SortKey::Filename,widths[0]),("Date",SortKey::DateTaken,widths[1]),("Size",SortKey::Bytes,widths[2]),("Dimensions",SortKey::Pixels,widths[3]),("Format",SortKey::Format,widths[4]),("Rating",SortKey::Rating,widths[5])]{
                if ui.add_sized([width,24.0],egui::Button::new(label).selected(self.sort.primary==key)).on_hover_text(format!("Sort by {}; click again to reverse. {}",key.label(),key.tooltip())).clicked(){if self.sort.primary==key{self.sort.primary_desc=!self.sort.primary_desc;}else{self.sort.primary=key;}self.submit_search();}
            }
            ui.add_sized([widths[6],24.0],egui::Label::new("Review")).on_hover_text("The recorded review decision");
        });
        egui::ScrollArea::vertical().id_salt("image-table").show_rows(
            ui, 28.0, self.total_results.max(self.photos.len() as i64) as usize,
            |ui, range| {
                if !self.page_loading && !self.facets_loading && !self.page_error
                    && range.end + 20 >= self.photos.len()
                    && (self.photos.len() as i64) < self.total_results {
                    self.page_loading = true;
                    self.request_explore(self.photos.len() as i64, false);
                }
                for index in range {
                    let Some(photo) = self.photos.get(index).cloned() else { continue; };
                    let mut action = None;
                    let response = ui.horizontal(|ui| {
                        if self.sort.group_by != GroupBy::None {
                            ui.add_sized([100.0,24.0], egui::Label::new(photo.image_group.as_deref().unwrap_or("Unknown")).truncate()).on_hover_text(self.sort.group_by.tooltip());
                        }
                        let name = std::path::Path::new(&photo.path).file_name()
                            .unwrap_or_default().to_string_lossy();
                        let cells=[format!("{}{}",if self.selected_ids.contains(&photo.id){"✓ "}else{""},name),photo.capture_day.as_deref().unwrap_or("—").to_owned(),format_bytes(photo.size_bytes),format!("{}×{}",photo.width,photo.height),photo.format.as_deref().unwrap_or("—").to_owned(),format!("{} ★{}",if photo.favorite{"♥"}else{""},photo.rating),photo.review_state.clone()];
                        for (column,text) in cells.iter().enumerate(){ui.allocate_ui_with_layout(egui::vec2(widths[column],24.0),egui::Layout::left_to_right(egui::Align::Center),|ui|{ui.set_min_size(egui::vec2(widths[column],24.0));ui.set_max_width(widths[column]);ui.add(egui::Label::new(text).truncate()).on_hover_text(text);});}
                    }).response.interact(egui::Sense::click());
                    response.clone().on_hover_text(photo_tooltip(&photo));
                    if self.selected_ids.contains(&photo.id){ui.painter().rect_stroke(response.rect.expand(2.0),2.0,egui::Stroke::new(3.0,ui.visuals().selection.bg_fill),egui::StrokeKind::Outside);ui.painter().text(response.rect.right_top(),egui::Align2::RIGHT_TOP,"✓",egui::FontId::proportional(20.0),ui.visuals().selection.stroke.color);}
                    response.context_menu(|ui| {
                        action = Self::image_menu(ui, &photo, &self.settings.editors,self.catalog_panel.context["readonly"].as_bool().unwrap_or(false));
                    });
                    if response.clicked() || response.double_clicked() {
                        action = self.image_click_action(index, response.double_clicked(), ui.ctx());
                    }
                    if let Some(action) = action { self.apply_image_action(&photo, action, ui.ctx()); }
                }
            });
        });
    }

    fn filmstrip_ui(&mut self,ui:&mut egui::Ui) {
        let index=self.selected.unwrap_or(0).min(self.photos.len().saturating_sub(1));
        let Some(photo)=self.photos.get(index).cloned() else {ui.label("No images in this view");return;};
        self.record_displayed(photo.id);
        if index+2>=self.photos.len()&&!self.page_loading&&!self.facets_loading&&!self.page_error&&(self.photos.len() as i64)<self.total_results {self.page_loading=true;self.request_explore(self.photos.len() as i64,false);}
        self.request_full(&photo,2560);self.request_thumbs(&[photo.id]);
        let keys=!ui.ctx().egui_wants_keyboard_input();
        let left=keys&&ui.input(|i|i.key_pressed(egui::Key::ArrowLeft));let right=keys&&ui.input(|i|i.key_pressed(egui::Key::ArrowRight));
        ui.horizontal(|ui|{
            if (ui.add_enabled(index>0,egui::Button::new("←")).on_hover_text("Previous image (Left arrow)").clicked()||left)&&index>0{self.selected=Some(index-1);}
            if (ui.add_enabled(index+1<self.photos.len(),egui::Button::new("→")).on_hover_text("Next image (Right arrow)").clicked()||right)&&index+1<self.photos.len(){self.selected=Some(index+1);}
            ui.add(egui::Label::new(format!("{} / {} · {}",index+1,self.total_results,photo.path)).truncate()).on_hover_text(photo_tooltip(&photo));
            if self.page_loading{ui.spinner();}
        });
        self.decode_error_ui(ui,photo.id,false);
        let mut action=None;
        let available=ui.available_height().clamp(180.0,650.0);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),available-95.0),egui::Layout::top_down(egui::Align::Center),|ui|{
            if let Some(entry)=self.full_images.get_mut(&photo.id).or_else(||self.thumbnails.get_mut(&photo.id)) {
                entry.used=self.frame;let size=entry.texture.size_vec2();let scale=(ui.available_width()/size.x).min(ui.available_height()/size.y).min(1.0);
                let response=ui.add(egui::Image::new((entry.texture.id(),size*scale)).sense(egui::Sense::click())).on_hover_text(photo_tooltip(&photo));
                if response.double_clicked(){action=Some(ImageAction::OpenOriginal);}
                response.context_menu(|ui|{action=Self::image_menu(ui,&photo,&self.settings.editors,self.catalog_panel.context["readonly"].as_bool().unwrap_or(false));});
            }else if !self.failed_full.contains_key(&photo.id){ui.spinner();}
        });
        if let Some(action)=action {self.apply_image_action(&photo,action,ui.ctx());}
        let start=index.saturating_sub(8);let end=(index+9).min(self.photos.len());
        let ids:Vec<_>=self.photos[start..end].iter().map(|p|p.id).collect();self.request_thumbs(&ids);
        let mut strip_action=None;
        egui::ScrollArea::horizontal().show(ui,|ui|{ui.horizontal(|ui|{for next in start..end {
            let photo=self.photos[next].clone();
            if let Some(entry)=self.thumbnails.get_mut(&photo.id){entry.used=self.frame;let size=entry.texture.size_vec2();let scale=72.0/size.x.max(size.y).max(1.0);
                let response=ui.add(egui::Image::new((entry.texture.id(),size*scale)).sense(egui::Sense::click())).on_hover_text(photo_tooltip(&photo));
                if next==index {ui.painter().rect_stroke(response.rect.expand(2.0),2.0,egui::Stroke::new(2.0,ui.visuals().selection.bg_fill),egui::StrokeKind::Outside);}
                if self.selected_ids.contains(&photo.id){ui.painter().text(response.rect.right_top(),egui::Align2::RIGHT_TOP,"✓",egui::FontId::proportional(16.0),ui.visuals().selection.stroke.color);}
                if response.clicked() || response.double_clicked(){
                    strip_action=self.image_click_action(next,response.double_clicked(),ui.ctx()).map(|a|(photo.clone(),a));
                }
                response.context_menu(|ui|{strip_action=Self::image_menu(ui,&photo,&self.settings.editors,self.catalog_panel.context["readonly"].as_bool().unwrap_or(false)).map(|a|(photo.clone(),a));});
            }
        }});});
        if let Some((photo,action))=strip_action{self.apply_image_action(&photo,action,ui.ctx());}
    }

    fn decode_error_ui(&mut self,ui:&mut egui::Ui,id:i64,face:bool) {
        let failed=if face{&mut self.failed_faces}else{&mut self.failed_full};
        if let Some(error)=failed.get(&id).cloned(){ui.colored_label(egui::Color32::LIGHT_RED,error);if ui.small_button("Retry preview").on_hover_text("Retry once after fixing the file or decoder; failures are not retried automatically").clicked(){failed.remove(&id);}}
    }

    fn grid_ui(&mut self, ui: &mut egui::Ui) {
        if self.sort.group_by != GroupBy::None { self.grouped_grid_ui(ui); return; }
        let columns = (ui.available_width() / 184.0).floor().max(1.0) as usize;
        let rows = (self.total_results.max(self.photos.len() as i64) as usize).div_ceil(columns);
        egui::ScrollArea::vertical()
            .id_salt("image-grid")
            .show_rows(ui, 194.0, rows, |ui, range| {
                if !self.page_loading
                    && !self.facets_loading
                    && !self.page_error
                    && (range.end + 4) * columns >= self.photos.len()
                    && (self.photos.len() as i64) < self.total_results
                {
                    self.page_loading = true;
                    self.request_explore(self.photos.len() as i64, false);
                }
                let start = range.start.saturating_sub(3) * columns;
                let end = ((range.end + 3) * columns).min(self.photos.len());
                let ids: Vec<i64> = if start < end {
                    self.photos[start..end].iter().map(|p| p.id).collect()
                } else {
                    Vec::new()
                };
                self.request_thumbs(&ids);
                let upcoming=self.photos[start.min(self.photos.len())..(end+24).min(self.photos.len())].to_vec();
                for photo in upcoming {if self.thumbnails.contains_key(&photo.id){self.request_full(&photo,1024);}}
                for row in range {
                    ui.horizontal(|ui| {
                        for col in 0..columns {
                            let index = row * columns + col;
                            let Some(_) = self.photos.get(index) else {
                                if index < self.total_results as usize {
                                    ui.add_sized([176.0, 194.0], egui::Label::new("Loading…"));
                                }
                                continue;
                            };
                            self.photo_card(ui, index);
                        }
                    });
                }
            });
    }

    fn photo_card(&mut self, ui: &mut egui::Ui, index: usize) {
        let Some(photo) = self.photos.get(index) else { return; };
        let id = photo.id;
        let photo = photo.clone();
        let mut action = None;
        let name = std::path::Path::new(&photo.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        ui.allocate_ui_with_layout(
            egui::vec2(176.0, 194.0),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                ui.set_min_size(egui::vec2(176.0,194.0));
                if let Some(entry) = self.thumbnails.get_mut(&id) {
                    entry.used = self.frame;
                    let size = entry.texture.size_vec2();
                    let scale = 160.0 / size.x.max(size.y).max(1.0);
                    let response = ui
                        .add(
                            egui::Image::new((
                                entry.texture.id(),
                                size * scale,
                            ))
                            .sense(egui::Sense::click()),
                        );
                    response.clone().on_hover_text(photo_tooltip(&photo));
                    if self.selected_ids.contains(&photo.id){ui.painter().rect_stroke(response.rect.expand(2.0),2.0,egui::Stroke::new(3.0,ui.visuals().selection.bg_fill),egui::StrokeKind::Outside);ui.painter().text(response.rect.right_top(),egui::Align2::RIGHT_TOP,"✓",egui::FontId::proportional(20.0),ui.visuals().selection.stroke.color);}
                    response.context_menu(|ui| {
                        action = Self::image_menu(ui, &photo, &self.settings.editors,self.catalog_panel.context["readonly"].as_bool().unwrap_or(false));
                    });
                    if response.clicked() || response.double_clicked() {
                        action = self.image_click_action(index, response.double_clicked(), ui.ctx());
                    }
                }else{
                    let response=ui.add_sized([160.0,160.0],egui::Button::new(if self.missing_thumbs.contains(&id){"No thumbnail"}else{"Loading…"})).on_hover_text(photo_tooltip(&photo));
                    response.context_menu(|ui|{action=Self::image_menu(ui,&photo,&self.settings.editors,self.catalog_panel.context["readonly"].as_bool().unwrap_or(false));});
                    if response.clicked() || response.double_clicked(){action=self.image_click_action(index,response.double_clicked(),ui.ctx());}
                }
                let response=ui.add(egui::Label::new(egui::RichText::new(if self.view_mode == ViewMode::Stacks {
                    format!("{name} · {} copies", photo.duplicate_count)
                } else {format!("{}{}",if self.selected_ids.contains(&id){"✓ "}else{""},name)}).size(11.0)).truncate().sense(egui::Sense::click())).on_hover_text(photo_tooltip(&photo));
                if response.clicked() || response.double_clicked(){action=self.image_click_action(index,response.double_clicked(),ui.ctx());}
                if let Some(names)=&photo.collection_names {
                    ui.add(egui::Label::new(egui::RichText::new(names).size(10.0).color(egui::Color32::LIGHT_BLUE)).truncate()).on_hover_text(format!("Saved collections: {names}"));
                }
            },
        );
        if let Some(action) = action {
            self.apply_image_action(&photo, action, ui.ctx());
        }
    }

    fn grouped_grid_ui(&mut self, ui: &mut egui::Ui) {
        let columns = (ui.available_width()/184.0).floor().max(1.0) as usize;
        let rows = grouped_rows(&self.photos, columns);
        let loading_row = usize::from((self.photos.len() as i64) < self.total_results);
        egui::ScrollArea::vertical().id_salt(("grouped-grid", self.sort.group_by.label()))
            .show_rows(ui, 222.0, rows.len()+loading_row, |ui, range| {
                if range.end+3 >= rows.len() && !self.page_loading && !self.facets_loading && !self.page_error
                    && (self.photos.len() as i64) < self.total_results {
                    self.page_loading = true;
                    self.request_explore(self.photos.len() as i64, false);
                }
                let ids: Vec<_> = rows.iter().skip(range.start.saturating_sub(2)).take(range.len()+4)
                    .flat_map(|row| self.photos[row.start..row.end].iter().map(|p| p.id)).collect();
                self.request_thumbs(&ids);
                for index in range {
                    if let Some(row) = rows.get(index) {
                        ui.add_sized([ui.available_width(), 20.0], egui::Label::new(
                            egui::RichText::new(format!("{}{}", row.label, if row.heading { "" } else { " · continued" })).strong()).truncate())
                            .on_hover_text(format!("{}: {}\n{}\nImages within each group use the selected primary and secondary order.", self.sort.group_by.label(), row.label, self.sort.group_by.tooltip()));
                        ui.horizontal(|ui| { for photo in row.start..row.end { self.photo_card(ui, photo); } });
                    } else { ui.label(if self.page_error { "Could not load the next group" } else { "Loading more groups…" }); }
                }
            });
    }

    fn visual_ui(&mut self, ui: &mut egui::Ui) {
        let previous = self.explore_visual;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.explore_visual, ExploreVisual::Folders, "Folders").on_hover_text("Folder treemap: tile area shows the selected storage/count metric. Hover for totals; click to filter or drill down.");
            ui.selectable_value(&mut self.explore_visual, ExploreVisual::Gps, "GPS map").on_hover_text("Location report: each point covers a 2° area. Hover for coordinates and count; click to filter. Only images with GPS metadata appear.");
            ui.selectable_value(&mut self.explore_visual, ExploreVisual::SizePixels, "Size × pixels").on_hover_text("File-size/resolution report on logarithmic axes. Hover points for ranges and counts; click a bucket or drag to filter.");
            ui.selectable_value(&mut self.explore_visual, ExploreVisual::FolderMonths, "Folder × month").on_hover_text("Monthly activity in the 12 busiest folders for the selected year. Hover cells for dates and counts; click to filter.");
        });
        if self.explore_visual != previous { self.submit_search(); }
        match self.explore_visual {
            ExploreVisual::Folders => self.treemap_ui(ui),
            ExploreVisual::Gps => self.gps_ui(ui),
            ExploreVisual::SizePixels => self.scatter_ui(ui),
            ExploreVisual::FolderMonths => self.folder_months_ui(ui),
        }
    }

    fn gps_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Images with GPS coordinates · click a point to filter its 2° area");
        let (rect, _) = ui.allocate_exact_size(ui.available_size().max(egui::vec2(160.0,120.0)), egui::Sense::hover());
        for longitude in [-120.0_f32, -60.0, 0.0, 60.0, 120.0] {
            let x = rect.left() + ((longitude+180.0)/360.0)*rect.width();
            ui.painter().line_segment([egui::pos2(x,rect.top()),egui::pos2(x,rect.bottom())],
                egui::Stroke::new(1.0,egui::Color32::DARK_GRAY));
        }
        for latitude in [-60.0_f32, -30.0, 0.0, 30.0, 60.0] {
            let y = rect.top() + ((90.0-latitude)/180.0)*rect.height();
            ui.painter().line_segment([egui::pos2(rect.left(),y),egui::pos2(rect.right(),y)],
                egui::Stroke::new(1.0,egui::Color32::DARK_GRAY));
        }
        let mut chosen = None;
        for bin in &self.geo_bins {
            let latitude = bin.lat*2-90;
            let longitude = bin.lon*2-180;
            let x = rect.left() + ((longitude as f32+181.0)/360.0)*rect.width();
            let y = rect.top() + ((89.0-latitude as f32)/180.0)*rect.height();
            let center = egui::pos2(x,y);
            let radius = (bin.count as f32).sqrt().clamp(3.0,15.0);
            let hit = egui::Rect::from_center_size(center,egui::vec2(radius*2.0,radius*2.0));
            let response = ui.interact(hit, ui.id().with((bin.lat,bin.lon)), egui::Sense::click());
            ui.painter().circle_filled(center,radius,egui::Color32::from_rgba_unmultiplied(83,163,230,190));
            response.clone().on_hover_text(format!("{latitude}° to {}° N, {longitude}° to {}° E · {} images",
                latitude+2,longitude+2,bin.count));
            if response.clicked() { chosen = Some((latitude,longitude)); }
        }
        if let Some((lat,lon)) = chosen {
            self.filters.retain(|f|!matches!(f.field.as_str(),"lat_min"|"lat_max"|"lon_min"|"lon_max"|"lat_lt"|"lon_lt"));
            for (field,value) in [("lat_min",lat.to_string()),(if lat+2>=90{"lat_max"}else{"lat_lt"},(lat+2).to_string()),
                ("lon_min",lon.to_string()),(if lon+2>=180{"lon_max"}else{"lon_lt"},(lon+2).to_string())] {
                self.filters.push(FilterChip { field: field.into(), value, exclude: false });
            }
            self.submit_search();
        }
    }

    fn scatter_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("File size (horizontal) × pixel count (vertical), logarithmic · drag to filter");
        let (rect, response) = ui.allocate_exact_size(ui.available_size().max(egui::vec2(160.0,120.0)),
            egui::Sense::click_and_drag());
        let Some(min_x) = self.scatter_bins.iter().map(|bin| bin.bytes_log2).min() else {
            ui.painter().text(rect.center(),egui::Align2::CENTER_CENTER,"No sized images",
                egui::FontId::proportional(14.0),egui::Color32::GRAY);
            return;
        };
        let max_x = self.scatter_bins.iter().map(|bin| bin.bytes_log2).max().unwrap_or(min_x).max(min_x+1);
        let min_y = self.scatter_bins.iter().map(|bin| bin.pixels_log2).min().unwrap_or(0);
        let max_y = self.scatter_bins.iter().map(|bin| bin.pixels_log2).max().unwrap_or(min_y).max(min_y+1);
        let point = |x:i32,y:i32| egui::pos2(
            rect.left()+((x-min_x) as f32+0.5)/(max_x-min_x+1) as f32*rect.width(),
            rect.bottom()-((y-min_y) as f32+0.5)/(max_y-min_y+1) as f32*rect.height());
        for bin in &self.scatter_bins {
            let center = point(bin.bytes_log2,bin.pixels_log2);
            let radius = (bin.count as f32).sqrt().clamp(2.0,12.0);
            ui.painter().circle_filled(center,radius,egui::Color32::from_rgba_unmultiplied(107,207,158,175));
            let pow2 = |value:i32| 1_i64.checked_shl(value.clamp(0,60) as u32).unwrap_or(i64::MAX);
            ui.interact(egui::Rect::from_center_size(center,egui::vec2(radius.max(5.0)*2.0,radius.max(5.0)*2.0)),
                ui.id().with(("scatter-bin",bin.bytes_log2,bin.pixels_log2)), egui::Sense::hover())
                .on_hover_text(format!("{} images\nFile size: {}–{}\nResolution: {}–{} pixels\nClick to filter this bucket, or drag a rectangle to select a range.",
                    bin.count, format_bytes(pow2(bin.bytes_log2)), format_bytes(pow2(bin.bytes_log2+1)-1),
                    pow2(bin.pixels_log2),pow2(bin.pixels_log2+1)-1));
        }
        if response.drag_started() { self.chart_drag = response.interact_pointer_pos(); }
        if let Some(start) = self.chart_drag {
            if let Some(now) = response.interact_pointer_pos() {
                ui.painter().rect_stroke(egui::Rect::from_two_pos(start,now),0.0,
                    egui::Stroke::new(1.0,egui::Color32::WHITE),egui::StrokeKind::Inside);
            }
        }
        if response.drag_stopped() || response.clicked() {
            if let Some(end) = response.interact_pointer_pos() {
                let start = self.chart_drag.take().unwrap_or(end);
                let to_bin = |pos:egui::Pos2| {
                    let x = min_x+(((pos.x-rect.left())/rect.width()).clamp(0.0,0.9999)*(max_x-min_x+1) as f32) as i32;
                    let y = min_y+(((rect.bottom()-pos.y)/rect.height()).clamp(0.0,0.9999)*(max_y-min_y+1) as f32) as i32;
                    (x,y)
                };
                let (a,b) = to_bin(start); let (c,d) = to_bin(end);
                let pow2 = |value:i32| 1_i64.checked_shl(value.clamp(0,60) as u32).unwrap_or(i64::MAX);
                self.filters.retain(|f|!matches!(f.field.as_str(),"size_min"|"size_max"|"pixels_min"|"pixels_max"));
                for (field,value) in [("size_min",pow2(a.min(c)).to_string()),
                    ("size_max",(pow2(a.max(c)+1)-1).to_string()),
                    ("pixels_min",pow2(b.min(d)).to_string()),
                    ("pixels_max",(pow2(b.max(d)+1)-1).to_string())] {
                    self.filters.push(FilterChip { field: field.into(), value, exclude: false });
                }
                self.submit_search();
            }
        }
    }

    fn folder_months_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.calendar_year>1,egui::Button::new("Previous year").small()).on_hover_text("Show the previous year in the monthly report.").clicked() { self.calendar_year -= 1; self.submit_search(); }
            ui.strong(format!("{} · 12 busiest folders", self.calendar_year));
            if ui.add_enabled(self.calendar_year<9999,egui::Button::new("Next year").small()).on_hover_text("Show the next year in the monthly report.").clicked() { self.calendar_year += 1; self.submit_search(); }
        });
        if self.facets_loading{ui.spinner();ui.label("Loading folder/month counts…");return;}
        if self.folder_months.is_empty() { ui.label("No dated images in this year"); return; }
        let mut folder_counts = HashMap::<String,i64>::new();
        let mut cells = HashMap::<(String,u32),i64>::new();
        let mut max_count = 1_i64;
        for row in &self.folder_months {
            *folder_counts.entry(row.folder.clone()).or_default() += row.count;
            cells.insert((row.folder.clone(),row.month),row.count);
            max_count = max_count.max(row.count);
        }
        let mut folders: Vec<_> = folder_counts.into_iter().collect();
        folders.sort_unstable_by(|a,b| b.1.cmp(&a.1));
        let mut chosen = None;
        egui::ScrollArea::both().id_salt("folder-month-report").show(ui, |ui| {
            egui::Grid::new("folder-months").spacing([3.0,3.0]).show(ui, |ui| {
                ui.label("Folder").on_hover_text("Folders ranked by image count in the selected year.");
                for month in 1..=12 { ui.small(format!("{month:02}")).on_hover_text(format!("Month {month} of {}. Cell brightness indicates image count.",self.calendar_year)); }
                ui.end_row();
                for (folder,total) in folders.iter().take(12) {
                    let label = std::path::Path::new(folder).file_name()
                        .unwrap_or_default().to_string_lossy();
                    ui.label(format!("{} ({})",label,total)).on_hover_text(folder);
                    for month in 1..=12 {
                        let count = *cells.get(&(folder.clone(),month)).unwrap_or(&0);
                        let intensity = ((count as f32/max_count as f32).sqrt()*180.0) as u8;
                        let color = egui::Color32::from_rgb(35,55+intensity/2,65+intensity);
                        let response = ui.add_sized([27.0,25.0],egui::Button::new(if count>0 { count.to_string() } else { String::new() }).fill(color));
                        response.clone().on_hover_text(format!("{folder} · {}-{month:02} · {count} images",self.calendar_year));
                        if response.clicked() && count>0 { chosen = Some((folder.clone(),month)); }
                    }
                    ui.end_row();
                }
            });
        });
        if let Some((folder,month)) = chosen {
            if let Some(start) = NaiveDate::from_ymd_opt(self.calendar_year,month,1) {
                let end = start.checked_add_months(Months::new(1))
                    .and_then(|day| day.pred_opt()).unwrap_or(start);
                self.map_root = None;
                self.filters.retain(|f|f.field!="folder_direct");
                self.filters.push(FilterChip{field:"folder_direct".into(),value:folder,exclude:false});
                self.from_day = Some(start.to_string());
                self.to_day = Some(end.to_string());
                self.submit_search();
            }
        }
    }

    fn treemap_ui(&mut self, ui: &mut egui::Ui) {
        let previous_metric = self.map_metric;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Folders").on_hover_text("Folder totals for the current filters. Larger tiles represent more of the selected metric.");
            if self.facets_loading {
                ui.spinner();
            }
            for (metric, label) in [
                (visuals::Metric::Bytes, "Bytes"),
                (visuals::Metric::Count, "Image count"),
                (visuals::Metric::UniqueBytes, "Unique bytes"),
                (visuals::Metric::DuplicateWaste, "Duplicate waste"),
            ] {
                if ui.selectable_label(self.map_metric == metric, label).on_hover_text(match metric { visuals::Metric::Count => "Number of images matching the current filters.", visuals::Metric::Bytes => "Total original-file bytes of matching images.", visuals::Metric::UniqueBytes => "Bytes after counting each content hash once within a folder.", visuals::Metric::DuplicateWaste => "Extra bytes used by exact copies within a folder." }).clicked() {
                    self.map_metric = metric;
                    self.refresh_map_tiles();
                }
            }
        });
        if self.map_metric != previous_metric
            && matches!(self.map_metric, visuals::Metric::UniqueBytes | visuals::Metric::DuplicateWaste)
            && !matches!(previous_metric, visuals::Metric::UniqueBytes | visuals::Metric::DuplicateWaste) {
            self.submit_search();
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("All").clicked() {
                self.map_root = None;
                self.refresh_map_tiles();
                self.submit_search();
            }
            if let Some(root) = self.map_root.clone() {
                let ancestors: Vec<_> = std::path::Path::new(&root)
                    .ancestors()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                for path in ancestors.into_iter().rev() {
                    let label = std::path::Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    if ui.small_button(label).on_hover_text(&path).clicked() && path != root {
                        self.map_root = Some(path);
                        self.refresh_map_tiles();
                        self.submit_search();
                    }
                }
            }
        });
        let available = egui::vec2(
            ui.available_width().max(100.0),
            ui.available_height().max(60.0),
        );
        let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
        if self.map_tiles.is_empty() {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No folders in these results",
                egui::FontId::proportional(14.0),
                egui::Color32::GRAY,
            );
            return;
        }
        let mut chosen = None;
        for (tile, tile_rect) in visuals::layout(&self.map_tiles, rect, self.map_metric) {
            let tile_rect = tile_rect.shrink(1.0);
            let tile_id = match &tile.kind {
                visuals::TileKind::Folder(path) => ui.id().with(path),
                visuals::TileKind::Direct => ui.id().with("direct-images"),
                visuals::TileKind::Other => ui.id().with("other-folders"),
            };
            let response = ui.interact(tile_rect, tile_id, egui::Sense::click());
            let mut hash = 0u32;
            for byte in tile.label.as_bytes() {
                hash = hash.wrapping_mul(33).wrapping_add(*byte as u32);
            }
            let lift = if response.hovered() { 28 } else { 0 };
            let color = egui::Color32::from_rgb(
                (45 + hash % 52) as u8 + lift,
                (80 + (hash >> 5) % 65) as u8 + lift,
                (110 + (hash >> 12) % 65) as u8 + lift,
            );
            ui.painter().rect_filled(tile_rect, 3.0, color);
            if tile_rect.width() > 60.0 && tile_rect.height() > 27.0 {
                let title = if tile.label.chars().count() > 28 {
                    format!("{}…", tile.label.chars().take(27).collect::<String>())
                } else {
                    tile.label.clone()
                };
                ui.painter().text(
                    tile_rect.left_top() + egui::vec2(6.0, 5.0),
                    egui::Align2::LEFT_TOP,
                    title,
                    egui::FontId::proportional(12.0),
                    egui::Color32::WHITE,
                );
                if tile_rect.height() > 48.0 {
                    ui.painter().text(
                        tile_rect.left_top() + egui::vec2(6.0, 24.0),
                        egui::Align2::LEFT_TOP,
                        if self.map_metric == visuals::Metric::Count {
                            format!("{} images", tile.count)
                        } else {
                            format!("{} images · {}", tile.count, format_metric(self.map_metric, &tile))
                        },
                        egui::FontId::proportional(10.0),
                        egui::Color32::LIGHT_GRAY,
                    );
                }
            }
            let _ = response.clone().on_hover_text(format!(
                "{}\n{} images · {}",
                tile.label,
                tile.count,
                if self.map_metric == visuals::Metric::Count {
                    String::new()
                } else { format_metric(self.map_metric, &tile) }
            ));
            if response.clicked() {
                chosen = Some(tile.kind);
            }
        }
        if let Some(chosen) = chosen {
            match chosen {
                visuals::TileKind::Folder(path) => {
                    self.map_root = Some(path);
                    self.refresh_map_tiles();
                    self.submit_search();
                }
                visuals::TileKind::Other => self.show_other = true,
                visuals::TileKind::Direct => {}
            }
        }
        if self.show_other {
            let mut open = true;
            let mut picked = None;
            let others = self
                .folder_tree
                .other(self.map_root.as_deref(), self.map_metric);
            if !others.is_empty() {
                egui::Window::new("Other folders")
                    .open(&mut open)
                    .show(ui.ctx(), |ui| {
                        egui::ScrollArea::vertical().max_height(400.0).show_rows(
                            ui,
                            26.0,
                            others.len(),
                            |ui, range| {
                                for tile in &others[range] {
                                    if ui
                                        .button(format!("{} · {} images", tile.label, tile.count))
                                        .clicked()
                                    {
                                        if let visuals::TileKind::Folder(path) = &tile.kind {
                                            picked = Some(path.clone());
                                        }
                                    }
                                }
                            },
                        );
                    });
            }
            self.show_other = open && picked.is_none();
            if let Some(path) = picked {
                self.map_root = Some(path);
                self.refresh_map_tiles();
                self.submit_search();
            }
        }
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Timeline").on_hover_text("Image counts by camera date or file-date fallback. Hover a bar for totals; click or drag to filter dates.");
            if ui.selectable_label(self.show_calendar, "Calendar").on_hover_text("Switch between timeline bars and a calendar heatmap.").clicked() {
                self.show_calendar = !self.show_calendar;
            }
            if self.facets_loading {
                ui.spinner();
            }
            if let (Some(from), Some(to)) = (&self.from_day, &self.to_day) {
                ui.label(format!("{from} – {to}"));
                if ui.small_button("Clear dates").on_hover_text("Remove the date range while keeping other filters.").clicked() {
                    self.from_day = None;
                    self.to_day = None;
                    self.submit_search();
                }
            }
            ui.colored_label(egui::Color32::from_rgb(87, 158, 217), "■ Camera date").on_hover_text("Date read from camera/EXIF metadata.");
            ui.colored_label(egui::Color32::from_rgb(175, 174, 158), "■ File date").on_hover_text("Filesystem modification date used when capture metadata is missing.");
        });
        if self.show_calendar { self.calendar_ui(ui); return; }
        let daily = match (&self.from_day, &self.to_day) {
            (Some(from), Some(to)) => NaiveDate::parse_from_str(from, "%Y-%m-%d")
                .ok()
                .zip(NaiveDate::parse_from_str(to, "%Y-%m-%d").ok())
                .is_some_and(|(start, end)| (end - start).num_days() <= 62),
            _ => false,
        };
        let mut bins = std::collections::BTreeMap::<String, (i64, i64)>::new();
        for stat in &self.days {
            let key = if daily {
                stat.day.clone()
            } else {
                stat.day.get(..7).unwrap_or(&stat.day).to_string()
            };
            let entry = bins.entry(key).or_default();
            entry.0 += stat.count;
            entry.1 += stat.fallback_count;
        }
        if let (Some(first), Some(last)) = (bins.first_key_value(), bins.last_key_value()) {
            let first_key = first.0.clone();
            let last_key = last.0.clone();
            let parse = |key: &str| {
                NaiveDate::parse_from_str(
                    &if daily {
                        key.to_string()
                    } else {
                        format!("{key}-01")
                    },
                    "%Y-%m-%d",
                )
                .ok()
            };
            if let Some(mut day) = parse(&first_key) {
                if let Some(end) = parse(&last_key) {
                    for _ in 0..1200 {
                        if day > end {
                            break;
                        }
                        bins.entry(if daily {
                            day.to_string()
                        } else {
                            day.format("%Y-%m").to_string()
                        })
                        .or_default();
                        let Some(next) = (if daily {
                            day.succ_opt()
                        } else {
                            day.checked_add_months(Months::new(1))
                        }) else {
                            break;
                        };
                        day = next;
                    }
                }
            }
        }
        let bins: Vec<_> = bins.into_iter().collect();
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(100.0), 112.0),
            egui::Sense::click_and_drag(),
        );
        if bins.is_empty() {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No dated images in these results",
                egui::FontId::proportional(13.0),
                egui::Color32::GRAY,
            );
            return;
        }
        let max_count = bins
            .iter()
            .map(|(_, (count, _))| *count)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let bar_width = rect.width() / bins.len() as f32;
        for (index, (_, (count, fallback))) in bins.iter().enumerate() {
            let height = (*count as f32 / max_count) * (rect.height() - 22.0);
            let fallback_height = (*fallback as f32 / max_count) * (rect.height() - 22.0);
            let left = rect.left() + index as f32 * bar_width;
            let bar = egui::Rect::from_min_max(
                egui::pos2(left + (bar_width*0.15).min(1.0), rect.bottom() - 20.0 - height + fallback_height),
                egui::pos2(left + bar_width - (bar_width*0.15).min(1.0), rect.bottom() - 20.0),
            );
            ui.painter()
                .rect_filled(bar, 1.0, egui::Color32::from_rgb(87, 158, 217));
            if *fallback > 0 {
                let fallback_bar = egui::Rect::from_min_max(
                    egui::pos2(left + (bar_width*0.15).min(1.0), rect.bottom() - 20.0 - height),
                    egui::pos2(
                        left + bar_width - (bar_width*0.15).min(1.0),
                        rect.bottom() - 20.0 - height + fallback_height,
                    ),
                );
                ui.painter()
                    .rect_filled(fallback_bar, 1.0, egui::Color32::from_rgb(175, 174, 158));
            }
        }
        ui.painter().text(
            rect.left_bottom() - egui::vec2(0.0, 2.0),
            egui::Align2::LEFT_BOTTOM,
            &bins[0].0,
            egui::FontId::proportional(11.0),
            egui::Color32::GRAY,
        );
        ui.painter().text(
            rect.right_bottom() - egui::vec2(0.0, 2.0),
            egui::Align2::RIGHT_BOTTOM,
            &bins[bins.len() - 1].0,
            egui::FontId::proportional(11.0),
            egui::Color32::GRAY,
        );
        if let Some(pos) = response.hover_pos().or_else(||response.interact_pointer_pos()) {
            let index = (((pos.x - rect.left()) / bar_width).floor() as usize).min(bins.len() - 1);
            let _ = response.clone().on_hover_text(format!(
                "{} · {} images ({} file dates)",
                bins[index].0, bins[index].1.0, bins[index].1.1
            ));
            if response.drag_started() {
                self.timeline_drag = Some(index);
            }
            if response.clicked() || response.drag_stopped() {
                let anchor = self.timeline_drag.take().unwrap_or(index);
                let start = anchor.min(index);
                let end = anchor.max(index);
                let (from, to) = if daily {
                    (bins[start].0.clone(), bins[end].0.clone())
                } else {
                    let first =
                        NaiveDate::parse_from_str(&format!("{}-01", bins[start].0), "%Y-%m-%d")
                            .ok();
                    let last =
                        NaiveDate::parse_from_str(&format!("{}-01", bins[end].0), "%Y-%m-%d").ok();
                    match first.zip(last) {
                        Some((first, last)) => (
                            first.to_string(),
                            last.checked_add_months(Months::new(1))
                                .and_then(|day| day.pred_opt())
                                .unwrap_or(last)
                                .to_string(),
                        ),
                        None => return,
                    }
                };
                self.from_day = Some(from);
                self.to_day = Some(to);
                self.submit_search();
            }
        }
    }

    fn calendar_ui(&mut self, ui: &mut egui::Ui) {
        let previous_year=self.calendar_year;
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::DragValue::new(&mut self.calendar_year).range(1..=9999)).on_hover_text("Jump directly to a calendar year");
            ui.add(egui::DragValue::new(&mut self.calendar_month).range(1..=12)).on_hover_text("Jump directly to a calendar month");
            if ui.small_button("Today").on_hover_text("Return to the current month").clicked(){self.calendar_year=Local::now().year();self.calendar_month=Local::now().month();}
            if ui.add_enabled(self.calendar_year>1||self.calendar_month>1,egui::Button::new("Previous month").small()).on_hover_text("Show the previous calendar month.").clicked() {
                if self.calendar_month == 1 { self.calendar_month = 12; self.calendar_year -= 1; }
                else { self.calendar_month -= 1; }
            }
            ui.strong(format!("{}-{:02}", self.calendar_year, self.calendar_month));
            if ui.add_enabled(self.calendar_year<9999||self.calendar_month<12,egui::Button::new("Next month").small()).on_hover_text("Show the next calendar month.").clicked() {
                if self.calendar_month == 12 { self.calendar_month = 1; self.calendar_year += 1; }
                else { self.calendar_month += 1; }
            }
        });
        if self.calendar_year!=previous_year{self.folder_months.clear();self.refresh_search();}
        let Some(first) = NaiveDate::from_ymd_opt(self.calendar_year, self.calendar_month, 1) else { return; };
        let next = first.checked_add_months(Months::new(1)).unwrap_or(first);
        let days = (next - first).num_days().max(1) as u32;
        let counts: HashMap<&str, (i64, i64)> = self.days.iter()
            .map(|stat| (stat.day.as_str(), (stat.count, stat.fallback_count))).collect();
        let max_count = self.days.iter().map(|stat| stat.count).max().unwrap_or(1).max(1) as f32;
        let mut picked = None;
        egui::Grid::new("calendar-grid").num_columns(7).spacing([4.0, 2.0]).show(ui, |ui| {
            for name in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] { ui.strong(name); }
            ui.end_row();
            let offset = first.weekday().num_days_from_monday();
            for _ in 0..offset { ui.label(""); }
            for day in 1..=days {
                let date = NaiveDate::from_ymd_opt(self.calendar_year, self.calendar_month, day).unwrap();
                let key = date.to_string();
                let (count, fallback) = counts.get(key.as_str()).copied().unwrap_or_default();
                let intensity = ((count as f32 / max_count).sqrt()*170.0) as u8;
                let color = egui::Color32::from_rgb(35, 65 + intensity/2, 80 + intensity);
                let response = ui.add_sized([36.0, 23.0], egui::Button::new(day.to_string()).fill(color));
                response.clone().on_hover_text(format!("{key}: {count} images ({fallback} file dates)"));
                if response.clicked() { picked = Some(key); }
                if (offset + day) % 7 == 0 { ui.end_row(); }
            }
        });
        if let Some(day) = picked {
            self.from_day = Some(day.clone()); self.to_day = Some(day);
            self.submit_search();
        }
    }

    fn people_ui(&mut self,ui:&mut egui::Ui) {
        ui.horizontal_wrapped(|ui|{
            if let Some(snapshot)=self.face_undo.as_ref(){if ui.add_enabled(!self.readonly(),egui::Button::new("Undo removed detections")).on_hover_text("Restore the last removed face records and assignments; available until this window changes database or closes").clicked(){let _=self.db_tx.send(db::Request::FaceRestore(snapshot.clone()));}}
            ui.heading("People").on_hover_text("Face groups retain stable person IDs. Inspect all faces to correct mixed groups.");
            if ui.button("Manage identities").on_hover_text("Rename, merge, ignore, and restore ignored people").clicked(){self.people_manager=true;let _=self.db_tx.send(db::Request::PeopleAll);}
        });
        let readonly=self.readonly();
        if self.groups_loading{ui.horizontal(|ui|{ui.spinner();ui.label("Loading face groups…");});}
        if self.groups_error || (self.groups.is_empty()&&!self.groups_loading){
            ui.label(if self.groups_error {"Face groups could not be loaded."}else{"No reviewable face groups. Mine faces first, or restore ignored identities in Manage identities."});
            if ui.button("Refresh face groups").on_hover_text("Retry loading groups without changing assignments").clicked(){self.groups_loading=true;self.groups_error=false;let _=self.db_tx.send(db::Request::Groups);}
        }
        egui::ScrollArea::vertical().show_rows(ui,122.0,self.groups.len(),|ui,range|{for index in range {
            let group=self.groups[index].clone();
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),122.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_height(122.0);
            ui.horizontal_wrapped(|ui|{
                ui.label(format!("#{} · {} faces · {}",group.id,group.count,group.person.as_deref().unwrap_or("Unassigned"))).on_hover_text(format!("Assigned person ID: {:?}\nSuggested person ID: {:?}",group.person_id,group.suggested_person_id));
                if ui.button("Review all faces").on_hover_text("Inspect every group member, reassign or split selected faces, and remove false detections").clicked(){self.group_detail=Some(group.id);self.group_loading=true;self.group_faces.clear();self.selected_faces.clear();let _=self.db_tx.send(db::Request::GroupFaces{group:group.id,offset:0});}
                if ui.add_enabled(!readonly,egui::Button::new("Ignore person")).on_hover_text("Hide this identity from tagging; recover it in Manage identities").clicked(){let _=self.db_tx.send(db::Request::IgnoreGroup(group.id));}
                if ui.add_enabled(!readonly&&!self.tag_saving,egui::Button::new("Tag")).on_hover_text("Choose an existing stable person ID or create a name").clicked(){self.tag_group=Some(group.id);self.tag_name=group.person.clone().unwrap_or_default();self.tag_person=group.person_id.or(group.suggested_person_id);self.tag_saving=false;}
            });
            ui.horizontal_wrapped(|ui|{for sample in &group.samples {
                self.request_face(sample);
                if let Some(face)=self.face_images.get_mut(&sample.face_id){face.used=self.frame;let size=face.texture.size_vec2();let response=ui.add(egui::Image::new((face.texture.id(),size*(72.0/size.x.max(size.y)))).sense(egui::Sense::click())).on_hover_text(format!("Face #{} · image #{}\nClick to view {}",sample.face_id,sample.image_id,sample.path));
                    if response.clicked(){self.requested_open=Some(sample.image_id);let _=self.db_tx.send(db::Request::LookupImage(sample.image_id));}
                }else if self.failed_faces.contains_key(&sample.face_id){self.decode_error_ui(ui,sample.face_id,true);}else{ui.spinner();}
            }});
            ui.separator();
        });
        }});
        if let Some(group_id)=self.tag_group {
            let mut tag_open=true;egui::Window::new(format!("Name group #{group_id}")).open(&mut tag_open).max_height((ui.ctx().content_rect().height()-70.0).max(150.0)).vscroll(true).show(ui.ctx(),|ui|{
                egui::ComboBox::from_id_salt("tag_person").selected_text(self.known_people.iter().find(|p|Some(p.id)==self.tag_person).map(|p|format!("{} (#{})",p.name,p.id)).unwrap_or_else(||"New person".into())).show_ui(ui,|ui|{
                    ui.selectable_value(&mut self.tag_person,None,"New person");for person in &self.known_people {if !person.ignored{ui.selectable_value(&mut self.tag_person,Some(person.id),format!("{} (#{})",person.name,person.id)).on_hover_text("Reuse this person ID; recognition samples are retained");}}
                }).response.on_hover_text("Select an existing identity or create a new one");
                if self.tag_person.is_none(){ui.text_edit_singleline(&mut self.tag_name).on_hover_text("Name for a new stable person ID");}
                if ui.add_enabled(!readonly&&!self.tag_saving&&(self.tag_person.is_some()||!self.tag_name.trim().is_empty()),egui::Button::new("Save person")).on_hover_text("Save the assignment; the dialog remains open if it fails").clicked(){self.tag_saving=true;let _=self.db_tx.send(db::Request::Tag{group_id,name:self.tag_name.clone(),person_id:self.tag_person});}
                if self.tag_saving{ui.spinner();}ui.label(&self.message);
                if ui.button("Cancel").on_hover_text("Close this dialog; an already submitted assignment continues in the background").clicked(){self.tag_group=None;}
            });
            if !tag_open{self.tag_group=None;}
        }
        self.identity_manager_ui(ui.ctx());
        self.face_review_ui(ui.ctx());
    }

    fn identity_manager_ui(&mut self,ctx:&egui::Context){
        if !self.people_manager{return;}
        let readonly=self.readonly();let mut open=true;let mut command=None;
        egui::Window::new("Manage identities").open(&mut open).default_size([600.0,450.0]).max_height((ctx.content_rect().height()-70.0).max(160.0)).vscroll(true).show(ctx,|ui|{
            ui.add(egui::TextEdit::singleline(&mut self.identity_search).hint_text("Find an identity by name or ID")).on_hover_text("Filter identities, including ignored people");
            egui::ScrollArea::vertical().id_salt("identity-list").max_height((ui.available_height()-160.0).clamp(80.0,250.0)).show(ui,|ui|{for person in self.known_people.clone().into_iter().filter(|p|self.identity_search.is_empty()||format!("{} {}",p.id,p.name).to_lowercase().contains(&self.identity_search.to_lowercase())){ui.horizontal_wrapped(|ui|{
                ui.add(egui::Label::new(format!("#{} · {}{}",person.id,person.name,if person.ignored{" · ignored"}else{""})).truncate()).on_hover_text(&person.name);
                if ui.add_enabled(!readonly,egui::Button::new("Rename / merge")).on_hover_text("Correct this identity name or merge its samples into another stable ID").clicked(){self.person_edit=Some(person.id);self.person_name=person.name.clone();self.person_merge=None;}
                if ui.add_enabled(!readonly,egui::Button::new(if person.ignored{"Unignore"}else{"Ignore"})).on_hover_text("Change whether this identity is offered for tagging").clicked(){command=Some(db::Request::PersonIgnore{id:person.id,ignored:!person.ignored});}
            });}});
            if let Some(id)=self.person_edit {
                ui.separator();ui.text_edit_singleline(&mut self.person_name).on_hover_text("Editable person name; renaming keeps the ID");
                if ui.add_enabled(!readonly&&!self.person_name.trim().is_empty(),egui::Button::new("Save name")).on_hover_text("Rename this person without changing their samples").clicked(){command=Some(db::Request::PersonRename{id,name:self.person_name.clone()});}
                egui::ComboBox::from_id_salt("person-merge-target").selected_text(self.known_people.iter().find(|p|Some(p.id)==self.person_merge).map(|p|p.name.as_str()).unwrap_or("Merge into…")).show_ui(ui,|ui|{for person in &self.known_people {if person.id!=id&&!person.ignored{ui.selectable_value(&mut self.person_merge,Some(person.id),format!("{} (#{})",person.name,person.id)).on_hover_text(format!("Merge samples into {} (#{})",person.name,person.id));}}}).response.on_hover_text("Choose the stable identity that should retain these recognition samples");
                if ui.add_enabled(!readonly&&self.person_merge.is_some(),egui::Button::new("Merge identities")).on_hover_text("Move all samples to the target identity and remove the duplicate identity").clicked(){self.person_confirmation=Some(db::Request::PersonMerge{source:id,target:self.person_merge.unwrap()});}
            }
            ui.label(&self.message);
        });
        if let Some(command)=command{let _=self.db_tx.send(command);}self.people_manager=open;
    }

    fn face_review_ui(&mut self,ctx:&egui::Context){
        let Some(group)=self.group_detail else{return;};let readonly=self.readonly();let mut open=true;let mut command=None;
        egui::Window::new(format!("Review group #{group}")).open(&mut open).default_size([700.0,600.0]).max_height((ctx.content_rect().height()-70.0).max(160.0)).vscroll(true).show(ctx,|ui|{
            ui.label(format!("{} loaded faces · {} selected",self.group_faces.len(),self.selected_faces.len())).on_hover_text("Load more to inspect every sample; select individual faces for correction");
            ui.horizontal_wrapped(|ui|{
                egui::ComboBox::from_id_salt("face-reassign-target").selected_text(self.known_people.iter().find(|p|Some(p.id)==self.face_target).map(|p|p.name.as_str()).unwrap_or("Choose person")).show_ui(ui,|ui|{for p in &self.known_people{if !p.ignored{ui.selectable_value(&mut self.face_target,Some(p.id),format!("{} (#{})",p.name,p.id));}}}).response.on_hover_text("Assign selected faces to this existing person ID");
                let enabled=!readonly&&!self.selected_faces.is_empty();
                if ui.add_enabled(enabled&&self.face_target.is_some(),egui::Button::new("Reassign selected")).on_hover_text("Move just the selected faces into a separate group assigned to this identity").clicked(){command=Some(db::Request::FaceMove{ids:self.selected_faces.iter().copied().collect(),person_id:self.face_target});}
                if ui.add_enabled(enabled,egui::Button::new("Split selected")).on_hover_text("Create a new unassigned group from these faces, ready for tagging").clicked(){command=Some(db::Request::FaceMove{ids:self.selected_faces.iter().copied().collect(),person_id:None});}
                if ui.add_enabled(enabled,egui::Button::new("Remove false detections")).on_hover_text("Remove these face records; source images are preserved").clicked(){self.person_confirmation=Some(db::Request::FaceReject(self.selected_faces.iter().copied().collect()));}
            });
            egui::ScrollArea::vertical().id_salt("review-faces").max_height((ui.available_height()-65.0).clamp(60.0,300.0)).show_rows(ui,92.0,self.group_faces.len(),|ui,range|{for index in range {
                let sample=self.group_faces[index].clone();self.request_face(&sample);
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),92.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_height(92.0);
                ui.horizontal_wrapped(|ui|{
                    let mut selected=self.selected_faces.contains(&sample.face_id);if ui.checkbox(&mut selected,format!("Face #{}",sample.face_id)).on_hover_text("Select this face for individual correction").changed(){if selected{self.selected_faces.insert(sample.face_id);}else{self.selected_faces.remove(&sample.face_id);}}
                    if let Some(face)=self.face_images.get_mut(&sample.face_id){face.used=self.frame;let size=face.texture.size_vec2();ui.image((face.texture.id(),size*(72.0/size.x.max(size.y)))).on_hover_text(format!("Face #{} · {}",sample.face_id,sample.path));}else{self.decode_error_ui(ui,sample.face_id,true);}
                    if ui.button(format!("Image #{}",sample.image_id)).on_hover_text(&sample.path).clicked(){self.requested_open=Some(sample.image_id);let _=self.db_tx.send(db::Request::LookupImage(sample.image_id));}
                });
            });
        }});
            if self.group_loading {ui.spinner();} if self.group_more&&ui.add_enabled(!self.group_loading,egui::Button::new("Load next 100 faces")).on_hover_text("Load the next group members without changing assignments").clicked(){let _=self.db_tx.send(db::Request::GroupFaces{group,offset:self.group_faces.len() as i64});self.group_loading=true;}
            ui.label(&self.message);
        });
        if let Some(command)=command{let _=self.db_tx.send(command);}
        if !open{self.group_detail=None;self.group_faces.clear();self.selected_faces.clear();}
    }

    fn request_proposals(&mut self,append:bool){
        let after=if append{self.proposals.last().map(|p|p.id)}else{None};
        self.proposal_loading=true;self.proposal_error=false;if !append{self.proposals.clear();self.proposal_more=false;}
        let _=self.db_tx.send(db::Request::ProposalPage{status:self.proposal_status.clone(),search:self.proposal_search.clone(),after});
    }
    fn proposals_ui(&mut self,ui:&mut egui::Ui){
        ui.heading("Sort proposals").on_hover_text("Review proposed destinations; approval records a decision without moving originals");
        let mut changed=false;
        ui.horizontal_wrapped(|ui|{
            egui::ComboBox::from_id_salt("proposal-state").selected_text(if self.proposal_status.is_empty(){"All statuses"}else{&self.proposal_status}).show_ui(ui,|ui|{for status in ["pending","approved","rejected",""]{changed|=ui.selectable_value(&mut self.proposal_status,status.into(),if status.is_empty(){"All statuses"}else{status}).changed();}}).response.on_hover_text("Filter the review queue by decision status");
            changed|=ui.add(egui::TextEdit::singleline(&mut self.proposal_search).hint_text("Search source, destination, reason")).on_hover_text("Search the entire proposal queue").changed();
            if ui.button("Refresh").on_hover_text("Reload newest matching proposals").clicked(){changed=true;}
        });
        if changed{self.request_proposals(false);}
        let readonly=self.readonly();let mut inspect=None;let mut decision=None;
        egui::ScrollArea::vertical().id_salt("proposal-list").max_height((ui.available_height()-42.0).max(40.0)).show_rows(ui,90.0,self.proposals.len(),|ui,range|{for index in range {
            let p=self.proposals[index].clone();self.request_thumbs(&[p.image_id]);
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),90.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_height(90.0);
            ui.horizontal_wrapped(|ui|{
                if let Some(entry)=self.thumbnails.get_mut(&p.image_id){entry.used=self.frame;let size=entry.texture.size_vec2();ui.image((entry.texture.id(),size*(70.0/size.x.max(size.y)))).on_hover_text(&p.source);}
                ui.vertical(|ui|{
                    ui.add(egui::Label::new(format!("{} → {}",p.source,p.destination)).truncate()).on_hover_text(format!("{}\n{}",p.source,p.destination));
                    ui.add(egui::Label::new(format!("{} · {}",p.status,p.reason)).truncate()).on_hover_text("Recorded status and proposal reason");
                    ui.horizontal_wrapped(|ui|{
                        if ui.button("Inspect image").on_hover_text("Open this image and its properties").clicked(){inspect=Some(p.image_id);}
                        if ui.add_enabled(!readonly,egui::Button::new("Approve")).on_hover_text("Record approval without moving the file").clicked(){decision=Some((p.id,true));}
                        if ui.add_enabled(!readonly,egui::Button::new("Reject")).on_hover_text("Record rejection").clicked(){decision=Some((p.id,false));}
                    });
                });
            });
        });
        }});
        if self.proposal_loading{ui.horizontal(|ui|{ui.spinner();ui.label("Loading proposals…");});}
        else if self.proposal_error{ui.label("Proposal request failed. Use Refresh to retry.");}
        else if self.proposals.is_empty(){ui.label("No proposals match this queue. Use Propose move from an image to record one.");}
        if self.proposal_more&&ui.add_enabled(!self.proposal_loading,egui::Button::new("Load next 100 proposals")).on_hover_text("Load more results in stable newest-ID order").clicked(){self.request_proposals(true);}
        if let Some(id)=inspect{self.requested_open=Some(id);self.requested_details=Some(id);let _=self.db_tx.send(db::Request::LookupImage(id));let _=self.db_tx.send(db::Request::Details(id));}
        if let Some((id,approved))=decision{let _=self.db_tx.send(db::Request::Review{id,approved});}
    }

    fn action_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(photo) = self.dialog_photo.clone() else { return; };
        let mut open = true;

        egui::Window::new(if self.dialog_kind == Some("tag") { "Add image tag" } else { "Propose image move" })
            .open(&mut open).show(ctx, |ui| {
                ui.label(&photo.path);if self.dialog_saving{ui.spinner();}ui.label(&self.message);
                if self.dialog_kind == Some("tag") {
                    ui.add(egui::TextEdit::singleline(&mut self.dialog_tag).hint_text("Tag")).on_hover_text("Image tag, up to 80 Unicode characters");
                    if ui.add_enabled(!self.readonly()&&!self.dialog_saving&&!self.dialog_tag.trim().is_empty()&&self.dialog_tag.trim().chars().count()<=80,egui::Button::new("Add tag")).on_hover_text("Save a tag of up to 80 characters; failed saves retain your input").clicked() {
                        let _ = self.db_tx.send(db::Request::AddTag { id: photo.id,
                            tag: self.dialog_tag.trim().to_owned() });
                        self.dialog_saving=true;
                    }
                } else {
                    ui.add(egui::TextEdit::singleline(&mut self.dialog_destination).hint_text("Destination file path")).on_hover_text("Proposed absolute destination filename; recording does not move the original");
                    ui.add(egui::TextEdit::singleline(&mut self.dialog_reason).hint_text("Reason")).on_hover_text("Explain why this destination is proposed");
                    if ui.add_enabled(!self.readonly()&&!self.dialog_saving&&!self.dialog_destination.trim().is_empty(),egui::Button::new("Record proposal")).on_hover_text("Record a destination for review without moving the source file").clicked() {
                        let _ = self.db_tx.send(db::Request::ProposeMove { id: photo.id,
                            destination: self.dialog_destination.trim().to_owned(),
                            reason: self.dialog_reason.trim().to_owned() });
                        self.dialog_saving=true;
                    }
                    ui.small("This records a proposal; it does not move the original.");
                }
            });
        if !open { self.dialog_photo = None; self.dialog_kind = None; }
    }

    fn details_ui(&mut self, ctx: &egui::Context) {
        let Some(details) = self.details.as_ref() else { return; };
        let mut open = true;
        let mut add_filter = None;
        egui::Window::new("Image properties").open(&mut open)
            .default_size([560.0, 650.0]).show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(map) = details.as_object() {
                        for (key, value) in map {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(format!("{key}:"));
                                ui.label(crate::bridge::readable(value)).on_hover_text(format!("{key}: {}",crate::bridge::readable(value)));
                                let field = match key.as_str() {
                                    "camera_model" => Some("camera"), "lens" => Some("lens"),
                                    "format" => Some("format"), "review_state" => Some("review"),
                                    "root" => Some("root"), "date_source" => Some("date_source"),
                                    _ => None,
                                };
                                if let Some(field) = field {
                                    if value.as_str().is_some() && ui.small_button("Filter to value").on_hover_text("Limit images and reports to this property value.").clicked() {
                                        add_filter = Some((field.to_owned(), value.as_str().unwrap().to_owned()));
                                    }
                                }
                            });
                        }
                    }
                });
            });
        if let Some((field, value)) = add_filter {
            self.filters.push(FilterChip { field, value, exclude: false });
            self.submit_search();
        }
        if !open { self.details = None;self.requested_details=None; }
    }

    fn person_choices_ui(&mut self, ctx: &egui::Context) {
        if self.person_choices.is_empty() { return; }
        let mut chosen = None;
        let mut open = true;
        egui::Window::new("Show images of person").open(&mut open).show(ctx, |ui| {
            for person in &self.person_choices {
                if ui.button(format!("{} (#{})",person.name,person.id)).on_hover_text("Filter by this stable person ID").clicked() { chosen = Some(person.id); }
            }
        });
        if let Some(id) = chosen {
            self.filters.push(FilterChip { field: "person_id".into(), value: id.to_string(), exclude: false });
            self.submit_search();
            self.person_choices.clear();
        }
        if !open { self.person_choices.clear(); }
    }

    fn saved_confirmation_ui(&mut self,ctx:&egui::Context){
        let Some((name,state))=self.saved_confirmation.clone() else{return;};let mut open=true;let mut accepted=false;let mut cancelled=false;
        egui::Window::new(if state.is_some(){"Replace saved view?"}else{"Delete saved view?"}).open(&mut open).show(ctx,|ui|{
            ui.label(format!("{} '{name}'?",if state.is_some(){"Replace the configuration for"}else{"Delete"}));
            if ui.add_enabled(!self.readonly(),egui::Button::new("Confirm")).on_hover_text("Apply this change to the saved view").clicked(){accepted=true;}
            if ui.button("Cancel").on_hover_text("Keep the existing saved view").clicked(){cancelled=true;}
        });
        if accepted{let request=if let Some(state)=state{db::Request::SaveView{name,state}}else{db::Request::DeleteView(name)};let _=self.db_tx.send(request);open=false;}
        if !open||cancelled{self.saved_confirmation=None;}
    }

    fn contact_sheet_ui(&mut self, ctx: &egui::Context) {
        if !self.contact_sheet_open { return; }
        let mut open = true;
        egui::Window::new(format!("Contact sheet · {} images", self.contact_photos.len()))
            .open(&mut open).default_size([900.0,650.0]).max_height((ctx.content_rect().height()-70.0).max(150.0)).show(ctx, |ui| {
                let columns = (ui.available_width()/170.0).floor().max(1.0) as usize;
                let rows = self.contact_photos.len().div_ceil(columns);
                ui.horizontal_wrapped(|ui|{
                    ui.add(egui::TextEdit::singleline(&mut self.sheet_destination).hint_text("New PNG destination path").desired_width(420.0)).on_hover_text("Save a contact sheet from cached thumbnails; existing files are never overwritten");
                    if ui.add_enabled(self.export_rx.is_none()&&!self.sheet_destination.trim().is_empty(),egui::Button::new("Export PNG")).on_hover_text("Export the selected images as a shareable contact sheet; up to 1000 images per sheet").clicked(){
                        let (tx,rx)=crossbeam_channel::bounded(1);self.export_rx=Some(rx);
                        let destination=self.sheet_destination.trim().to_owned();let ids=self.contact_photos.iter().map(|p|p.id.to_string()).collect::<Vec<_>>().join(",");
                        let dsn=self.catalog_panel.context["dsn"].as_str().unwrap_or_default().to_owned();let ctx=ctx.clone();
                        std::thread::spawn(move ||{
                            let result=(||{let mut command=std::process::Command::new(std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into()));command.args(["-m","image_atlas.viewer_export",&destination,&ids]).env("IMAGE_CLI_DSN",dsn);
                                if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
                                let output=crate::bridge::output(&mut command,Duration::from_secs(60),1024*1024)?;
                                if !output.status.success(){return Err(String::from_utf8_lossy(&output.stderr).into());}Ok(String::from_utf8_lossy(&output.stdout).trim().into())})();
                            let _=tx.send(result);ctx.request_repaint();
                        });
                    }
                    if self.export_rx.is_some(){ui.spinner();}
                });
                egui::ScrollArea::vertical().show_rows(ui,180.0,rows, |ui,range| {
                    let start = range.start.saturating_sub(1)*columns;
                    let end = ((range.end+1)*columns).min(self.contact_photos.len());
                    let ids: Vec<i64> = self.contact_photos[start..end].iter().map(|photo| photo.id).collect();
                    self.request_thumbs(&ids);
                    for row in range {
                        ui.horizontal(|ui| {
                            for col in 0..columns {
                                let index = row*columns+col;
                                let Some(photo) = self.contact_photos.get(index) else { continue; };
                                ui.allocate_ui_with_layout(egui::vec2(165.0,175.0),
                                    egui::Layout::top_down(egui::Align::Center), |ui| {
                                        if let Some(entry) = self.thumbnails.get_mut(&photo.id) {
                                            entry.used = self.frame;
                                            let size = entry.texture.size_vec2();
                                            let scale = 150.0/size.x.max(size.y).max(1.0);
                                            ui.image((entry.texture.id(),size*scale)).on_hover_text(photo_tooltip(photo));
                                        } else { ui.spinner(); }
                                        ui.add_sized([160.0,20.0],egui::Label::new(egui::RichText::new(std::path::Path::new(&photo.path).file_name().unwrap_or_default().to_string_lossy()).small()).truncate()).on_hover_text(&photo.path);
                                    });
                            }
                        });
                    }
                });
            });
        self.contact_sheet_open = open;
    }

    fn compare_ui(&mut self, ctx: &egui::Context) {
        if self.compare_ids.is_empty() { return; }
        let photos = self.compare_ids.clone();
        for photo in &photos { self.request_full(photo, 2560); self.request_thumbs(&[photo.id]); }
        let mut open = true;
        let mut action = None;
        let mut remove = None;
        egui::Window::new("Compare images").open(&mut open)
            .default_size([1100.0, 700.0]).max_width((ctx.content_rect().width()-24.0).max(200.0)).max_height((ctx.content_rect().height()-64.0).max(160.0)).show(ctx, |ui| {
                ui.add(egui::Slider::new(&mut self.zoom, 0.25..=8.0).text("Shared zoom"));
                let pane_width = ((ui.available_width()-ui.spacing().item_spacing.x*photos.len().saturating_sub(1) as f32) / photos.len().max(1) as f32).max(30.0);
                let height = ui.available_height();
                ui.horizontal(|ui| {
                    for (index, photo) in photos.iter().enumerate() {
                        ui.allocate_ui_with_layout(egui::vec2(pane_width, height),
                            egui::Layout::top_down(egui::Align::Center), |ui| {
                                ui.add_sized([pane_width,24.0],egui::Label::new(std::path::Path::new(&photo.path).file_name().unwrap_or_default().to_string_lossy()).truncate()).on_hover_text(&photo.path);
                                if ui.small_button("Remove").on_hover_text("Remove this image from comparison").clicked() { remove = Some(index); }
                                self.decode_error_ui(ui,photo.id,false);
                                if let Some(entry) = self.full_images.get_mut(&photo.id)
                                    .or_else(|| self.thumbnails.get_mut(&photo.id)) {
                                    entry.used = self.frame;
                                    let size = entry.texture.size_vec2();
                                    let scale = ((pane_width - 12.0).max(1.0)/size.x)
                                        .min((height - 55.0).max(1.0)/size.y).min(1.0) * self.zoom;
                                    egui::ScrollArea::both().id_salt(("compare-pane",photo.id)).show(ui, |ui| {
                                        ui.image((entry.texture.id(), size*scale)).on_hover_text(photo_tooltip(&photo)).context_menu(|ui| {
                                            action = Self::image_menu(ui, photo, &self.settings.editors,self.catalog_panel.context["readonly"].as_bool().unwrap_or(false))
                                                .map(|action| (photo.clone(), action));
                                        });
                                    });
                                } else { ui.spinner(); }
                            });
                    }
                });
            });
        if let Some(index) = remove { self.compare_ids.remove(index); }
        if let Some((photo, action)) = action { self.apply_image_action(&photo, action, ctx); }
        if !open { self.compare_ids.clear(); }
    }

    fn images_page_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = double_clicked_image(ui.ctx(), &mut self.last_image_click) {
            if let Some(photo) = self.photos.iter().find(|photo| photo.id == id).cloned()
                .or_else(|| self.preview_photo.clone().filter(|photo| photo.id == id)) {
                self.apply_image_action(&photo, ImageAction::OpenOriginal, ui.ctx());
                // The second click must not activate a different control after layout changes.
                ui.disable();
            }
        }
        if self.selected.is_some() || self.preview_photo.is_some() {
            let maximum = (ui.available_width() * 0.5).max(180.0);
            egui::Panel::right("selected-image-preview")
                .default_size(420.0).min_size(180.0).max_size(maximum)
                .show(ui, |ui| self.preview_panel_ui(ui));
        }
        let bounds = ui.available_rect_before_wrap();
        ui.scope_builder(egui::UiBuilder::new().max_rect(bounds), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(bounds));
            ui.set_max_width(bounds.width());
            let viewport_height = ui.available_height();
            egui::ScrollArea::both().auto_shrink([false, false]).id_salt("images-page").show(ui, |ui| {
                ui.set_max_height(viewport_height);
                self.images_ui(ui);
            });
        });
    }

    fn preview_panel_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let index = self.selected;
        let Some(photo) = index.and_then(|index| self.photos.get(index)).cloned()
            .or_else(|| self.preview_photo.clone()) else {
                self.selected = None;
                self.last_displayed = None;
                return;
            };
        if index.is_some_and(|index| index + 2 >= self.photos.len())
            && !self.page_loading && !self.facets_loading && !self.page_error
            && (self.photos.len() as i64) < self.total_results {
            self.page_loading = true;
            self.request_explore(self.photos.len() as i64, false);
        }
        self.record_displayed(photo.id);
        self.request_thumbs(&[photo.id]);
        self.request_database_preview(&photo);
        let mut close = false;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.heading("Image preview");
            close = ui.small_button("Close").clicked();
        });
        egui::ScrollArea::vertical().id_salt("selected-image-panel").show(ui, |ui| {
            ui.add(egui::Label::new(std::path::Path::new(&photo.path).file_name()
                .unwrap_or_default().to_string_lossy()).truncate()).on_hover_text(&photo.path);
            ui.small("Double-click to open the original in your default app.");
            ui.horizontal_wrapped(|ui| {
                if let Some(index) = index {
                    if ui.add_enabled(index > 0, egui::Button::new("← Previous")).clicked() {
                        self.selected = Some(index - 1);
                        self.zoom = 1.0;
                    }
                    if ui.add_enabled(index + 1 < self.photos.len(), egui::Button::new("Next →")).clicked() {
                        self.selected = Some(index + 1);
                        self.zoom = 1.0;
                    }
                }
                if ui.add_enabled(!photo.missing, egui::Button::new("Open original")).clicked() {
                    action = Some(ImageAction::OpenOriginal);
                }
            });
            ui.label(format!("{} × {}{}", photo.width, photo.height,
                if photo.missing { " · original missing" } else { "" }));
            ui.separator();
            self.decode_error_ui(ui, photo.id, false);
            if self.pending_full.contains(&photo.id) {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Loading database preview…"); });
            }
            if !self.full_images.contains_key(&photo.id) && self.thumbnails.contains_key(&photo.id) {
                ui.small("Thumbnail shown until a database preview is available.");
            }
            if let Some(entry) = self.full_images.get_mut(&photo.id)
                .or_else(|| self.thumbnails.get_mut(&photo.id)) {
                entry.used = self.frame;
                let size = entry.texture.size_vec2();
                let scale = (ui.available_width().max(1.0) / size.x)
                    .min(ui.available_height().max(100.0) / size.y).min(1.0);
                let response = ui.add(egui::Image::new((entry.texture.id(), size * scale))
                    .sense(egui::Sense::click())).on_hover_text(photo_tooltip(&photo));
                if response.double_clicked() { action = Some(ImageAction::OpenOriginal); }
                response.context_menu(|ui| {
                    action = Self::image_menu(ui, &photo, &self.settings.editors, self.readonly());
                });
            }
        });
        if let Some(action) = action { self.apply_image_action(&photo, action, &ctx); }
        if self.view_mode != ViewMode::Filmstrip && !ctx.egui_wants_keyboard_input() {
            if let Some(index) = index {
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) && index + 1 < self.photos.len() {
                    self.selected = Some(index + 1);
                }
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && index > 0 {
                    self.selected = Some(index - 1);
                }
            }
        }
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selected = None;
            self.preview_photo = None;
        }
    }

}

impl Drop for Viewer {
    fn drop(&mut self){self.decode_cancel.store(true,Ordering::Release);}
}

impl eframe::App for Viewer {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}
impl Viewer {
    fn draw(&mut self, ui:&mut egui::Ui) {
        ui.painter().rect_filled(ui.max_rect(),0.0,ui.visuals().panel_fill);
        if let Some(context)=self.catalog_panel.ui_with_selector(ui,!self.external_catalog_selector) {
            self.switch_catalog(ui.ctx().clone(),context);
            return;
        }
        let frame_start = Instant::now();
        self.frame += 1;
        let ctx = ui.ctx().clone();
        #[cfg(debug_assertions)]
        if let Some(path) = &self.screenshot_path {
            for event in ui.input(|input| input.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let pixels: Vec<u8> = image
                        .pixels
                        .iter()
                        .flat_map(|pixel| pixel.to_array())
                        .collect();
                    if let Err(error) = image::save_buffer(
                        path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        self.message = format!("Screenshot failed: {error}");
                    }
                    self.screenshot_path = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    break;
                }
            }
        }
        self.receive(&ctx);
        if self.model_queue_pending&&self.visual_pending&&self.search_sent {
            self.model_queue_pending=self.model_tx.try_send(model::Request::Encode{generation:self.generation,text:self.search.clone()}).is_err();
            if self.model_queue_pending{ctx.request_repaint_after(Duration::from_millis(100));}
        }
        #[cfg(debug_assertions)] if self.screenshot_path.is_some(){
            if let Some(photo)=self.photos.first(){self.request_thumbs(&[photo.id]);}
            if std::env::var_os("IMAGE_CLI_QA_SELECT_FIRST").is_some() && self.selected.is_none() && !self.photos.is_empty(){self.selected=Some(0);}
        }
        if self.last_stats_request.elapsed() >= Duration::from_secs(3) {
            let _ = self.db_tx.try_send(db::Request::Stats);
            self.last_stats_request = Instant::now();
        }
        if !self.search_sent && self.search_changed.elapsed() >= Duration::from_millis(90) {
            self.submit_search();
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(self.tab == Tab::Images, "Images")
                .clicked()
            {
                self.tab = Tab::Images;
            }
            if ui
                .selectable_label(self.tab == Tab::People, "People")
                .clicked()
            {
                self.tab = Tab::People;
                self.groups_loading=true;self.groups_error=false;let _ = self.db_tx.send(db::Request::Groups);
            }
            if ui
                .selectable_label(self.tab == Tab::Proposals, "Sort proposals")
                .clicked()
            {
                self.tab = Tab::Proposals;
                self.request_proposals(false);
            }
            if ui.selectable_label(self.tab == Tab::Collections,"Collections").on_hover_text("Browse and edit detected events, trips, and places").clicked() {
                self.tab=Tab::Collections;
                self.collections_panel.refresh(&ctx);
            }
            if self.frame_durations.len() >= 60 {
                ui.small(format!("UI p95 {:.1} ms", self.ui_p95_ms)).on_hover_text("95th percentile time spent drawing the viewer UI across recent frames; excludes background database and model work.");
            }
        });
        ui.add(egui::Label::new(&self.message).truncate()).on_hover_text(&self.message);
        ui.separator();
        match self.tab {
            Tab::Images => self.images_page_ui(ui),
            Tab::People => self.people_ui(ui),
            Tab::Proposals => self.proposals_ui(ui),
            Tab::Collections => {
                let textures=self.thumbnails.iter().map(|(id,c)|(*id,c.texture.clone())).collect();
                let mut requests=Vec::new();
                let browse=self.collections_panel.ui(ui,&textures,&mut requests,&self.selected_ids);
                self.request_thumbs(&requests);
                if self.collections_panel.changed {self.collections_panel.changed=false;self.refresh_search();}
                if let Some(id)=browse {
                    self.filters.retain(|f|f.field!="collection");
                    self.filters.push(FilterChip{field:"collection".into(),value:id.to_string(),exclude:false});
                    self.tab=Tab::Images;self.submit_search();
                }
            },
        }
        self.compare_ui(&ctx);
        self.action_dialog_ui(&ctx);
        self.details_ui(&ctx);
        self.person_choices_ui(&ctx);
        self.contact_sheet_ui(&ctx);
        self.saved_confirmation_ui(&ctx);
        if self.person_confirmation.is_some(){
            let mut open=true;let mut confirmed=false;let mut cancel=false;
            egui::Window::new("Confirm identity correction").open(&mut open).show(&ctx,|ui|{
                ui.label(match self.person_confirmation.as_ref().unwrap(){db::Request::PersonMerge{source,target}=>format!("Merge person #{source} into #{target}? The source ID will be removed and its samples transferred."),db::Request::FaceReject(ids)=>format!("Remove {} selected face detections? Their assignments will be removed; original images are preserved.",ids.len()),_=>String::new()});
                confirmed=ui.button("Confirm correction").on_hover_text("Apply the correction described above").clicked();if ui.button("Cancel").clicked(){cancel=true;}
            });
            if confirmed{if let Some(command)=self.person_confirmation.take(){let _=self.db_tx.send(command);}}else if !open||cancel{self.person_confirmation=None;}
        }
        #[cfg(debug_assertions)]
        if self.screenshot_path.is_some()
            && !self.screenshot_requested
            && !self.page_loading
            && !self.facets_loading
            && self.pending_thumbs.is_empty()
            && self.catalog_panel.ready()
            && (self.tab!=Tab::People||(!self.groups.is_empty()&&self.pending_faces.is_empty()))
            && (self.tab!=Tab::Collections||self.collections_panel.ready())
            && (self.tab!=Tab::Proposals||!self.proposals.is_empty())
            && self
                .screenshot_expected_folder
                .as_ref()
                .is_none_or(|expected| self.map_root.as_ref() == Some(expected))
            && self.photos.first().is_some_and(|photo| self.thumbnails.contains_key(&photo.id))
            && (std::env::var_os("IMAGE_CLI_QA_SELECT_FIRST").is_none()
                || self.selected.and_then(|index|self.photos.get(index)).is_some_and(|photo|self.full_images.contains_key(&photo.id)))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.screenshot_requested = true;
        }
        if !self.search_sent {
            ctx.request_repaint_after(Duration::from_millis(90));
        }
        ctx.request_repaint_after(Duration::from_secs(3));
        self.frame_durations.push_back(frame_start.elapsed().as_secs_f32()*1000.0);
        if self.frame_durations.len()>600 { self.frame_durations.pop_front(); }
        if self.frame%60==0 {
            let mut samples: Vec<f32> = self.frame_durations.iter().copied().collect();
            samples.sort_unstable_by(f32::total_cmp);
            self.ui_p95_ms = samples.get((samples.len()*95/100).min(samples.len().saturating_sub(1)))
                .copied().unwrap_or_default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolbar_frame(ctx: &egui::Context, sort: &mut SortSpec, events: Vec<egui::Event>, width: f32) -> egui::FullOutput {
        ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.0))), events, ..Default::default() }, |ui| { sorting_toolbar(ui,sort); })
    }

    fn text_position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output.shapes.iter().find_map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => Some(text.pos + text.galley.size()/2.0),
            _ => None,
        }).unwrap_or_else(|| panic!("missing toolbar label: {label}"))
    }

    fn click_toolbar(ctx: &egui::Context, sort: &mut SortSpec, label: &str, width: f32) {
        let output=toolbar_frame(ctx,sort,Vec::new(),width);
        let pos=text_position(&output,label);
        let _=toolbar_frame(ctx,sort,vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton { pos, button:egui::PointerButton::Primary, pressed:true, modifiers:Default::default() }],width);
        let _=toolbar_frame(ctx,sort,vec![egui::Event::PointerButton { pos, button:egui::PointerButton::Primary, pressed:false, modifiers:Default::default() }],width);
    }

    #[test]
    fn toolbar_clicks_reverse_sort_and_choose_group_at_supported_widths() {
        for width in [640.0,1200.0] {
            let ctx=egui::Context::default();
            let mut sort=SortSpec::default();
            click_toolbar(&ctx,&mut sort,"Name",width);
            assert_eq!(sort.primary,SortKey::Filename); assert!(!sort.primary_desc);
            click_toolbar(&ctx,&mut sort,"Name",width); assert!(sort.primary_desc);
            click_toolbar(&ctx,&mut sort,"Descending",width); assert!(!sort.primary_desc);
            click_toolbar(&ctx,&mut sort,"No grouping",width);
            click_toolbar(&ctx,&mut sort,"People",width); assert_eq!(sort.group_by,GroupBy::People);
            click_toolbar(&ctx,&mut sort,"Reset",width); assert_eq!(sort,SortSpec::default());
            let output=toolbar_frame(&ctx,&mut sort,Vec::new(),width);
            for label in ["Match","Date","Name","Size","Pixels","Folder","Camera","Rating","Faces","Copies","Shuffle","Group by"] {
                let pos=text_position(&output,label); assert!(pos.x>=0.0 && pos.x<width,"{label} clipped at {width}");
            }
        }
    }

    #[test]
    fn toolbar_mouse_over_shows_explanation() {
        let ctx=egui::Context::default();
        ctx.style_mut_of(egui::Theme::Dark, |style| { style.interaction.tooltip_delay=0.0; style.interaction.show_tooltips_only_when_still=false; });
        let mut sort=SortSpec::default();
        let output=toolbar_frame(&ctx,&mut sort,Vec::new(),1200.0);
        let pos=text_position(&output,"Size");
        let _=toolbar_frame(&ctx,&mut sort,vec![egui::Event::PointerMoved(pos)],1200.0);
        let output=toolbar_frame(&ctx,&mut sort,Vec::new(),1200.0);
        assert!(output.shapes.iter().any(|clipped| matches!(&clipped.shape,egui::epaint::Shape::Text(text) if text.galley.job.text.contains("Original file size in bytes"))),"size tooltip did not appear");
    }

    #[test]
    fn live_visual_search_reaches_viewer_results() {
        if std::env::var("IMAGE_CLI_LIVE_SEARCH_DSN").is_err() {
            return;
        }
        let ctx = egui::Context::default();
        let mut viewer = Viewer::new(ctx.clone());
        let catalog_deadline = Instant::now() + Duration::from_secs(10);
        while viewer.photos.is_empty() && Instant::now() < catalog_deadline {
            viewer.receive(&ctx);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!viewer.photos.is_empty(), "catalog did not load");
        viewer.search = "dog".into();
        viewer.submit_search();
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            viewer.receive(&ctx);
            if viewer.visual_pending {
                assert!(
                    !viewer.photos.is_empty(),
                    "catalog vanished while visual search waits"
                );
            }
            if viewer.visual_results.is_some() {
                assert!(!viewer.photos.is_empty());
                assert!(!viewer.visual_pending);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("visual search timed out: {}", viewer.message);
    }
}

#[cfg(test)] mod qa_tests;
