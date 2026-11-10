use super::*;

fn app()->(Viewer,egui::Context){
    let ctx=egui::Context::default();
    let(tx,_)=crossbeam_channel::unbounded();let(_,rx)=crossbeam_channel::unbounded();
    let mut app=Viewer::build_catalog(ctx.clone(),serde_json::json!({"name":"Unit fixture","dsn":""}),Some((tx,rx,Arc::new(AtomicU64::new(0)),false,None)));
    let(tx,_)=crossbeam_channel::unbounded();app.db_tx=tx;
    let(tx,_)=crossbeam_channel::unbounded();app.decode_tx=tx;
    app.page_loading=false;app.facets_loading=false;
    app.photos=(1..=200).map(|id|Photo{id,sha256:None,path:format!("fixture-{id}.jpg"),width:320,height:240,missing:false,size_bytes:100,capture_day:Some("2026-10-01".into()),capture_at:None,camera_model:None,root_path:"fixture".into(),dominant_rgb:None,duplicate_count:1,format:Some("JPEG".into()),document:None,favorite:false,rating:0,review_state:"unreviewed".into(),cursor_primary:None,cursor_secondary:None,image_group:None,collection_names:None}).collect();
    app.folder_tree=visuals::FolderTree::new(&[db::FolderStat{path:"fixture".into(),count:200,bytes:20000,unique_bytes:18000}]);app.refresh_map_tiles();
    app.days=vec![DayStat{day:"2026-10-01".into(),count:200,fallback_count:10}];
    app.geo_bins=vec![GeoBin{lat:60,lon:49,count:10}];app.scatter_bins=vec![ScatterBin{bytes_log2:12,pixels_log2:18,count:10}];app.folder_months=vec![FolderMonth{folder:"fixture".into(),month:10,count:200}];
    app.total_results=240;(app,ctx)
}

#[test] fn filmstrip_requests_next_page_at_boundary(){let(mut app,ctx)=app();app.selected=Some(199);let _=ctx.run_ui(Default::default(),|ui|app.filmstrip_ui(ui));assert!(app.page_loading);}

#[test] fn metadata_refresh_preserves_preview_by_id(){
    let(mut app,ctx)=app();app.selected=Some(50);app.refresh_search();assert_eq!(app.preview_photo.as_ref().map(|p|p.id),Some(51));
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;let mut photos=app.photos.clone();photos.reverse();
    tx.send(db::Event::ExplorePage{generation:app.generation,phase:app.phase,offset:0,photos}).unwrap();app.receive(&ctx);
    assert_eq!(app.photos[app.selected.unwrap()].id,51);assert!(app.preview_photo.is_none());
}

#[test]
fn dropped_reference_search_uses_an_image_vector_and_ignores_stale_replies() {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        for width in [640.0, 1200.0] {
            let (mut app, ctx) = app(); ctx.set_theme(theme);
            let path = std::env::temp_dir().join(format!("imageatlas-reference-{}-{width}-{theme:?}.png", std::process::id()));
            std::fs::write(&path, b"reference fixture").unwrap();
            let (tx, requests) = crossbeam_channel::unbounded(); app.model_tx = tx;
            let (tx, database) = crossbeam_channel::unbounded(); app.db_tx = tx;
            let dropped = egui::DroppedFile { path: Some(path.clone()), ..Default::default() };
            let _ = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0))),
                dropped_files: vec![dropped], ..Default::default()
            }, |ui| app.images_ui(ui));
            assert!(matches!(requests.try_recv().unwrap(), model::Request::EncodeImage { generation, path: selected }
                if generation == app.generation && selected == path.canonicalize().unwrap()));
            assert_eq!(app.sort.primary, SortKey::Relevance);
            assert!(app.search.is_empty());
            database.try_iter().for_each(drop);
            let (tx, rx) = crossbeam_channel::unbounded(); app.model_rx = rx;
            let mut vector = vec![0.0; 512]; vector[0] = 1.0;
            tx.send(model::Event::Vector { generation: app.generation, vector: vector.clone() }).unwrap();
            app.receive(&ctx);
            assert!(database.try_iter().any(|request| matches!(request, db::Request::Explore(request)
                if request.query.is_empty() && request.source_image_id.is_none() && request.vector == Some(vector.clone()))));
            let old_generation = app.generation;
            app.reference_action(image_search::Action::Clear);
            tx.send(model::Event::Error { generation: old_generation, error: "Stale error".into() }).unwrap();
            tx.send(model::Event::Vector { generation: old_generation, vector }).unwrap();
            app.receive(&ctx);
            assert!(app.cached_vector.is_none() && app.image_search.path.is_none() && app.model_error.is_none());
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[test]
fn saved_reference_queries_remain_compatible_with_older_saved_views() {
    let (mut app, _) = app();
    app.image_search.path = Some(PathBuf::from("reference.png"));
    let saved = app.saved_state();
    app.image_search.path = None; app.restore_state(saved.clone());
    assert_eq!(app.image_search.path, Some(PathBuf::from("reference.png")));
    let mut old = saved; old.as_object_mut().unwrap().remove("reference_image");
    app.restore_state(old); assert!(app.image_search.path.is_none());
}

