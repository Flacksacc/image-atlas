use crate::{explore, visuals};
use chrono::{DateTime, Local, NaiveDate, Utc};
use crossbeam_channel::{Receiver, Sender};
use postgres::Client;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;

#[derive(Clone, Debug)]
pub struct Photo {
    pub id: i64,
    pub path: String,
    pub sha256:Option<String>,
    pub width: i32,
    pub height: i32,
    pub missing: bool,
    pub size_bytes: i64,
    pub capture_day: Option<String>,
    pub capture_at: Option<String>,
    pub camera_model: Option<String>,
    pub root_path: String,
    pub dominant_rgb: Option<i32>,
    pub duplicate_count: i64,
    pub format: Option<String>,
    pub document: Option<serde_json::Value>,
    pub favorite: bool,
    pub rating: i16,
    pub review_state: String,
    pub cursor_primary: Option<String>,
    pub cursor_secondary: Option<String>,
    pub image_group: Option<String>,
    pub collection_names: Option<String>,
}

pub fn photo_from_row(row: &postgres::Row) -> Photo {
    Photo {
        id: row.get("id"), path: row.get("path"),sha256:row.try_get("sha256").unwrap_or(None),
        width: row.get::<_, Option<i32>>("width").unwrap_or(0),
        height: row.get::<_, Option<i32>>("height").unwrap_or(0),
        missing: row.get("missing"),
        size_bytes: row.get("size_bytes"),
        capture_day: row.get::<_, Option<String>>("capture_day"),
        capture_at: row.get("capture_at"),
        camera_model: row.get("camera_model"),
        root_path: row.get("root_path"),
        dominant_rgb: row.get("dominant_rgb"),
        duplicate_count: row.try_get("duplicate_count").unwrap_or(1),
        format: row.get("format"),
        document: row.try_get::<_,Option<String>>("document").unwrap_or(None).and_then(|text|serde_json::from_str(&text).ok()),
        favorite: row.get("favorite"),
        rating: row.get("rating"),
        review_state: row.get("review_state"),
        cursor_primary: row.try_get("cursor_primary").unwrap_or(None),
        cursor_secondary: row.try_get("cursor_secondary").unwrap_or(None),
        image_group: row.try_get("image_group").unwrap_or(None),
        collection_names: row.try_get("collection_names").unwrap_or(None),
    }
}

#[derive(Clone, Debug)]
pub struct Group {
    pub id: i64,
    pub count: i64,
    pub person: Option<String>,
    pub person_id: Option<i64>,
    pub suggested_person_id: Option<i64>,
    pub samples: Vec<FaceSample>,
}

#[derive(Clone, Debug)]
pub struct Person { pub id: i64, pub name: String, pub ignored: bool }

#[derive(Clone, Debug)]
pub struct FaceSample {
    pub image_id: i64,
    pub face_id: i64,
    pub path: String,
    pub sha256:Option<String>,
    pub box_xywh: (i32, i32, i32, i32),
}

#[derive(Clone, Debug)]
pub struct Proposal {
    pub id: i64,
    pub source: String,
    pub image_id:i64,
    pub destination: String,
    pub reason: String,
    pub status: String,
}