#[test] fn failed_decode_requires_explicit_retry(){
    let(mut app,ctx)=app();let(done,rx)=crossbeam_channel::unbounded();app.decode_rx=rx;
    done.send(decode::Decoded{id:1,revision:None,full:true,face:false,image:Err("Unreadable source".into())}).unwrap();app.receive(&ctx);
    let(jobs,rx)=crossbeam_channel::unbounded();app.db_tx=jobs;app.request_full(&app.photos[0].clone(),2560);assert!(rx.try_recv().is_err());
    app.failed_full.remove(&1);app.request_full(&app.photos[0].clone(),2560);assert!(rx.try_recv().is_ok());
}

#[test] fn four_comparison_images_remain_cached(){let(mut app,ctx)=app();for id in 1..=4{app.full_images.insert(id,CachedTexture{texture:ctx.load_texture(format!("pane-{id}"),egui::ColorImage::filled([64,64],egui::Color32::WHITE),Default::default()),used:id as u64});}app.trim_caches();assert_eq!(app.full_images.len(),4);}

#[test] fn side_preview_uses_database_even_after_another_artboard_was_selected(){
    let(mut app,ctx)=app();
    let mut photo=app.photos[0].clone();photo.path="artboards.ai".into();photo.format=Some("AI".into());
    photo.document=Some(serde_json::json!({"preview_available":true,"page_count":2}));
    app.photos[0]=photo.clone();app.selected=Some(0);app.document_artboards.insert(photo.id,1);
    let(jobs,decodes)=crossbeam_channel::unbounded();app.decode_tx=jobs;
    app.request_full(&photo,2048);
    assert!(matches!(decodes.try_recv().unwrap(),decode::Job::Full(1,_,2048,1,_)));
    let old=app.source_revisions[&photo.id].clone();
    let(tx,requests)=crossbeam_channel::unbounded();app.db_tx=tx;
    let _=ctx.run_ui(Default::default(),|ui|app.preview_panel_ui(ui));
    assert!(requests.try_iter().any(|request|matches!(request,db::Request::Preview(1,Some(revision)) if revision.ends_with("|0"))));
    assert!(decodes.try_recv().is_err(),"Selecting an image must not decode an original");
    let(done,rx)=crossbeam_channel::unbounded();app.decode_rx=rx;
    done.send(decode::Decoded{id:photo.id,revision:old,full:true,face:false,image:Ok(egui::ColorImage::filled([10,10],egui::Color32::RED))}).unwrap();
    app.receive(&ctx);assert!(!app.full_images.contains_key(&photo.id));
}

#[test] fn selection_previews_and_double_click_requests_the_original_in_every_view(){
    for mode in [ViewMode::Grid,ViewMode::Table,ViewMode::Filmstrip,ViewMode::Stacks]{
        let(mut app,ctx)=app();app.view_mode=mode;
        assert!(app.image_click_action(0,false,&ctx).is_none());
        assert_eq!(app.selected,Some(0));
        assert!(matches!(app.image_click_action(0,true,&ctx),Some(ImageAction::OpenOriginal)));
        assert_eq!(app.selected,Some(0));
    }
}

#[test] fn selected_preview_is_docked_and_uses_cached_pixels_in_both_themes_and_sizes(){
    for theme in [egui::Theme::Dark,egui::Theme::Light]{for width in [640.,1200.]{
        let(mut app,ctx)=app();ctx.set_theme(theme);app.selected=Some(0);
        let texture=ctx.load_texture("database-preview",egui::ColorImage::filled([1024,683],egui::Color32::LIGHT_BLUE),Default::default());
        let texture_id=texture.id();
        app.full_images.insert(1,CachedTexture{texture,used:0});
        let mut output=None;
        for _ in 0..2 {output=Some(ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.))),..Default::default()},|ui|app.images_page_ui(ui)));}
        let output=output.unwrap();
        let label=output.shapes.iter().find_map(|shape|match &shape.shape{egui::epaint::Shape::Text(text) if text.galley.job.text=="Image preview"=>Some(text.pos),_=>None}).expect("Side preview heading");
        assert!(label.x>=width*0.45&&label.x<width);
        let primitives=ctx.tessellate(output.shapes.clone(),output.pixels_per_point);
        let image=primitives.iter().find_map(|shape|match &shape.primitive{egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id==texture_id=>Some(mesh.calc_bounds()),_=>None}).expect("Database preview rendered");
        assert!(image.min.x>=label.x-2.&&image.max.x<=width&&image.width()>100.);
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::epaint::Shape::Text(text) if text.galley.job.text=="Search photos")),"Browser remains visible");
        let search_clip=output.shapes.iter().find_map(|shape|match &shape.shape{egui::epaint::Shape::Text(text) if text.galley.job.text=="Search photos"=>Some(shape.clip_rect),_=>None}).unwrap();
        assert!(search_clip.max.x<=label.x,"Browser content must not paint over the side preview");
    }}
}

#[test] fn double_click_keeps_its_original_target_after_the_side_panel_moves_the_grid(){
    for width in [640.,1200.]{
        let(mut app,ctx)=app();
        let frame=|app:&mut Viewer,events|ctx.run_ui(egui::RawInput{
            screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,800.))),
            events,..Default::default()},|ui|app.images_page_ui(ui));
        let output=frame(&mut app,vec![]);
        let position=output.shapes.iter().find_map(|shape|match &shape.shape{
            egui::epaint::Shape::Text(text) if text.galley.job.text=="fixture-1.jpg"=>Some(text.pos+text.galley.size()/2.),_=>None
        }).expect("First image filename");
        let click=|app:&mut Viewer|{
            frame(app,vec![egui::Event::PointerMoved(position),egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
            frame(app,vec![egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
        };
        click(&mut app);
        assert_eq!(app.selected,Some(0));
        assert!(app.message.is_empty()||!app.message.contains("Register the catalog"));
        click(&mut app);
        // This fixture has no registered roots: the original action must reach the
        // access check rather than opening a system application during the test.
        assert!(app.message.contains("Register the catalog"),"{}",app.message);
        assert_eq!(app.selected,Some(0));
    }
}

#[test] fn an_expired_image_click_cannot_open_an_original_from_an_unrelated_double_click(){
    let ctx=egui::Context::default();let position=egui::pos2(100.,100.);
    let mut previous=Some((1,position,0.));
    let _=ctx.run_ui(egui::RawInput{time:Some(2.),events:vec![
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()},
        egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()},
    ],..Default::default()},|_|{});
    let _=ctx.run_ui(egui::RawInput{time:Some(2.05),events:vec![
        egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()},
        egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()},
    ],..Default::default()},|_|{
        assert!(ctx.input(|input|input.pointer.button_double_clicked(egui::PointerButton::Primary)));
        assert_eq!(double_clicked_image(&ctx,&mut previous),None);
    });
    assert!(previous.is_none());
}