#[derive(Clone, Debug)]
pub struct SavedView {
    pub name: String,
    pub state: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct FolderStat {
    pub path: String,
    pub count: i64,
    pub bytes: i64,
    pub unique_bytes: i64,
}

#[derive(Clone, Debug)]
pub struct DayStat {
    pub day: String,
    pub count: i64,
    pub fallback_count: i64,
}

#[derive(Clone, Debug)]
pub struct FacetValue {
    pub field: String,
    pub value: String,
    pub count: i64,
}

#[derive(Clone, Debug)]
pub struct GeoBin { pub lat: i32, pub lon: i32, pub count: i64 }

#[derive(Clone, Debug)]
pub struct ScatterBin { pub bytes_log2: i32, pub pixels_log2: i32, pub count: i64 }

#[derive(Clone, Debug)]
pub struct FolderMonth { pub folder: String, pub month: u32, pub count: i64 }

pub enum Request {
    Explore(explore::Request),
    BackfillVisuals,
    Thumbs(Vec<i64>),
    Preview(i64,Option<String>),
    LookupImage(i64),
    Groups,
    PeopleAll,
    GroupFaces { group:i64, offset:i64 },
    PersonRename { id:i64, name:String },
    PersonIgnore { id:i64, ignored:bool },
    PersonMerge { source:i64, target:i64 },
    FaceMove { ids:Vec<i64>, person_id:Option<i64> },
    FaceReject(Vec<i64>),
    FaceRestore(String),
    Tag { group_id: i64, name: String, person_id: Option<i64> },
    IgnoreGroup(i64),
    ProposalPage { status:String, search:String, after:Option<i64> },
    Review { id: i64, approved: bool },
    Stats,
    SetFavorite { id: i64, favorite: bool },
    SetRating { id: i64, rating: i16 },
    Viewed(i64),
    SavedViews,
    SaveView { name: String, state: serde_json::Value },
    DeleteView(String),
    SetReview { id: i64, state: String },
    AddTag { id: i64, tag: String },
    ProposeMove { id: i64, destination: String, reason: String },
    Details(i64),
    LookupImages(Vec<i64>),
    RefreshPhotos(Vec<i64>),
    PeopleForImage(i64),
}

pub enum Event {
    ExplorePage {
        generation: u64,
        phase: u8,
        offset: i64,
        photos: Vec<Photo>,
    },
    ExploreFacets {
        generation: u64,
        phase: u8,
        total: i64,
        tree: visuals::FolderTree,
        days: Vec<DayStat>,
        values: Vec<FacetValue>,
        geo: Vec<GeoBin>,
        scatter: Vec<ScatterBin>,
    },
    ExploreError {
        generation: u64,
        phase: u8,
        error: String,
    },
    BackfillDone,
    CatalogChanged,
    Thumb(i64, Vec<u8>,Option<String>),
    Preview(i64,Option<Vec<u8>>,Option<String>),
    ThumbMissing(i64),
    OpenPhoto(Photo),
    Groups(Vec<Group>),
    GroupFaces { group:i64, offset:i64, samples:Vec<FaceSample> },
    Saved(&'static str,i64),
    FacesEdited(Vec<i64>),
    FaceUndoReady(String),
    FacesRestored,
    PersonMerged(i64,i64),
    SaveFailed(&'static str,i64,String),
    People(Vec<Person>),
    ProposalPage { status:String,search:String,after:Option<i64>,rows:Vec<Proposal> },
    ProposalsChanged,
    Message(String),
    RequestFailed(&'static str,String),
    Stats {
        total: i64,
        embedded: i64,
        revision: String,
    },
    SavedViews(Vec<SavedView>),
    Details(serde_json::Value),
    SelectedPhotos(Vec<Photo>),
    PhotosUpdated{ids:Vec<i64>,photos:Vec<Photo>},
    PeopleForImage(i64,Vec<Person>),
    FolderMonths { generation: u64, phase: u8, rows: Vec<FolderMonth> },
}


pub(crate) fn normalize_people_filters(client:&mut impl postgres::GenericClient,state:&mut serde_json::Value,strict:bool)->anyhow::Result<()> {
    if let Some(filters)=state["filters"].as_array_mut(){for chip in filters {
        if chip["field"]=="person" {
            let name=chip["value"].as_str().unwrap_or_default();
            let matches=client.query("SELECT id FROM people WHERE name=$1",&[&name])?;
            if matches.len()!=1 {if strict {anyhow::bail!("Choose a person ID for ambiguous or missing person name: {name}");}else{continue;}}
            let id:i64=matches[0].get(0);chip["field"]="person_id".into();chip["value"]=id.to_string().into();
        }
    }}Ok(())
}
pub(crate) fn migrate_people_views(client:&mut impl postgres::GenericClient,merge:Option<(i64,i64)>)->anyhow::Result<()> {
    for row in client.query("SELECT name,state::text FROM saved_views FOR UPDATE",&[])? {
        let name:String=row.get(0);let mut state:serde_json::Value=serde_json::from_str(&row.get::<_,String>(1))?;
        normalize_people_filters(client,&mut state,false)?;
        if let Some((source,target))=merge {if let Some(filters)=state["filters"].as_array_mut(){for chip in filters {if chip["field"]=="person_id"&&chip["value"].as_str()==Some(source.to_string().as_str()){chip["value"]=target.to_string().into();}}}}
        client.execute("UPDATE saved_views SET state=$1::text::jsonb WHERE name=$2",&[&state.to_string(),&name])?;
    }Ok(())
}

fn query(client: &mut Client, request: Request, tx: &Sender<Event>) -> anyhow::Result<()> {
    match request {
        Request::Explore(_) => unreachable!("handled by worker"),
        Request::BackfillVisuals => backfill_visuals(client, tx)?,
        Request::Preview(id,revision) => {
            let exists=client.query_one("SELECT to_regclass('public.image_previews') IS NOT NULL",&[])?.get::<_,bool>(0);
            let bytes=if exists{client.query_opt("SELECT p.data FROM images i JOIN image_previews p ON p.sha256=i.sha256 AND p.version=$1 AND p.status='done' WHERE i.id=$2",&[&crate::remote::PREVIEW_VERSION,&id])?.map(|row|row.get::<_,Vec<u8>>(0))}else{None};
            let _=tx.send(Event::Preview(id,bytes,revision));
        },
        Request::Thumbs(ids) => {
            let rows = client.query(
                "SELECT i.id,t.data,i.sha256 FROM images i JOIN thumbnails t ON t.sha256=i.sha256 AND t.version=1 WHERE i.id=ANY($1)",
                &[&ids],
            )?;
            let mut found = std::collections::HashSet::new();
            for row in rows {
                let id = row.get("id");
                found.insert(id);
                let _ = tx.send(Event::Thumb(id, row.get("data"),row.get("sha256")));
            }
            for id in ids {
                if !found.contains(&id) {
                    let _ = tx.send(Event::ThumbMissing(id));
                }
            }
        }
        Request::LookupImage(id) => {
            if let Some(row) = client.query_opt(
                "SELECT id,path,sha256,(exif->'Document')::text AS document,(CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN height ELSE width END) AS width,(CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN width ELSE height END) AS height,missing,size_bytes,capture_day::text AS capture_day, \
                capture_at::text AS capture_at,camera_model,root_path,dominant_rgb,format,favorite,rating,review_state FROM images WHERE id=$1",
                &[&id],
            )? {
                let _ = tx.send(Event::OpenPhoto(photo_from_row(&row)));
            }
        }
        request @ (Request::PeopleAll|Request::GroupFaces{..}|Request::PersonRename{..}|Request::PersonIgnore{..}|Request::PersonMerge{..}|Request::FaceMove{..}|Request::FaceReject(_)|Request::FaceRestore(_)) => {
            let views_changed=matches!(&request,Request::PersonRename{..}|Request::PersonMerge{..});
            let changed=!matches!(request,Request::PeopleAll|Request::GroupFaces{..});
            let edited=match &request{Request::FaceMove{ids,..}|Request::FaceReject(ids)=>Some(ids.clone()),_=>None};
            crate::people::query(client,request,tx)?;
            if views_changed{query(client,Request::SavedViews,tx)?;}
            if let Some(ids)=edited{let _=tx.send(Event::FacesEdited(ids));}
            if changed {query(client,Request::Groups,tx)?;query(client,Request::PeopleAll,tx)?;let _=tx.send(Event::CatalogChanged);}
        },
        Request::Groups => {
            let rows = client.query(
                "WITH ranked AS (SELECT f.group_id,f.id AS face_id,f.image_id,f.x,f.y,f.width,f.height, \
                 i.path,i.sha256,p.name,f.person_id,f.suggested_person_id,row_number() OVER (PARTITION BY f.group_id ORDER BY f.id) AS sample, \
                 count(*) OVER (PARTITION BY f.group_id) AS n FROM faces f JOIN images i ON i.id=f.image_id \
                 LEFT JOIN people p ON p.id=f.person_id \
                 LEFT JOIN people sp ON sp.id=f.suggested_person_id \
                 WHERE NOT i.missing AND NOT coalesce(p.ignored,false) AND NOT coalesce(sp.ignored,false)) \
                 SELECT group_id AS id,n,name AS person,person_id,suggested_person_id,image_id,face_id,path,sha256,x,y,width,height \
                 FROM ranked WHERE sample<=4 ORDER BY n DESC,group_id,sample", &[],
            )?;
            let mut groups: Vec<Group> = Vec::new();
            for r in rows {
                let id: i64 = r.get("id");
                if groups.last().is_none_or(|group| group.id != id) {
                    groups.push(Group {
                        id,
                        count: r.get("n"),
                        person: r.get("person"),
                        person_id: r.get("person_id"),
                        suggested_person_id: r.get("suggested_person_id"),
                        samples: Vec::new(),
                    });
                }
                groups
                    .last_mut()
                    .expect("group exists")
                    .samples
                    .push(FaceSample {
                        image_id: r.get("image_id"),
                        face_id: r.get("face_id"),
                        path: r.get("path"),sha256:r.get("sha256"),
                        box_xywh: (r.get("x"), r.get("y"), r.get("width"), r.get("height")),
                    });
            }
            let _ = tx.send(Event::Groups(groups));
            let people = client.query("SELECT id,name,ignored FROM people ORDER BY ignored,lower(name),id", &[])?
                .into_iter().map(|r| Person { id: r.get("id"), name: r.get("name"), ignored:r.get("ignored") }).collect();
            let _ = tx.send(Event::People(people));
        }
        Request::Tag { group_id, name, person_id } => {
            let name = name.trim();
            anyhow::ensure!(person_id.is_some()||name.chars().count()<=200,"Person name must be at most 200 characters");
            if name.is_empty() && person_id.is_none() {
                return Ok(());
            }
            let mut transaction = client.transaction()?;
            anyhow::ensure!(transaction.query_opt("SELECT id FROM face_groups WHERE id=$1", &[&group_id])?.is_some(), "No face group {group_id}");
            let person_id: i64 = if let Some(id) = person_id {
                anyhow::ensure!(transaction.query_opt("SELECT id FROM people WHERE id=$1 AND NOT ignored", &[&id])?.is_some(), "Person is missing or ignored");
                id
            } else {
                transaction.query_one(
                    "INSERT INTO people(name) VALUES ($1) RETURNING id,ignored",
                    &[&name],
                )?.get(0)
            };
            anyhow::ensure!(!transaction.query_one("SELECT ignored FROM people WHERE id=$1", &[&person_id])?.get::<_, bool>(0), "Person is ignored");
            transaction.execute(
                "UPDATE faces SET person_id=$1,suggested_person_id=NULL WHERE group_id=$2",
                &[&person_id, &group_id],
            )?;
            transaction.commit()?;
            let _ = tx.send(Event::Message(format!(
                "Tagged group #{group_id} as person #{person_id}"
            )));
            let _ = tx.send(Event::Saved("person",group_id));
            query(client, Request::Groups, tx)?;
            let _ = tx.send(Event::CatalogChanged);
        }
        Request::IgnoreGroup(group_id) => {
            let mut transaction = client.transaction()?;
            anyhow::ensure!(transaction.query_opt("SELECT id FROM face_groups WHERE id=$1", &[&group_id])?.is_some(), "No face group {group_id}");
            let person_id: i64 = if let Some(row) = transaction.query_opt(
                "SELECT person_id FROM faces WHERE group_id=$1 AND person_id IS NOT NULL LIMIT 1", &[&group_id],
            )? { row.get(0) } else {
                transaction.query_one(
                    "INSERT INTO people(name,ignored) VALUES ('Ignored person ' || gen_random_uuid()::text,true) RETURNING id", &[],
                )?.get(0)
            };
            transaction.execute("UPDATE people SET ignored=true WHERE id=$1", &[&person_id])?;
            transaction.execute(
                "UPDATE faces SET person_id=$1,suggested_person_id=NULL WHERE group_id=$2 OR (person_id IS NULL AND suggested_person_id=$1)",
                &[&person_id, &group_id],
            )?;
            transaction.commit()?;
            let _ = tx.send(Event::Message(format!("Ignored person #{person_id}")));
            query(client, Request::Groups, tx)?;
            let _ = tx.send(Event::CatalogChanged);
        }
        Request::ProposalPage{status,search,after}=>{
            let rows=client.query("SELECT p.id,p.image_id,i.path AS source,p.destination,p.reason,p.status FROM sort_proposals p JOIN images i ON i.id=p.image_id WHERE ($1='' OR p.status=$1) AND ($2='' OR strpos(lower(i.path||' '||p.destination||' '||p.reason),lower($2))>0) AND ($3::bigint IS NULL OR p.id<$3) ORDER BY p.id DESC LIMIT 100",&[&status,&search,&after])?.into_iter().map(|r|Proposal{id:r.get("id"),image_id:r.get("image_id"),source:r.get("source"),destination:r.get("destination"),reason:r.get("reason"),status:r.get("status")}).collect();
            let _=tx.send(Event::ProposalPage{status,search,after,rows});
        },
        Request::Review { id, approved } => {
            let status = if approved { "approved" } else { "rejected" };
            client.execute(
                "UPDATE sort_proposals SET status=$1 WHERE id=$2",
                &[&status, &id],
            )?;
            let _=tx.send(Event::ProposalsChanged);
        }
        Request::Stats => {
            let row=client.query_one("SELECT count(*)::bigint AS total,count(*) FILTER(WHERE embedding IS NOT NULL AND embedding_model='open_clip:ViT-B-32:laion2b_s34b_b79k')::bigint AS embedded, \
                (SELECT string_agg(relname||':'||n_tup_ins||':'||n_tup_upd||':'||n_tup_del,',' ORDER BY relname) \
                 FROM pg_stat_all_tables WHERE schemaname='public' AND relname IN ('images','faces','people','image_tags','collections','collection_images','saved_views')) AS revision FROM images",&[])?;
            let _ = tx.send(Event::Stats {
                total: row.get("total"),
                embedded: row.get("embedded"),
                revision: row.get("revision"),
            });
        }
        Request::SetFavorite { id, favorite } => {
            client.execute("UPDATE images SET favorite=$1 WHERE id=$2", &[&favorite, &id])?;
            let _ = tx.send(Event::CatalogChanged);
        }
        Request::SetRating { id, rating } => {
            client.execute("UPDATE images SET rating=$1 WHERE id=$2", &[&rating.clamp(0,5), &id])?;
            let _ = tx.send(Event::CatalogChanged);
        }
        Request::Viewed(id) => {
            client.execute("UPDATE images SET last_viewed_at=now() WHERE id=$1", &[&id])?;
        }
        Request::SavedViews => {
            let rows = client.query("SELECT name,state::text AS state FROM saved_views ORDER BY name", &[])?;
            let views = rows.into_iter().map(|row| SavedView {
                name: row.get("name"), state: serde_json::from_str::<serde_json::Value>(row.get::<_, String>("state").as_str()).unwrap_or_default()
            }).collect();
            let _ = tx.send(Event::SavedViews(views));
        }
        Request::SaveView { name, mut state } => {
            normalize_people_filters(client,&mut state,true)?;
            let encoded = state.to_string();
            client.execute("INSERT INTO saved_views(name,state) VALUES ($1,$2::text::jsonb) \
                ON CONFLICT(name) DO UPDATE SET state=excluded.state,updated_at=now()", &[&name, &encoded])?;
            query(client, Request::SavedViews, tx)?;
        }
        Request::DeleteView(name) => {
            client.execute("DELETE FROM saved_views WHERE name=$1", &[&name])?;
            query(client, Request::SavedViews, tx)?;
        }
        Request::SetReview { id, state } => {
            anyhow::ensure!(["unreviewed", "reviewed", "flagged", "rejected"].contains(&state.as_str()), "Invalid review state");
            client.execute("UPDATE images SET review_state=$1 WHERE id=$2", &[&state, &id])?;
            let _ = tx.send(Event::CatalogChanged);
        }
        Request::AddTag { id, tag } => {
            let tag = tag.trim();
            anyhow::ensure!(!tag.is_empty() && tag.chars().count() <= 80, "Tag must be 1–80 characters");
            client.execute("INSERT INTO image_tags(image_id,tag) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&id, &tag])?;
            let _ = tx.send(Event::Saved("tag",id));
            let _ = tx.send(Event::CatalogChanged);
        }
        Request::ProposeMove { id, destination, reason } => {
            client.execute("INSERT INTO sort_proposals(image_id,destination,reason) VALUES ($1,$2,$3) \
                ON CONFLICT(image_id,destination) DO UPDATE SET reason=excluded.reason,status='pending'",
                &[&id, &destination, &reason])?;
            let _ = tx.send(Event::Saved("proposal",id));
            let _ = tx.send(Event::Message("Sort proposal recorded for review".into()));
        }
        Request::Details(id) => {
            if let Some(row) = client.query_opt("SELECT jsonb_build_object( \
                'id',id,'path',path,'root',root_path,'folder',folder_path, \
                'date_taken',capture_at,'day',capture_day,'date_source',date_source, \
                'size_bytes',size_bytes,'width',CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN height ELSE width END,'height',CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN width ELSE height END,'format',format, \
                'camera_make',camera_make,'camera_model',camera_model,'lens',lens, \
                'gps_lat',gps_lat,'gps_lon',gps_lon,'alpha',has_alpha,'animated',animated, \
                'raw',is_raw,'high_bit_depth',high_bit_depth, \
                'brightness',brightness,'sharpness',sharpness, \
                'favorite',favorite,'rating',rating,'review_state',review_state, \
                'embedding_status',embedding_status,'embedding_error',embedding_error,'face_status',face_status,'face_error',face_error, \
                'exif',exif,'generation',generation, \
                'collections',(SELECT coalesce(jsonb_agg(jsonb_build_object('id',c.id,'kind',c.kind,'name',coalesce(c.user_name,c.suggested_name))),'[]'::jsonb) FROM collection_images m JOIN collections c ON c.id=m.collection_id WHERE m.image_id=images.id AND m.source<>'excluded' AND c.status<>'dismissed'), \
                'tags',(SELECT coalesce(jsonb_agg(tag),'[]'::jsonb) FROM image_tags WHERE image_id=images.id) \
                ,'labels',(SELECT coalesce(jsonb_agg(jsonb_build_object('label',label,'score',score)),'[]'::jsonb) FROM image_labels WHERE image_id=images.id) \
                )::text AS details FROM images WHERE id=$1", &[&id])? {
                let data: String = row.get("details");
                let _ = tx.send(Event::Details(serde_json::from_str(&data)?));
            }
        }
        Request::LookupImages(ids) => {
            let rows = client.query("SELECT id,path,sha256,(exif->'Document')::text AS document,(CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN height ELSE width END) AS width,(CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN width ELSE height END) AS height,missing,size_bytes, \
                capture_day::text AS capture_day,capture_at::text AS capture_at,camera_model,root_path,dominant_rgb, \
                format,favorite,rating,review_state \
                FROM images WHERE id=ANY($1) ORDER BY capture_day DESC NULLS LAST,id DESC", &[&ids])?;
            let _ = tx.send(Event::SelectedPhotos(rows.iter().map(photo_from_row).collect()));
        }
        Request::RefreshPhotos(ids) => {
            let rows = client.query("SELECT id,path,sha256,(exif->'Document')::text AS document,(CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN height ELSE width END) AS width,(CASE WHEN metadata_version<5 AND exif->>'Orientation' IN ('5','6','7','8') THEN width ELSE height END) AS height,missing,size_bytes, \
                capture_day::text AS capture_day,capture_at::text AS capture_at,camera_model,root_path,dominant_rgb, \
                format,favorite,rating,review_state \
                FROM images WHERE id=ANY($1) ORDER BY capture_day DESC NULLS LAST,id DESC", &[&ids])?;
            let _ = tx.send(Event::PhotosUpdated{ids,photos:rows.iter().map(photo_from_row).collect()});
        }
        Request::PeopleForImage(id) => {
            let rows = client.query("SELECT DISTINCT p.id,p.name FROM faces f JOIN people p ON p.id=f.person_id \
                WHERE f.image_id=$1 AND NOT p.ignored ORDER BY p.name", &[&id])?;
            let _ = tx.send(Event::PeopleForImage(id,rows.into_iter().map(|row| Person{id:row.get(0),name:row.get(1),ignored:false}).collect()));
        }
    }
    Ok(())
}

fn backfill_visuals(client: &mut Client, tx: &Sender<Event>) -> anyhow::Result<()> {
    loop {
        let rows = client.query(
            "SELECT id,path,mtime_ns,exif->>'DateTimeOriginal' AS original, \
             exif->>'DateTimeDigitized' AS digitized,exif->>'DateTime' AS camera \
             FROM images WHERE folder_path IS NULL OR capture_day IS NULL ORDER BY id LIMIT 256",
            &[],
        )?;
        if rows.is_empty() {
            break;
        }
        let mut transaction = client.transaction()?;
        for row in rows {
            let path: String = row.get("path");
            let folder = std::path::Path::new(&path)
                .parent()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            let original: Option<String> = row.get("original");
            let digitized: Option<String> = row.get("digitized");
            let camera: Option<String> = row.get("camera");
            let camera_day = [original, digitized, camera]
                .into_iter()
                .flatten()
                .find_map(|value| {
                    value.get(..10).and_then(|date| {
                        NaiveDate::parse_from_str(date, "%Y:%m:%d")
                            .ok()
                            .map(|day| day.format("%Y-%m-%d").to_string())
                    })
                });
            let fallback = || {
                let nanos: i64 = row.get("mtime_ns");
                DateTime::<Utc>::from_timestamp(
                    nanos.div_euclid(1_000_000_000),
                    nanos.rem_euclid(1_000_000_000) as u32,
                )
                .map(|time| time.with_timezone(&Local).date_naive().to_string())
                .unwrap_or_else(|| "1970-01-01".into())
            };
            let source = if camera_day.is_some() {
                "exif"
            } else {
                "mtime"
            };
            let day = camera_day.unwrap_or_else(fallback);
            transaction.execute(
                "UPDATE images SET folder_path=$1,capture_day=$2::text::date,date_source=$3 WHERE id=$4",
                &[&folder, &day, &source, &row.get::<_, i64>("id")],
            )?;
        }
        transaction.commit()?;
    }
    for index in [
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS images_folder_path_idx ON images(folder_path text_pattern_ops)",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS images_capture_day_idx ON images(capture_day)",
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS images_path_prefix_idx ON images(path text_pattern_ops)",
    ] {
        client.batch_execute(index)?;
    }
    let _ = tx.send(Event::BackfillDone);
    Ok(())
}

#[allow(dead_code)]
pub fn start(
    dsn: String,
    ui: eframe::egui::Context,
    active: Arc<AtomicU64>,
) -> (Sender<Request>, Receiver<Event>) {
    start_catalog(dsn,ui,active,None,false)
}

pub fn start_catalog(dsn:String,ui:eframe::egui::Context,active:Arc<AtomicU64>,catalog:Option<String>,readonly:bool)->(Sender<Request>,Receiver<Event>){
    let (request_tx, request_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let (explore_tx,explore_rx)=crossbeam_channel::unbounded();
    let (other_tx,other_rx)=crossbeam_channel::unbounded();
    thread::spawn(move || {for request in request_rx {let sender=if matches!(request,Request::Explore(_)) {&explore_tx} else {&other_tx};if sender.send(request).is_err(){break;}}});
    // Ordered metadata/mutation responses; expensive exploration uses independent connections.
    for worker in 0..4 {
        let rx = if worker==0 {other_rx.clone()} else {explore_rx.clone()};
        let tx = event_tx.clone();
        let dsn = dsn.clone();
        let ui = ui.clone();
        let active = active.clone();
        let catalog=catalog.clone();
        thread::spawn(move || {
            if dsn.is_empty() {return;}
            let (mut config,tls)=match crate::connection::settings(&dsn){Ok(settings)=>settings,Err(error)=>{let _=tx.send(Event::Message(format!("Unsupported database connection settings: {error}")));ui.request_repaint();return;}};
            config.connect_timeout(std::time::Duration::from_secs(10));
            config.application_name("image-atlas viewer");
            let mut client = match config.connect(tls.clone()) {
                Ok(client) => client,
                Err(err) => {
                    let _ = tx.send(Event::Message(format!("Database unavailable: {err}")));
                    ui.request_repaint();
                    return;
                }
            };
            if let Some(ref id)=catalog {
                let identity=client.query_one("SELECT id::text FROM catalog_identity",&[]).map(|r|r.get::<_,String>(0));
                if identity.as_ref().ok()!=Some(id){let _=tx.send(Event::Message("Catalog identity mismatch".into()));return;}
            }
            if readonly {if let Err(error)=client.batch_execute("SET default_transaction_read_only=on") {let _=tx.send(Event::Message(error.to_string()));return;}}
            for request in rx {
                let operation=match &request {Request::Tag{group_id,..}=>Some(("person",*group_id)),Request::AddTag{id,..}=>Some(("tag",*id)),Request::ProposeMove{id,..}=>Some(("proposal",*id)),_=>None};
                if matches!(&request,Request::PersonRename{..}|Request::PersonIgnore{..}|Request::PersonMerge{..}|Request::FaceMove{..}|Request::FaceReject(_)|Request::FaceRestore(_)|Request::BackfillVisuals|Request::Tag{..}|Request::IgnoreGroup(_)|Request::Review{..}|Request::SetFavorite{..}|Request::SetRating{..}|Request::Viewed(_)|Request::SaveView{..}|Request::DeleteView(_)|Request::SetReview{..}|Request::AddTag{..}|Request::ProposeMove{..}) {
                    match crate::catalogs::readonly(catalog.as_deref()) {
                        Ok(false)=>{},
                        result=>{let message=result.err().unwrap_or_else(||"Catalog is read-only; assign folders before editing".into());if let Some((kind,id))=operation{let _=tx.send(Event::SaveFailed(kind,id,message.clone()));}let _=tx.send(Event::Message(message));ui.request_repaint();continue;}
                    }
                }
                match &request {
                    Request::Explore(request)
                        if request.generation != active.load(Ordering::Relaxed) =>
                    {
                        continue;
                    }
                    _ => {}
                }
                match request {
                    Request::Explore(request) => {
                        let generation = request.generation;
                        let phase = request.phase;
                        let finished = Arc::new(AtomicBool::new(false));
                        let watch_finished = finished.clone();
                        let watch_active = active.clone();
                        let cancel = client.cancel_token();
                        let tls=tls.clone();
                        thread::spawn(move || {
                            while !watch_finished.load(Ordering::Acquire) {
                                if watch_active.load(Ordering::Relaxed) != generation {
                                    let _ = cancel.cancel_query(tls);
                                    return;
                                }
                                thread::sleep(std::time::Duration::from_millis(25));
                            }
                        });
                        let result = explore::run(&mut client, request, &tx, &active);
                        finished.store(true, Ordering::Release);
                        if let Err(err) = result {
                            let _ = tx.send(Event::ExploreError {
                                generation,
                                phase,
                                error: err.to_string(),
                            });
                        }
                    }
                    request => {
                        let request_kind=match &request{Request::Groups=>"groups",Request::GroupFaces{..}=>"faces",Request::ProposalPage{..}=>"proposals",_=>"other"};
                        if let Err(err) = query(&mut client, request, &tx) {
                            if request_kind!="other"{let _=tx.send(Event::RequestFailed(request_kind,err.to_string()));}
                            if let Some((kind,id))=operation {let _=tx.send(Event::SaveFailed(kind,id,err.to_string()));}
                            let _ = tx.send(Event::Message(format!("Database error: {err}")));
                        }
                    }
                }
                ui.request_repaint();
            }
        });
    }
    (request_tx, event_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use postgres::NoTls;

    #[test]
    fn people_assignments_reuse_ids_and_ignore_groups() -> anyhow::Result<()> {
        let Ok(dsn) = std::env::var("IMAGE_CLI_TEST_DSN") else { return Ok(()); };
        let mut client = Client::connect(&dsn, NoTls)?;
        let unique = format!("viewer-person-{}", std::process::id());
        let image: i64 = client.query_one(
            "INSERT INTO images(path,root_path,size_bytes,mtime_ns) VALUES ($1,$1,1,1) RETURNING id", &[&unique],
        )?.get(0);
        let mut groups = Vec::new();
        for x in [0_i32, 10] {
            let group: i64 = client.query_one("INSERT INTO face_groups DEFAULT VALUES RETURNING id", &[])?.get(0);
            let vector = format!("[{}]", std::iter::once("1").chain(std::iter::repeat_n("0",127)).collect::<Vec<_>>().join(","));
            client.execute("INSERT INTO faces(image_id,x,y,width,height,embedding,group_id) VALUES ($1,$2,0,10,10,$3::text::halfvec,$4)", &[&image,&x,&vector,&group])?;
            groups.push(group);
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let result = (|| -> anyhow::Result<()> {
            query(&mut client, Request::Tag { group_id: groups[0], name: unique.clone(), person_id: None }, &tx)?;
            let person: i64 = client.query_one("SELECT id FROM people WHERE name=$1", &[&unique])?.get(0);
            query(&mut client, Request::Tag { group_id: groups[1], name: String::new(), person_id: Some(person) }, &tx)?;
            assert_eq!(client.query_one("SELECT count(*) FROM faces WHERE person_id=$1", &[&person])?.get::<_,i64>(0), 2);
            let events: Vec<_> = rx.try_iter().collect();
            assert!(events.into_iter().any(|event| matches!(event, Event::People(people) if people.iter().any(|p| p.id == person))));
            query(&mut client, Request::IgnoreGroup(groups[0]), &tx)?;
            assert!(client.query_one("SELECT ignored FROM people WHERE id=$1", &[&person])?.get::<_,bool>(0));
            for event in rx.try_iter() {
                match event {
                    Event::Groups(list) => assert!(list.iter().all(|g| !groups.contains(&g.id))),
                    Event::People(list) => assert!(list.iter().all(|p| p.id != person || p.ignored)),
                    _ => {}
                }
            }
            assert!(query(&mut client, Request::Tag { group_id: groups[1], name: String::new(), person_id: Some(person) }, &tx).is_err());
            Ok(())
        })();
        client.execute("DELETE FROM images WHERE id=$1", &[&image])?;
        for group in groups { client.execute("DELETE FROM face_groups WHERE id=$1", &[&group])?; }
        client.execute("DELETE FROM people WHERE name=$1", &[&unique])?;
        result
    }

    #[test]
    fn backfill_sets_folder_and_photo_day() -> anyhow::Result<()> {
        let Ok(dsn) = std::env::var("IMAGE_CLI_TEST_DSN") else {
            return Ok(());
        };
        let mut client = Client::connect(&dsn, NoTls)?;
        let path = format!(
            "C:\\viewer-backfill-{}\\trip\\photo.jpg",
            std::process::id()
        );
        let id: i64 = client.query_one(
            "INSERT INTO images(path,root_path,size_bytes,mtime_ns,exif) \
             VALUES ($1,'viewer-backfill',100,1,'{\"DateTimeOriginal\":\"2024:05:06 12:30:00\"}') RETURNING id",
            &[&path],
        )?.get(0);
        let (tx, rx) = crossbeam_channel::unbounded();
        let result = (|| -> anyhow::Result<()> {
            backfill_visuals(&mut client, &tx)?;
            anyhow::ensure!(matches!(rx.recv()?, Event::BackfillDone));
            let row = client.query_one(
                "SELECT folder_path,capture_day::text AS day,date_source FROM images WHERE id=$1",
                &[&id],
            )?;
            anyhow::ensure!(row.get::<_, String>("folder_path").ends_with("\\trip"));
            anyhow::ensure!(row.get::<_, String>("day") == "2024-05-06");
            anyhow::ensure!(row.get::<_, String>("date_source") == "exif");
            Ok(())
        })();
        client.execute("DELETE FROM images WHERE id=$1", &[&id])?;
        result
    }

    #[test]
    fn search_queries_find_metadata_people_and_vectors() -> anyhow::Result<()> {
        let Ok(dsn) = std::env::var("IMAGE_CLI_TEST_DSN") else {
            return Ok(());
        };
        let mut client = Client::connect(&dsn, NoTls)?;
        let path = format!("viewer-test-{}.png", std::process::id());
        let id: i64 = client.query_one(
            "INSERT INTO images(path,root_path,size_bytes,mtime_ns,sha256,embedding,embedding_model) \
             VALUES ($1,'viewer-test',1,1,$2,$3::text::halfvec,'open_clip:ViT-B-32:laion2b_s34b_b79k') RETURNING id",
            &[&path, &format!("test-{}", std::process::id()), &format!("[{}]", std::iter::repeat_n("0",511).chain(["1"]).collect::<Vec<_>>().join(","))],
        )?.get(0);
        let group: i64 = client
            .query_one("INSERT INTO face_groups DEFAULT VALUES RETURNING id", &[])?
            .get(0);
        let result = (|| -> anyhow::Result<()> {
            client.execute(
                "INSERT INTO image_search(image_id,search_text) VALUES ($1,'violet sunrise')",
                &[&id],
            )?;
            let person: i64 = client
                .query_one(
                    "INSERT INTO people(name) VALUES ('Viewer Test Alice') RETURNING id",
                    &[],
                )?
                .get(0);
            let face_vector = format!(
                "[{}]",
                std::iter::repeat_n("0", 127)
                    .chain(["1"])
                    .collect::<Vec<_>>()
                    .join(",")
            );
            client.execute(
                "INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id) \
                VALUES ($1,0,0,1,1,$2::text::halfvec,$3,$4)",
                &[&id, &face_vector, &group, &person],
            )?;
            let (tx, rx) = crossbeam_channel::unbounded();
            let active = AtomicU64::new(1);
            for term in ["violet", "Alice"] {
                explore::run(
                    &mut client,
                    explore::Request {
                        generation: 1,
                        phase: 0,
                        query: term.into(),
                        folder: None,
                        from_day: None,
                        to_day: None,
                        vector: None,
                        source_image_id: None,
                        show_missing: false,
                        offset: 0,
                        cursor: None,
                        include_facets: false,
                        relevance: 0.22,
                        filters: Vec::new(),
                        sort: explore::SortSpec::default(),
                        need_unique: false, need_geo: false, need_scatter: false,
                        need_folder_month: false, page_limit:200, heatmap_year: 2024,
                        duplicate_stacks: false,
                    },
                    &tx,
                    &active,
                )?;
                let Event::ExplorePage { photos, .. } = rx.recv()? else {
                    anyhow::bail!("missing list")
                };
                anyhow::ensure!(photos.iter().any(|photo| photo.id == id), "{term} missing");
            }
            let mut vector = vec![0.0; 512];
            vector[511] = 1.0;
            explore::run(
                &mut client,
                explore::Request {
                    generation: 1,
                    phase: 1,
                    query: "visual probe".into(),
                    folder: None,
                    from_day: None,
                    to_day: None,
                    vector: Some(vector),
                    source_image_id: None,
                    show_missing: false,
                    offset: 0,
                    cursor: None,
                    include_facets: false,
                    relevance: 0.22,
                    filters: Vec::new(),
                    sort: explore::SortSpec::default(),
                    need_unique: false, need_geo: false, need_scatter: false,
                    need_folder_month: false, page_limit:200, heatmap_year: 2024,
                    duplicate_stacks: false,
                },
                &tx,
                &active,
            )?;
            let Event::ExplorePage { photos, .. } = rx.recv()? else {
                anyhow::bail!("missing similar")
            };
            anyhow::ensure!(photos.iter().any(|photo| photo.id == id), "vector missing");
            Ok(())
        })();
        client.execute("DELETE FROM images WHERE id=$1", &[&id])?;
        client.execute("DELETE FROM face_groups WHERE id=$1", &[&group])?;
        client.execute("DELETE FROM people WHERE name='Viewer Test Alice'", &[])?;
        result
    }
}

#[cfg(test)] #[path="db_qa.rs"] mod qa;