#[test] fn illustrator_without_preview_does_not_enqueue_a_decode(){
    let(mut app,_)=app();let mut photo=app.photos[0].clone();
    photo.document=Some(serde_json::json!({"preview_available":false,"preview_reason":"No PDF-compatible artwork"}));
    let(jobs,requests)=crossbeam_channel::unbounded();app.decode_tx=jobs;
    app.request_full(&photo,2560);
    assert!(requests.try_recv().is_err());assert_eq!(app.failed_full[&photo.id],"No PDF-compatible artwork");
}

#[test] fn selection_toggles_and_stale_properties_are_ignored(){
    let(mut app,ctx)=app();let photo=app.photos[0].clone();app.apply_image_action(&photo,ImageAction::Select,&ctx);assert!(app.selected_ids.contains(&1));app.apply_image_action(&photo,ImageAction::Select,&ctx);assert!(app.selected_ids.is_empty());
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;app.requested_details=Some(2);
    tx.send(db::Event::Details(serde_json::json!({"id":1}))).unwrap();app.receive(&ctx);assert!(app.details.is_none());
    tx.send(db::Event::Details(serde_json::json!({"id":2}))).unwrap();app.receive(&ctx);assert_eq!(app.details.as_ref().unwrap()["id"],2);
}

#[test] fn failed_search_clears_stale_report_and_image_results(){
    let(mut app,ctx)=app();let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;
    tx.send(db::Event::ExploreError{generation:app.generation,phase:app.phase,error:"Bad filter".into()}).unwrap();app.receive(&ctx);assert!(app.photos.is_empty());assert_eq!(app.total_results,0);assert!(app.page_error);
}

#[test] fn unicode_tag_failure_keeps_dialog_input(){
    let(mut app,ctx)=app();app.dialog_photo=Some(app.photos[0].clone());app.dialog_kind=Some("tag");app.dialog_tag="東京 · café".into();app.dialog_saving=true;
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;tx.send(db::Event::SaveFailed("tag",1,"Read-only".into())).unwrap();app.receive(&ctx);
    assert_eq!(app.dialog_tag,"東京 · café");assert!(app.dialog_photo.is_some());assert!(!app.dialog_saving);
}

#[test] fn every_view_and_report_renders_in_both_themes_and_small_windows(){
    for theme in [egui::Theme::Dark,egui::Theme::Light]{for width in [640.0,1200.0]{
        let(mut app,ctx)=app();ctx.set_theme(theme);
        for mode in [ViewMode::Grid,ViewMode::Table,ViewMode::Filmstrip,ViewMode::Stacks]{for report in [ExploreVisual::Folders,ExploreVisual::Gps,ExploreVisual::SizePixels,ExploreVisual::FolderMonths]{app.view_mode=mode;app.explore_visual=report;let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,600.0))),..Default::default()},|ui|{app.images_ui(ui);});assert!(!output.shapes.is_empty());}}
    }}
}

#[test] fn dense_timeline_keeps_visible_bars_and_hover_totals(){
    let(mut app,ctx)=app();app.days=vec![DayStat{day:"2000-01-01".into(),count:2,fallback_count:0},DayStat{day:"2048-09-01".into(),count:3,fallback_count:0}];
    ctx.style_mut_of(egui::Theme::Dark,|style|style.interaction.tooltip_delay=0.0);ctx.style_mut_of(egui::Theme::Light,|style|style.interaction.tooltip_delay=0.0);
    let frame=|app:&mut Viewer,events:Vec<egui::Event>,time:f64|ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(640.0,300.0))),events,time:Some(time),..Default::default()},|ui|app.timeline_ui(ui));
    let output=frame(&mut app,Vec::new(),0.0);
    let bars:Vec<_>=output.shapes.iter().filter_map(|s|match &s.shape{egui::epaint::Shape::Rect(rect) if rect.fill==egui::Color32::from_rgb(87,158,217)&&rect.rect.height()>0.0=>Some(rect.rect),_=>None}).collect();
    assert_eq!(bars.len(),2);assert!(bars.iter().all(|bar|bar.width()>0.0));
    let pos=bars[0].center();let _=frame(&mut app,vec![egui::Event::PointerMoved(pos)],1.0);let output=frame(&mut app,Vec::new(),2.0);
    assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text.contains("2000-01 · 2 images"))));
}

#[test] fn narrow_scrolled_page_never_allocates_negative_panel_sizes(){
    let(mut app,ctx)=app();app.explore_visual=ExploreVisual::Gps;
    for height in [480.0,600.0]{let _=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(640.0,height))),..Default::default()},|ui|{
        ui.allocate_space(egui::vec2(640.0,80.0));let remaining=ui.available_height();egui::ScrollArea::vertical().show(ui,|ui|{ui.set_max_height(remaining);app.images_ui(ui);});
    });}
}

#[test] fn source_changes_invalidate_cached_pixels_and_ignore_late_decodes(){
    let(mut app,ctx)=app();app.track_content(1,Some("old".into()));
    app.thumbnails.insert(1,CachedTexture{texture:ctx.load_texture("old",egui::ColorImage::filled([8,8],egui::Color32::RED),Default::default()),used:1});
    app.track_content(1,Some("new".into()));assert!(!app.thumbnails.contains_key(&1));
    let(tx,rx)=crossbeam_channel::unbounded();app.decode_rx=rx;
    tx.send(decode::Decoded{id:1,revision:Some("old".into()),full:false,face:false,image:Ok(egui::ColorImage::filled([8,8],egui::Color32::RED))}).unwrap();app.receive(&ctx);assert!(!app.thumbnails.contains_key(&1));
    tx.send(decode::Decoded{id:1,revision:Some("new".into()),full:false,face:false,image:Ok(egui::ColorImage::filled([8,8],egui::Color32::BLUE))}).unwrap();app.receive(&ctx);assert!(app.thumbnails.contains_key(&1));
}

#[test] fn read_only_browsing_does_not_write_viewing_history(){
    let(mut app,_)=app();app.catalog_panel.context["readonly"]=serde_json::Value::Bool(true);
    let(tx,rx)=crossbeam_channel::unbounded();app.db_tx=tx;app.record_displayed(1);assert!(rx.try_recv().is_err());
}

#[test] fn filmstrip_arrow_keys_work_after_switching_views(){
    let(mut app,ctx)=app();
    let frame=|app:&mut Viewer,events:Vec<egui::Event>|ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1200.0,800.0))),events,..Default::default()},|ui|app.images_ui(ui));
    let output=frame(&mut app,Vec::new());let pos=output.shapes.iter().find_map(|s|match &s.shape{egui::epaint::Shape::Text(t) if t.galley.job.text=="Filmstrip"=>Some(t.pos+t.galley.size()/2.0),_=>None}).unwrap();
    frame(&mut app,vec![egui::Event::PointerMoved(pos),egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
    frame(&mut app,vec![egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
    assert!(app.view_mode==ViewMode::Filmstrip);
    frame(&mut app,vec![egui::Event::Key{key:egui::Key::ArrowRight,physical_key:None,pressed:true,repeat:false,modifiers:Default::default()}]);
    assert_eq!(app.selected,Some(1));
}

#[test] fn cold_visual_search_enqueues_the_first_request_and_keeps_selected_id_on_rerank(){
    let(mut app,ctx)=app();let(tx,requests)=crossbeam_channel::unbounded();app.model_tx=tx;app.model_ready=false;app.search="東京 beach".into();app.submit_search();
    assert!(matches!(requests.try_recv().unwrap(),model::Request::Encode{text,..} if text=="東京 beach"));
    app.selected=Some(20);let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;let mut photos=app.photos.clone();photos.reverse();tx.send(db::Event::ExplorePage{generation:app.generation,phase:app.phase,offset:0,photos}).unwrap();app.receive(&ctx);assert_eq!(app.photos[app.selected.unwrap()].id,21);
}

#[test] fn refresh_retains_the_loaded_span_for_navigation_beyond_first_page(){
    let(mut app,_)=app();let mut last=app.photos[0].clone();last.id=240;app.photos.push(last);app.selected=Some(200);app.refresh_search();assert_eq!(app.refresh_span,201);assert_eq!(app.preview_photo.as_ref().unwrap().id,240);
}

#[test] fn successful_proposal_acknowledgement_closes_its_pending_form(){
    let(mut app,ctx)=app();app.dialog_photo=Some(app.photos[0].clone());app.dialog_kind=Some("move");app.dialog_destination="D:\\proposed\\image.jpg".into();app.dialog_saving=true;
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;tx.send(db::Event::Saved("proposal",1)).unwrap();app.receive(&ctx);assert!(app.dialog_photo.is_none());assert!(!app.dialog_saving);assert!(app.dialog_destination.is_empty());
}

#[test] fn proposal_paging_footer_remains_visible(){
    for height in [480.0,800.0] {
        let(mut app,ctx)=app();
        app.proposals=(1..=100).map(|id|db::Proposal{id,image_id:id,source:format!("fixture-{id}.jpg"),destination:"D:\\qa.jpg".into(),reason:"QA".into(),status:"pending".into()}).collect();app.proposal_more=true;
        let mut position=None;
        for _ in 0..3 {let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(640.0,height))),..Default::default()},|ui|app.proposals_ui(ui));
            position=output.shapes.iter().find_map(|s|match &s.shape{egui::epaint::Shape::Text(t) if t.galley.job.text=="Load next 100 proposals"=>Some((t.pos.y,t.galley.size().y)),_=>None});
        }
        let(y,h)=position.expect("Paging footer painted");assert!(y+h<=height);
    }
}

#[test] fn failed_face_page_can_be_retried(){
    let(mut app,ctx)=app();app.group_detail=Some(1);app.group_more=true;app.group_loading=true;
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;tx.send(db::Event::RequestFailed("faces","Offline".into())).unwrap();app.receive(&ctx);
    assert!(!app.group_loading&&app.group_more);
}

#[test] fn preview_move_invalidates_failure_and_rejects_old_path_reply(){
    let(mut app,ctx)=app();let mut photo=app.photos[0].clone();app.request_full(&photo,2560);let old=app.source_revisions[&photo.id].clone();
    app.failed_full.insert(photo.id,"Old path missing".into());photo.path="new-location.jpg".into();app.request_full(&photo,2560);assert!(!app.failed_full.contains_key(&photo.id));
    let(tx,rx)=crossbeam_channel::unbounded();app.decode_rx=rx;tx.send(decode::Decoded{id:photo.id,revision:old,full:true,face:false,image:Err("Late old error".into())}).unwrap();app.receive(&ctx);
    assert!(!app.failed_full.contains_key(&photo.id));
}

#[test] fn metadata_columns_do_not_shift_with_filename_length(){
    let(mut app,ctx)=app();app.photos[0].path="東京 · café.jpg".into();app.photos[1].path="long-review-name-".repeat(12)+".jpg";
    let output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(840.0,480.0))),..Default::default()},|ui|app.table_ui(ui));
    let columns:Vec<_>=output.shapes.iter().filter_map(|s|match &s.shape{egui::epaint::Shape::Text(t) if t.galley.job.text=="2026-10-01"=>Some(t.pos.x),_=>None}).collect();
    assert!(columns.len()>2);assert!(columns.iter().all(|x|(*x-columns[0]).abs()<0.1));
}

#[test] fn active_person_filter_and_pickers_follow_identity_merge(){
    let(mut app,ctx)=app();app.filters.push(FilterChip{field:"person_id".into(),value:"12".into(),exclude:false});app.tag_person=Some(12);app.face_target=Some(12);
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;tx.send(db::Event::PersonMerged(12,34)).unwrap();app.receive(&ctx);
    assert_eq!(app.filters[0].value,"34");assert_eq!(app.tag_person,Some(34));assert_eq!(app.face_target,Some(34));
}

#[test] fn closed_pending_form_acknowledgements_release_busy_state(){
    let(mut app,ctx)=app();app.dialog_saving=true;app.dialog_photo=None;app.tag_saving=true;app.tag_group=None;
    let(tx,rx)=crossbeam_channel::unbounded();app.db_rx=rx;tx.send(db::Event::Saved("person",12)).unwrap();tx.send(db::Event::SaveFailed("tag",1,"Rejected".into())).unwrap();app.receive(&ctx);
    assert!(!app.tag_saving&&!app.dialog_saving);
}
