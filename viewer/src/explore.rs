use crate::db::{DayStat, Event, FacetValue, FolderMonth, FolderStat, GeoBin, ScatterBin};
use crate::visuals;
use crossbeam_channel::Sender;
use postgres::{Client, types::ToSql};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};


/// Older read-only archives cannot be migrated; normalize their display dimensions in queries.
pub(crate) fn oriented_sql(sql:&str)->String {
    let mut sql=sql.to_owned();
    for alias in ["i","source"] {
        sql=sql.replace(&format!("{alias}.width"),&format!("__{alias}_DISPLAY_W__")).replace(&format!("{alias}.height"),&format!("__{alias}_DISPLAY_H__"));
    }
    for alias in ["i","source"] {
        let rotate=format!("{alias}.metadata_version<5 AND {alias}.exif->>'Orientation' IN ('5','6','7','8')");
        sql=sql.replace(&format!("__{alias}_DISPLAY_W__"),&format!("(CASE WHEN {rotate} THEN {alias}.height ELSE {alias}.width END)"))
            .replace(&format!("__{alias}_DISPLAY_H__"),&format!("(CASE WHEN {rotate} THEN {alias}.width ELSE {alias}.height END)"));
    }
    sql
}

pub struct Request {
    pub model: crate::embedding_models::EmbeddingModel,
    pub generation: u64,
    pub phase: u8,
    pub query: String,
    pub folder: Option<String>,
    pub from_day: Option<String>,
    pub to_day: Option<String>,
    pub vector: Option<Vec<f32>>,
    pub source_image_id: Option<i64>,
    pub show_missing: bool,
    pub offset: i64,
    pub cursor: Option<PageCursor>,
    pub include_facets: bool,
    pub relevance: f64,
    pub filters: Vec<FilterChip>,
    pub sort: SortSpec,
    pub need_unique: bool,
    pub need_geo: bool,
    pub need_scatter: bool,
    pub need_folder_month: bool,
    pub page_limit:i64,
    pub heatmap_year: i32,
    pub duplicate_stacks: bool,
}

#[derive(Clone, Debug)]
pub struct PageCursor {
    pub id: i64,
    pub primary: Option<String>,
    pub secondary: Option<String>,
    pub group: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilterChip {
    pub field: String,
    pub value: String,
    pub exclude: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SortKey {
    Relevance, DateTaken, DateModified, DateIndexed, LastViewed,
    Filename, Path, Folder, Bytes, Width, Height, Pixels, Aspect,
    Format, Camera, Rating, Faces, Duplicates, FolderCount, FolderBytes, Random,
}

impl SortKey {
    pub const ALL: [Self; 21] = [Self::Relevance, Self::DateTaken, Self::DateModified,
        Self::DateIndexed, Self::LastViewed, Self::Filename, Self::Path, Self::Folder,
        Self::Bytes, Self::Width, Self::Height, Self::Pixels, Self::Aspect,
        Self::Format, Self::Camera, Self::Rating, Self::Faces, Self::Duplicates,
        Self::FolderCount, Self::FolderBytes, Self::Random];
    pub fn label(self) -> &'static str {
        match self {
            Self::Relevance => "Relevance / newest", Self::DateTaken => "Date taken",
            Self::DateModified => "Modified", Self::DateIndexed => "Indexed",
            Self::LastViewed => "Last viewed", Self::Filename => "Filename",
            Self::Path => "Path", Self::Folder => "Folder", Self::Bytes => "File size",
            Self::Width => "Width", Self::Height => "Height", Self::Pixels => "Pixels",
            Self::Aspect => "Aspect ratio", Self::Format => "Format",
            Self::Camera => "Camera", Self::Rating => "Rating", Self::Faces => "Face count",
            Self::Duplicates => "Exact duplicates", Self::Random => "Stable shuffle",
            Self::FolderCount => "Folder image count", Self::FolderBytes => "Folder total bytes",
        }
    }
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Relevance => "Text matches first, then visual similarity. When no search is active, the secondary field determines the order.",
            Self::DateTaken => "Capture timestamp from camera metadata, with file date fallback.",
            Self::DateModified => "Filesystem modification time.", Self::DateIndexed => "When the catalog last indexed the image.",
            Self::LastViewed => "When you last opened the image in this viewer; never-viewed images come last.",
            Self::Filename => "File name, case insensitive.", Self::Path => "Full file path, case insensitive.",
            Self::Folder => "Containing folder, case insensitive.", Self::Bytes => "Original file size in bytes.",
            Self::Width => "Image width in pixels.", Self::Height => "Image height in pixels.",
            Self::Pixels => "Width × height in pixels.", Self::Aspect => "Width divided by height; wide images have larger values.",
            Self::Format => "File format.", Self::Camera => "Camera model from image metadata.",
            Self::Rating => "Your zero-to-five-star rating.", Self::Faces => "Number of detected faces in the image.",
            Self::Duplicates => "Number of catalog files with the same SHA-256 content hash.",
            Self::FolderCount => "Total catalog image count in the containing folder.",
            Self::FolderBytes => "Total original-file bytes in the containing folder.",
            Self::Random => "Deterministic shuffle, stable while paging or reopening a saved view.",
        }
    }
    fn cursor_type(self) -> &'static str {
        match self {
            Self::Relevance => "jsonb",
            Self::DateTaken | Self::DateIndexed | Self::LastViewed => "timestamptz",
            Self::Aspect => "double precision",
            Self::Filename | Self::Path | Self::Folder | Self::Format | Self::Camera | Self::Random => "text",
            _ => "bigint",
        }
    }
    fn sql(self) -> &'static str {
        match self {
            Self::Relevance => "jsonb_build_array(CASE WHEN r.text_hit THEN 1 ELSE 0 END,coalesce(-r.distance,-2.0))",
            Self::DateTaken => "i.capture_at", Self::DateModified => "i.mtime_ns",
            Self::DateIndexed => "i.indexed_at", Self::LastViewed => "i.last_viewed_at",
            Self::Filename => "i.filename_sort",
            Self::Path => "lower(i.path)", Self::Folder => "lower(i.folder_path)",
            Self::Bytes => "i.size_bytes", Self::Width => "i.width", Self::Height => "i.height",
            Self::Pixels => "(i.width::bigint*i.height::bigint)",
            Self::Aspect => "(i.width::float8/nullif(i.height,0))", Self::Format => "i.format",
            Self::Camera => "i.camera_model", Self::Rating => "i.rating",
            Self::Faces => "coalesce(fc.n,0)",
            Self::Duplicates => "coalesce(dc.n,1)",
            Self::FolderCount => "coalesce(fs.n,0)",
            Self::FolderBytes => "coalesce(fs.bytes,0)",
            Self::Random => "md5(i.id::text)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum GroupBy {
    #[default]
    None, People, FaceGroup, Day, Month, Year, Folder, Camera, Format,
    Rating, Review, FaceCount, Orientation, Resolution, FileSize, Event, Trip, Place,
}

impl GroupBy {
    pub const ALL: [Self; 18] = [Self::None, Self::People, Self::FaceGroup, Self::Day,
        Self::Month, Self::Year, Self::Folder, Self::Camera, Self::Format, Self::Rating,
        Self::Review, Self::FaceCount, Self::Orientation, Self::Resolution, Self::FileSize, Self::Event, Self::Trip, Self::Place];
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No grouping", Self::People => "People", Self::FaceGroup => "Face sample sets",
            Self::Day => "Day", Self::Month => "Month", Self::Year => "Year", Self::Folder => "Folder",
            Self::Camera => "Camera", Self::Format => "Format", Self::Rating => "Rating",
            Self::Review => "Review state", Self::FaceCount => "Face count", Self::Orientation => "Orientation",
            Self::Resolution => "Resolution", Self::FileSize => "File size",
            Self::Event => "Event", Self::Trip => "Trip", Self::Place => "Place",
        }
    }
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::None => "Show one continuous list in the selected sort order.",
            Self::People => "Group by the set of confirmed people in each image. Photos of several people stay together; ignored identities are excluded. Unnamed faces and images without faces have separate groups.",
            Self::FaceGroup => "Group by the set of detected face sample IDs, including unconfirmed identities. An image appears once even if it contains several faces.",
            Self::Day | Self::Month | Self::Year => "Group by capture date, using file date when no camera date is available. Images without dates appear under Unknown date.",
            Self::Folder => "Group by the image's containing folder, without moving files.",
            Self::Camera => "Group by camera model; missing metadata appears under Unknown camera.",
            Self::Format => "Group by file format.", Self::Rating => "Group by your zero-to-five-star rating.",
            Self::Review => "Group by review state.", Self::FaceCount => "Group by number of detected faces.",
            Self::Orientation => "Separate landscape, portrait, square, and images with unknown dimensions.",
            Self::Resolution => "Separate images below 1 MP, 1–4 MP, 4–12 MP, and at least 12 MP.",
            Self::FileSize => "Separate files below 1 MiB, 1–10 MiB, 10–100 MiB, and at least 100 MiB.",
            Self::Event | Self::Trip | Self::Place => "Group by saved collections. Dismissed collections and excluded memberships are hidden; an image appears once.",
        }
    }
    fn sql(self) -> &'static str {
        match self {
            Self::None => "''::text",
            Self::Event => "coalesce((SELECT string_agg(label, ', ' ORDER BY label) FROM (SELECT coalesce(c.user_name,c.suggested_name) || ' (#' || c.id || ')' AS label FROM collection_images m JOIN collections c ON c.id=m.collection_id WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed' AND c.kind='event') names), 'No event')",
            Self::Trip => "coalesce((SELECT string_agg(label, ', ' ORDER BY label) FROM (SELECT coalesce(c.user_name,c.suggested_name) || ' (#' || c.id || ')' AS label FROM collection_images m JOIN collections c ON c.id=m.collection_id WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed' AND c.kind='trip') names), 'No trip')",
            Self::Place => "coalesce((SELECT string_agg(label, ', ' ORDER BY label) FROM (SELECT coalesce(c.user_name,c.suggested_name) || ' (#' || c.id || ')' AS label FROM collection_images m JOIN collections c ON c.id=m.collection_id WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed' AND c.kind='place') names), 'No place')",
            Self::People => "coalesce((SELECT string_agg(label, ', ' ORDER BY label) FROM (SELECT DISTINCT p.name || ' (#' || p.id || ')' AS label FROM faces f JOIN people p ON p.id=f.person_id WHERE f.image_id=i.id AND NOT p.ignored) names), CASE WHEN EXISTS(SELECT 1 FROM faces f WHERE f.image_id=i.id AND f.person_id IS NULL) THEN 'Unnamed people' WHEN EXISTS(SELECT 1 FROM faces f WHERE f.image_id=i.id) THEN 'Ignored people' ELSE 'No faces' END)",
            Self::FaceGroup => "coalesce((SELECT string_agg('Face #' || group_id, ', ' ORDER BY group_id) FROM (SELECT DISTINCT f.group_id FROM faces f LEFT JOIN people p ON p.id=f.person_id WHERE f.image_id=i.id AND NOT coalesce(p.ignored,false)) groups), 'No reviewable faces')",
            Self::Day => "coalesce(i.capture_day::text,'Unknown date')",
            Self::Month => "coalesce(to_char(i.capture_day,'YYYY-MM'),'Unknown date')",
            Self::Year => "coalesce(to_char(i.capture_day,'YYYY'),'Unknown date')",
            Self::Folder => "coalesce(i.folder_path,'Unknown folder')",
            Self::Camera => "coalesce(nullif(i.camera_model,''),'Unknown camera')",
            Self::Format => "coalesce(i.format,'Unknown format')",
            Self::Rating => "i.rating::text || ' stars'",
            Self::Review => "i.review_state",
            Self::FaceCount => "(SELECT count(*)::text || ' faces' FROM faces f WHERE f.image_id=i.id)",
            Self::Orientation => "CASE WHEN i.width IS NULL OR i.height IS NULL OR i.width<=0 OR i.height<=0 THEN 'Unknown dimensions' WHEN i.width=i.height THEN 'Square' WHEN i.width>i.height THEN 'Landscape' ELSE 'Portrait' END",
            Self::Resolution => "CASE WHEN i.width IS NULL OR i.height IS NULL OR i.width<=0 OR i.height<=0 THEN 'Unknown resolution' WHEN i.width::bigint*i.height<1000000 THEN '1 · Below 1 MP' WHEN i.width::bigint*i.height<4000000 THEN '2 · 1–4 MP' WHEN i.width::bigint*i.height<12000000 THEN '3 · 4–12 MP' ELSE '4 · 12 MP and above' END",
            Self::FileSize => "CASE WHEN i.size_bytes<1048576 THEN '1 · Below 1 MiB' WHEN i.size_bytes<10485760 THEN '2 · 1–10 MiB' WHEN i.size_bytes<104857600 THEN '3 · 10–100 MiB' ELSE '4 · 100 MiB and above' END",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SortSpec {
    pub primary: SortKey,
    pub primary_desc: bool,
    pub secondary: SortKey,
    pub secondary_desc: bool,
    #[serde(default)]
    pub group_by: GroupBy,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self { primary: SortKey::Relevance, primary_desc: true,
            secondary: SortKey::DateTaken, secondary_desc: true, group_by: GroupBy::None }
    }
}

impl SortSpec {
    fn parts(self) -> (&'static str, bool, &'static str, bool) {
        let secondary = if self.secondary == self.primary {
            "i.id" } else { self.secondary.sql() };
        (self.primary.sql(), self.primary_desc, secondary, self.secondary_desc)
    }
    fn order_sql(self) -> String {
        let (primary, primary_desc, secondary, secondary_desc) = self.parts();
        format!("{} ASC,{primary} {} NULLS LAST,{secondary} {} NULLS LAST,i.id DESC", self.group_by.sql(),
            if primary_desc { "DESC" } else { "ASC" },
            if secondary_desc { "DESC" } else { "ASC" })
    }
}

fn cursor_predicate_parts(sort: SortSpec, cursor: &PageCursor,
    values: &mut Vec<Box<dyn ToSql + Sync>>) -> String {
    let (primary,primary_desc,secondary,secondary_desc)=sort.parts();
    let first = values.len() + 1;
    values.push(Box::new(if cursor.primary.is_none() { 1_i32 } else { 0_i32 }));
    values.push(Box::new(cursor.primary.clone()));
    values.push(Box::new(if cursor.secondary.is_none() { 1_i32 } else { 0_i32 }));
    values.push(Box::new(cursor.secondary.clone()));
    values.push(Box::new(cursor.id));
    let (a, b, c, d, e) = (first, first+1, first+2, first+3, first+4);
    let primary_cmp = if primary_desc { "<" } else { ">" };
    let secondary_cmp = if secondary_desc { "<" } else { ">" };
    let argument = |index:usize, kind:&str| if kind=="jsonb" { format!("${index}::text::jsonb") }
        else { format!("(${index}::text::jsonb #>> '{{}}')::{kind}") };
    let p=argument(b,sort.primary.cursor_type());
    let secondary_type=if sort.primary==sort.secondary { "bigint" } else { sort.secondary.cursor_type() };
    let s=argument(d,secondary_type);
    // Compare with the same PostgreSQL types and collation as ORDER BY. This also
    // handles timestamp offsets and NULL ties without serializing the order to JSON.
    format!("((({primary} IS NULL)::int > ${a}::int) OR \
        (({primary} IS NULL)::int = ${a}::int AND \
          ({primary} {primary_cmp} {p} OR \
           ({primary} IS NOT DISTINCT FROM {p} AND \
             ((({secondary} IS NULL)::int > ${c}::int) OR \
              (({secondary} IS NULL)::int = ${c}::int AND \
                ({secondary} {secondary_cmp} {s} OR \
                 ({secondary} IS NOT DISTINCT FROM {s} AND i.id < ${e}::bigint))))))))")

}

fn cursor_predicate(sort: SortSpec, cursor: &PageCursor,
    values: &mut Vec<Box<dyn ToSql + Sync>>) -> String {
    let within = cursor_predicate_parts(sort,cursor,values);
    if sort.group_by == GroupBy::None { return within; }
    values.push(Box::new(cursor.group.clone().unwrap_or_default()));
    let param = values.len();
    let group = sort.group_by.sql();
    format!("(({group}) > ${param}::text OR (({group}) = ${param}::text AND {within}))")
}

const HITS: &str = "WITH visual_hits AS MATERIALIZED (\
    SELECT i.id, (i.embedding <=> $6::text::halfvec)::float8 AS distance FROM images i \
    WHERE $6::text IS NOT NULL AND i.embedding IS NOT NULL \
      AND ($8::bigint IS NULL OR i.id<>$8::bigint) \
      AND i.embedding_model='open_clip:ViT-B-32:laion2b_s34b_b79k' \
      AND ($2 OR NOT i.missing) AND ($3::text IS NULL OR i.path LIKE $3 ESCAPE '!') \
      AND ($4::text IS NULL OR i.capture_day >= $4::text::date) \
      AND ($5::text IS NULL OR i.capture_day <= $5::text::date) \
      AND (i.embedding <=> $6::text::halfvec) <= 1.0-$7::float8 \
      AND ({filter_sql}) {visual_order}), \
  text_hits AS MATERIALIZED (\
    SELECT s.image_id AS id FROM image_search s WHERE $1::text <> '' AND s.search_text ILIKE $1 ESCAPE '!' \
    UNION SELECT f.image_id AS id FROM faces f JOIN people p ON p.id=f.person_id \
      WHERE $1::text <> '' AND NOT p.ignored AND p.name ILIKE $1 ESCAPE '!' \
    UNION SELECT l.image_id AS id FROM image_labels l WHERE $1::text <> '' AND l.label ILIKE $1 ESCAPE '!' AND l.score>=0.20 \
    UNION SELECT t.image_id AS id FROM image_tags t WHERE $1::text <> '' AND t.tag ILIKE $1 ESCAPE '!' \
    UNION SELECT m.image_id AS id FROM collection_images m JOIN collections c ON c.id=m.collection_id \
      WHERE $1::text <> '' AND m.source<>'excluded' AND c.status<>'dismissed' AND coalesce(c.user_name,c.suggested_name) ILIKE $1 ESCAPE '!'), \
  hits AS MATERIALIZED (\
    SELECT i.id, false AS text_hit, NULL::float8 AS distance FROM images i \
      WHERE $1::text = '' AND $6::text IS NULL AND $8::bigint IS NULL AND ($2 OR NOT i.missing) \
        AND ($3::text IS NULL OR i.path LIKE $3 ESCAPE '!') \
        AND ($4::text IS NULL OR i.capture_day >= $4::text::date) \
        AND ($5::text IS NULL OR i.capture_day <= $5::text::date) \
    UNION ALL \
    SELECT i.id, true, NULL::float8 FROM text_hits t JOIN images i ON i.id=t.id \
      WHERE ($2 OR NOT i.missing) AND ($3::text IS NULL OR i.path LIKE $3 ESCAPE '!') \
        AND ($4::text IS NULL OR i.capture_day >= $4::text::date) \
        AND ($5::text IS NULL OR i.capture_day <= $5::text::date) \
    UNION ALL SELECT id, false, distance FROM visual_hits), \
  ranked AS (SELECT id, bool_or(text_hit) AS text_hit, min(distance) AS distance \
    FROM hits GROUP BY id) ";

fn filter_sql(chips: &[FilterChip], params: &mut Vec<Box<dyn ToSql + Sync>>) -> anyhow::Result<String> {
    let mut includes: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut excludes = Vec::new();
    for chip in chips {
        let field = chip.field.as_str();
        let value = chip.value.trim();
        if value.is_empty() { continue; }
        let predicate = match field {
            "format" | "camera" | "camera_make" | "lens" | "review" | "root" | "date_source" |
            "embedding_status" | "face_status" | "labels_status" | "tag" | "label" |
            "person" | "folder_direct" => {
                params.push(Box::new(value.to_owned()));
                let arg = format!("${}", params.len());
                match field {
                    "format" => format!("i.format={arg}"),
                    "camera" => format!("i.camera_model={arg}"),
                    "camera_make" => format!("i.camera_make={arg}"),
                    "lens" => format!("i.lens={arg}"),
                    "review" => format!("i.review_state={arg}"),
                    "root" => format!("i.root_path={arg}"),
                    "date_source" => format!("i.date_source={arg}"),
                    "embedding_status" => format!("i.embedding_status={arg}"),
                    "face_status" => format!("i.face_status={arg}"),
                    "labels_status" => format!("i.labels_status={arg}"),
                    "folder_direct" => format!("i.folder_path={arg}"),
                    "tag" => format!("EXISTS (SELECT 1 FROM image_tags t WHERE t.image_id=i.id AND t.tag={arg})"),
                    "label" => format!("EXISTS (SELECT 1 FROM image_labels l WHERE l.image_id=i.id AND l.label={arg} AND l.score>=0.20)"),
                    _ => format!("EXISTS (SELECT 1 FROM faces f JOIN people p ON p.id=f.person_id WHERE f.image_id=i.id AND NOT p.ignored AND p.name={arg})"),
                }
            }
            "size_min" | "size_max" | "rating_min" | "pixels_min" | "pixels_max" |
            "faces_min" | "faces_max" | "person_id" => {
                let number: i64 = value.parse()?;
                params.push(Box::new(number));
                let arg = format!("${}::bigint", params.len());
                match field {
                    "size_min" => format!("i.size_bytes>={arg}"),
                    "size_max" => format!("i.size_bytes<={arg}"),
                    "rating_min" => format!("i.rating>={arg}"),
                    "pixels_min" => format!("i.width::bigint*i.height::bigint>={arg}"),
                    "pixels_max" => format!("i.width::bigint*i.height::bigint<={arg}"),
                    "person_id" => format!("EXISTS (SELECT 1 FROM faces f JOIN people p ON p.id=f.person_id WHERE f.image_id=i.id AND NOT p.ignored AND p.id={arg})"),
                    "faces_min" => format!("(SELECT count(*) FROM faces f WHERE f.image_id=i.id)>={arg}"),
                    _ => format!("(SELECT count(*) FROM faces f WHERE f.image_id=i.id)<={arg}"),
                }
            }
            "brightness_min" | "brightness_max" | "sharpness_min" | "sharpness_max" |
            "aspect_min" | "aspect_max" | "lat_min" | "lat_max" | "lon_min" | "lon_max" | "lat_lt" | "lon_lt" => {
                let number: f64 = value.parse()?;
                anyhow::ensure!(number.is_finite(), "Filter must be finite");
                params.push(Box::new(number));
                let arg = format!("${}::float8", params.len());
                let (column, op) = match field {
                    "brightness_min" => ("i.brightness", ">="),
                    "brightness_max" => ("i.brightness", "<="),
                    "sharpness_min" => ("i.sharpness", ">="),
                    "sharpness_max" => ("i.sharpness", "<="),
                    "aspect_min" => ("i.width::float8/nullif(i.height,0)", ">="),
                    "aspect_max" => ("i.width::float8/nullif(i.height,0)", "<="),
                    "lat_min" => ("i.gps_lat", ">="), "lat_max" => ("i.gps_lat", "<="),
                    "lat_lt" => ("i.gps_lat", "<"), "lon_lt" => ("i.gps_lon", "<"),
                    "lon_min" => ("i.gps_lon", ">="), _ => ("i.gps_lon", "<="),
                };
                format!("({column}){op}{arg}")
            }
            "modified_from" | "modified_to" => {
                let mut day = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
                if field == "modified_to" { day = day.succ_opt().ok_or_else(|| anyhow::anyhow!("Date out of range"))?; }
                let nanos = day.and_hms_opt(0,0,0).unwrap().and_utc().timestamp_nanos_opt()
                    .ok_or_else(|| anyhow::anyhow!("Date out of range"))?;
                params.push(Box::new(nanos));
                format!("i.mtime_ns{}${}::bigint", if field == "modified_from" { ">=" } else { "<" }, params.len())
            }
            "indexed_from" | "indexed_to" => {
                let mut day = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
                if field == "indexed_to" { day = day.succ_opt().ok_or_else(|| anyhow::anyhow!("Date out of range"))?; }
                params.push(Box::new(day.to_string()));
                format!("i.indexed_at{}${}::text::date", if field == "indexed_from" { ">=" } else { "<" }, params.len())
            }
            "collection" => {
                let id:i64=value.parse()?; params.push(Box::new(id));
                format!("EXISTS(SELECT 1 FROM collection_images m JOIN collections c ON c.id=m.collection_id WHERE m.image_id=i.id AND m.collection_id=${}::bigint AND m.source<>'excluded' AND c.status<>'dismissed')",params.len())
            }
            "duplicate_of" => {
                let id: i64 = value.parse()?;
                params.push(Box::new(id));
                format!("i.sha256=(SELECT sha256 FROM images WHERE id=${}::bigint) AND i.id<>${}::bigint", params.len(), params.len())
            }
            "same_hash_of" => {
                let id: i64 = value.parse()?;
                params.push(Box::new(id));
                format!("i.sha256=(SELECT sha256 FROM images WHERE id=${}::bigint)", params.len())
            }
            "near_of" => {
                let id: i64 = value.parse()?;
                params.push(Box::new(id));
                let arg = format!("${}::bigint", params.len());
                format!("EXISTS (SELECT 1 FROM images source WHERE source.id={arg} \
                    AND source.dhash_num IS NOT NULL AND i.dhash_num IS NOT NULL \
                    AND i.id<>source.id AND \
                    abs(i.width::float8/nullif(i.height,0)-source.width::float8/nullif(source.height,0))<0.07 \
                    AND bit_count((i.dhash_num # source.dhash_num)::bit(64))<=8)")
            }
            "dominant_color" => {
                let color = i32::from_str_radix(value.trim_start_matches('#'),16)?;
                anyhow::ensure!((0..=0xffffff).contains(&color), "Color must be #RRGGBB");
                params.push(Box::new(color));
                let arg = format!("${}::int", params.len());
                format!("i.dominant_rgb IS NOT NULL AND \
                    abs(((i.dominant_rgb >> 16) & 255)-(({arg} >> 16) & 255))<=45 AND \
                    abs(((i.dominant_rgb >> 8) & 255)-(({arg} >> 8) & 255))<=45 AND \
                    abs((i.dominant_rgb & 255)-({arg} & 255))<=45")
            }
            "burst_of" | "event_of" => {
                let id: i64 = value.parse()?;
                params.push(Box::new(id));
                let arg = format!("${}::bigint", params.len());
                if field == "burst_of" {
                    format!("EXISTS (SELECT 1 FROM images source WHERE source.id={arg} \
                        AND source.capture_at IS NOT NULL AND i.capture_at IS NOT NULL \
                        AND i.camera_model IS NOT DISTINCT FROM source.camera_model \
                        AND i.id<>source.id AND abs(extract(epoch FROM i.capture_at-source.capture_at))<=10)")
                } else {
                    format!("EXISTS (SELECT 1 FROM collection_images source JOIN collections c ON c.id=source.collection_id \
                        JOIN collection_images member ON member.collection_id=c.id WHERE source.image_id={arg} \
                        AND member.image_id=i.id AND source.source<>'excluded' AND member.source<>'excluded' \
                        AND c.kind='event' AND c.status<>'dismissed')")
                }
            }
            "favorite" | "gps" | "alpha" | "animated" | "missing" | "faces" |
            "generated" | "exact_duplicates" | "raw" | "high_bit_depth" => {
                let expected: bool = value.parse()?;
                let expression = match field {
                    "favorite" => "i.favorite", "gps" => "i.gps_lat IS NOT NULL AND i.gps_lon IS NOT NULL",
                    "alpha" => "coalesce(i.has_alpha,false)",
                    "animated" => "coalesce(i.animated,false)", "missing" => "i.missing",
                    "faces" => "EXISTS (SELECT 1 FROM faces f WHERE f.image_id=i.id)",
                    "generated" => "i.generation<>'{}'::jsonb",
                    "raw" => "coalesce(i.is_raw,false)",
                    "high_bit_depth" => "coalesce(i.high_bit_depth,false)",
                    _ => "EXISTS (SELECT 1 FROM images d WHERE d.sha256=i.sha256 AND d.id<>i.id)",
                };
                format!("({expression}) IS {}", if expected { "TRUE" } else { "FALSE" })
            }
            "orientation" => match value {
                "portrait" => "i.height>i.width".into(),
                "landscape" => "i.width>i.height".into(),
                "square" => "i.width=i.height".into(),
                _ => anyhow::bail!("Invalid orientation"),
            },
            _ => anyhow::bail!("Unsupported filter: {field}"),
        };
        if chip.exclude { excludes.push(format!("({predicate}) IS NOT TRUE")); }
        else if field.ends_with("_min") || field.ends_with("_max") || field.ends_with("_from") || field.ends_with("_to") || field.ends_with("_lt") { excludes.push(predicate); }
        else { includes.entry(field).or_default().push(predicate); }
    }
    let mut clauses = excludes;
    for values in includes.into_values() { clauses.push(format!("({})", values.join(" OR "))); }
    Ok(if clauses.is_empty() { "TRUE".into() } else { clauses.join(" AND ") })
}

pub fn validate_filters(chips: &[FilterChip]) -> anyhow::Result<()> {
    filter_sql(chips, &mut Vec::new()).map(|_| ())
}

fn escape_like(value: &str) -> String {
    value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

pub fn run(
    client: &mut Client,
    request: Request,
    tx: &Sender<Event>,
    active: &AtomicU64,
) -> anyhow::Result<()> {
    let query = request.query.trim();
    let pattern = if query.is_empty() {
        String::new()
    } else {
        format!("%{}%", escape_like(query))
    };
    let folder_pattern = request
        .folder
        .as_ref()
        .map(|folder| format!("{}\\%", escape_like(folder.trim_end_matches('\\'))));
    let vector = request.vector.as_ref().map(|values| {
        format!(
            "[{}]",
            values
                .iter()
                .map(f32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
    });
    let vector = if vector.is_some() { vector } else if let Some(id) = request.source_image_id {
        let prefix = request.model.prefix();
        Some(client.query_opt(&format!("SELECT {prefix}embedding::text FROM images WHERE id=$1 AND {prefix}embedding IS NOT NULL AND {prefix}embedding_model=$2"), &[&id,&request.model.id()])?
            .ok_or_else(|| anyhow::anyhow!("Image #{id} has no compatible visual embedding; its embedding needs to be rebuilt with the current model"))?.get(0))
    } else { None };
    let has_vector = vector.is_some();
    let mut values: Vec<Box<dyn ToSql + Sync>> = vec![
        Box::new(pattern), Box::new(request.show_missing), Box::new(folder_pattern),
        Box::new(request.from_day), Box::new(request.to_day), Box::new(vector),
        Box::new(request.relevance.clamp(0.0, 1.0)),
        Box::new(request.source_image_id),
    ];
    let filter_sql = request.model.search_sql(&filter_sql(&request.filters, &mut values)?);
    let outer_filter = format!("($2 OR NOT i.missing) AND \
        ($3::text IS NULL OR i.path LIKE $3 ESCAPE '!') AND \
        ($4::text IS NULL OR i.capture_day >= $4::text::date) AND \
        ($5::text IS NULL OR i.capture_day <= $5::text::date) AND ({filter_sql})");
    let base_params = values.len();
    let visual_order = if request.phase >= 2 { String::new() }
        else { "ORDER BY i.embedding <=> $6::text::halfvec LIMIT 1000".into() };
    let hits = if query.is_empty() && request.source_image_id.is_none() && !has_vector {
        "WITH ranked AS (SELECT id,false AS text_hit,NULL::float8 AS distance FROM images \
         WHERE $1::text='' AND $6::text IS NULL AND $7::float8>=0 AND $8::bigint IS NULL) ".to_owned()
    } else {
        request.model.search_sql(&HITS.replace("{filter_sql}", &filter_sql).replace("{visual_order}", &visual_order))
    };
    let order = request.sort.order_sql();
    let sort_keys = [request.sort.primary, request.sort.secondary];
    let folder_join = if sort_keys.iter().any(|key| matches!(key, SortKey::FolderCount | SortKey::FolderBytes)) {
        "LEFT JOIN (SELECT folder_path,count(*) AS n,sum(size_bytes)::bigint AS bytes FROM images GROUP BY folder_path) fs ON fs.folder_path=i.folder_path"
    } else { "" };
    let face_join = if sort_keys.contains(&SortKey::Faces) {
        "LEFT JOIN (SELECT image_id,count(*) AS n FROM faces GROUP BY image_id) fc ON fc.image_id=i.id"
    } else { "" };
    let duplicate_join = if sort_keys.contains(&SortKey::Duplicates) {
        "LEFT JOIN (SELECT sha256,count(*) AS n FROM images WHERE sha256 IS NOT NULL GROUP BY sha256) dc ON dc.sha256=i.sha256"
    } else { "" };
    let (primary, _, secondary, _) = request.sort.parts();
    let cursor_clause = request.cursor.as_ref().map(|cursor| {
        cursor_predicate(request.sort, cursor, &mut values)
    }).unwrap_or_else(|| "TRUE".into());
    let offset = if request.cursor.is_some() { 0 } else { request.offset.max(0) };
    let group = request.sort.group_by.sql();
    let photo_columns = "i.id,i.path,i.sha256,i.width AS width,i.height AS height,i.missing,i.size_bytes, \
         i.capture_day::text AS capture_day,i.capture_at::text AS capture_at,i.camera_model,i.root_path,i.dominant_rgb, \
         i.format,(i.exif->'Document')::text AS document,i.favorite,i.rating,i.review_state, \
         (SELECT string_agg(coalesce(c.user_name,c.suggested_name), ', ' ORDER BY c.id) FROM collection_images m JOIN collections c ON c.id=m.collection_id WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed') AS collection_names";
    let page_limit=request.page_limit.max(1);
    let page_sql = if request.duplicate_stacks {
        format!("{hits}, filtered AS MATERIALIZED (SELECT i.id,i.sha256 FROM ranked r \
            JOIN images i ON i.id=r.id WHERE {outer_filter} AND i.sha256 IS NOT NULL), \
            grouped AS (SELECT sha256,max(id) AS id,count(*) AS n FROM filtered \
                GROUP BY sha256 HAVING count(*)>1) \
            SELECT {photo_columns},g.n AS duplicate_count,count(*) OVER() AS stack_total, \
                to_jsonb({primary})::text AS cursor_primary,to_jsonb({secondary})::text AS cursor_secondary,({group}) AS image_group \
            FROM grouped g JOIN images i ON i.id=g.id JOIN ranked r ON r.id=i.id {folder_join} {face_join} {duplicate_join} \
            WHERE {cursor_clause} ORDER BY {order} LIMIT {page_limit} OFFSET {offset}")
    } else { format!(
        "{hits} SELECT {photo_columns}, \
         to_jsonb({primary})::text AS cursor_primary,to_jsonb({secondary})::text AS cursor_secondary,({group}) AS image_group \
         FROM ranked r JOIN images i ON i.id=r.id {folder_join} {face_join} {duplicate_join} \
         WHERE {outer_filter} AND {cursor_clause} ORDER BY {order} \
         LIMIT {page_limit} OFFSET {offset}") };
    let params: Vec<&(dyn ToSql + Sync)> = values.iter().map(|value| value.as_ref()).collect();
    let rows = client.query(&oriented_sql(&page_sql), &params)?;
    if request.generation != active.load(Ordering::Relaxed) {
        return Ok(());
    }
    let stack_total = if request.duplicate_stacks { rows.first().map(|row| row.get::<_, i64>("stack_total")).unwrap_or(0) } else { 0 };
    let photos = rows.iter().map(crate::db::photo_from_row).collect();
    let _ = tx.send(Event::ExplorePage {
        generation: request.generation,
        phase: request.phase,
        offset: request.offset,
        photos,
    });
    if !request.include_facets || request.generation != active.load(Ordering::Relaxed) {
        return Ok(());
    }
    drop(params);
    values.truncate(base_params);
    let params: Vec<&(dyn ToSql + Sync)> = values.iter().map(|value| value.as_ref()).collect();
    let unique_expr = if request.need_unique {
        "row_number() OVER (PARTITION BY i.folder_path,coalesce(i.sha256,i.id::text) ORDER BY i.id)"
    } else { "1::bigint" };
    let geo_expr = if request.need_geo {
        "CASE WHEN i.gps_lat BETWEEN -90 AND 90 AND i.gps_lon BETWEEN -180 AND 180 THEN least(89,floor((i.gps_lat+90)/2)::int) END AS lat_bin, CASE WHEN i.gps_lat BETWEEN -90 AND 90 AND i.gps_lon BETWEEN -180 AND 180 THEN least(179,floor((i.gps_lon+180)/2)::int) END AS lon_bin"
    } else { "NULL::int AS lat_bin,NULL::int AS lon_bin" };
    let scatter_expr = if request.need_scatter {
        "floor(log(2,greatest(i.size_bytes,1)))::int AS bytes_bin, \
         floor(log(2,greatest(coalesce(i.width::bigint*i.height::bigint,1),1)))::int AS pixels_bin"
    } else { "NULL::int AS bytes_bin,NULL::int AS pixels_bin" };
    let facet_sql = format!(
        "{hits} SELECT GROUPING(x.folder_path) AS folder_group, \
         GROUPING(x.capture_day) AS day_group,GROUPING(x.format) AS format_group, \
         GROUPING(x.camera_model) AS camera_group,GROUPING(x.review_state) AS review_group, \
         GROUPING(x.lat_bin) AS lat_group,GROUPING(x.bytes_bin) AS bytes_group, \
         x.folder_path,x.capture_day::text AS day,x.format,x.camera_model,x.review_state, \
         x.lat_bin,x.lon_bin,x.bytes_bin,x.pixels_bin, \
         count(*) AS n,coalesce(sum(x.size_bytes),0)::bigint AS bytes, \
         coalesce(sum(x.size_bytes) FILTER (WHERE x.copy_number=1),0)::bigint AS unique_bytes, \
         count(*) FILTER (WHERE x.date_source='mtime') AS fallback_n \
         FROM (SELECT i.folder_path,i.capture_day,i.date_source,i.size_bytes,i.format,i.camera_model,i.review_state, \
               {geo_expr},{scatter_expr},{unique_expr} AS copy_number \
               FROM ranked r JOIN images i ON i.id=r.id WHERE {outer_filter}) x \
         GROUP BY GROUPING SETS ((x.folder_path),(x.capture_day),(x.format),(x.camera_model),(x.review_state), \
            (x.lat_bin,x.lon_bin),(x.bytes_bin,x.pixels_bin))"
    );
    let rows = client.query(&oriented_sql(&facet_sql), &params)?;
    if request.generation != active.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut folders = Vec::new();
    let mut days = Vec::new();
    let mut facet_values = Vec::new();
    let mut geo = Vec::new();
    let mut scatter = Vec::new();
    let mut total = 0;
    for row in rows {
        let count: i64 = row.get("n");
        if row.get::<_, i32>("folder_group") == 0 {
            total += count;
            if let Some(path) = row.get::<_, Option<String>>("folder_path") {
                folders.push(FolderStat {
                    path,
                    count,
                    bytes: row.get("bytes"),
                    unique_bytes: row.get("unique_bytes"),
                });
            }
        } else if row.get::<_, i32>("day_group") == 0 {
            let Some(day) = row.get::<_, Option<String>>("day") else { continue; };
            days.push(DayStat {
                day,
                count,
                fallback_count: row.get("fallback_n"),
            });
        } else if row.get::<_, i32>("lat_group") == 0 {
            if let (Some(lat), Some(lon)) = (row.get::<_, Option<i32>>("lat_bin"), row.get::<_, Option<i32>>("lon_bin")) {
                geo.push(GeoBin { lat, lon, count });
            }
        } else if row.get::<_, i32>("bytes_group") == 0 {
            if let (Some(bytes_log2), Some(pixels_log2)) = (row.get::<_, Option<i32>>("bytes_bin"), row.get::<_, Option<i32>>("pixels_bin")) {
                scatter.push(ScatterBin { bytes_log2, pixels_log2, count });
            }
        } else {
            let (field, value) = if row.get::<_, i32>("format_group") == 0 {
                ("format", row.get::<_, Option<String>>("format"))
            } else if row.get::<_, i32>("camera_group") == 0 {
                ("camera", row.get::<_, Option<String>>("camera_model"))
            } else if row.get::<_, i32>("review_group") == 0 {
                ("review", row.get::<_, Option<String>>("review_state"))
            } else { continue };
            if let Some(value) = value {
                facet_values.push(FacetValue { field: field.into(), value, count });
            }
        }
    }
    let _ = tx.send(Event::ExploreFacets {
        generation: request.generation,
        phase: request.phase,
        total: if request.duplicate_stacks { stack_total } else { total },
        tree: visuals::FolderTree::new(&folders),
        days,
        values: facet_values,
        geo,
        scatter,
    });
    if request.need_folder_month && request.generation == active.load(Ordering::Relaxed) {
        values.push(Box::new(request.heatmap_year));
        let year_param = values.len();
        let heatmap_sql = format!("{hits} WITH_PLACEHOLDER");
        let heatmap_sql = heatmap_sql.replace("WITH_PLACEHOLDER", &format!(
            " ,filtered AS MATERIALIZED (SELECT i.folder_path,i.capture_day FROM ranked r \
                JOIN images i ON i.id=r.id WHERE {outer_filter} \
                AND i.capture_day>=make_date(${year_param}::int,1,1) \
                AND i.capture_day<make_date(${year_param}::int+1,1,1)), \
              top_folders AS (SELECT folder_path FROM filtered GROUP BY folder_path \
                ORDER BY count(*) DESC LIMIT 12) \
              SELECT f.folder_path,extract(month FROM f.capture_day)::int AS month,count(*) AS n \
              FROM filtered f JOIN top_folders t USING(folder_path) \
              GROUP BY f.folder_path,month ORDER BY f.folder_path,month"));
        let params: Vec<&(dyn ToSql + Sync)> = values.iter().map(|value| value.as_ref()).collect();
        let rows = client.query(&oriented_sql(&heatmap_sql), &params)?;
        let rows = rows.into_iter().map(|row| FolderMonth {
            folder: row.get("folder_path"), month: row.get::<_, i32>("month") as u32,
            count: row.get("n"),
        }).collect();
        let _ = tx.send(Event::FolderMonths { generation: request.generation, phase: request.phase, rows });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Request, escape_like, run};
    use crate::db::Event;
    use crossbeam_channel::unbounded;
    use postgres::{Client, NoTls};
    use std::sync::atomic::AtomicU64;

    fn result_page(client: &mut Client, root: &str, sort: super::SortSpec,
        cursor: Option<super::PageCursor>, stacks: bool) -> anyhow::Result<Vec<crate::db::Photo>> {
        let (tx, rx) = unbounded();
        run(client, Request {
            model: Default::default(), generation: 9, phase: 0, query: String::new(), folder: Some(root.into()),
            from_day: None, to_day: None, vector: None, source_image_id: None, show_missing: false,
            offset: 0, cursor, include_facets: false, relevance: 0.22, filters: Vec::new(), sort,
            need_unique: false, need_geo: false, need_scatter: false, need_folder_month: false,
            page_limit:200, heatmap_year: 2026, duplicate_stacks: stacks,
        }, &tx, &AtomicU64::new(9))?;
        match rx.recv()? { Event::ExplorePage { photos, .. } => Ok(photos), _ => anyhow::bail!("missing image page") }
    }

    #[test]
    fn every_sort_and_group_pages_without_losing_images() -> anyhow::Result<()> {
        let Ok(dsn) = std::env::var("IMAGE_CLI_TEST_DSN") else { return Ok(()); };
        let mut client = Client::connect(&dsn, NoTls)?;
        let root = format!("sort-group-test-{}", std::process::id());
        let group: i64 = client.query_one("INSERT INTO face_groups DEFAULT VALUES RETURNING id", &[])?.get(0);
        let person: i64 = client.query_one("INSERT INTO people(name) VALUES ($1) RETURNING id", &[&root])?.get(0);
        let mut expected_ids = std::collections::HashSet::new();
        for n in 0..270_i32 {
            let id: i64 = client.query_one(
                "INSERT INTO images(path,root_path,folder_path,filename_sort,size_bytes,mtime_ns,sha256,width,height,format,camera_model,capture_day,capture_at,rating) \
                 VALUES ($1,$2,$2,$3,$4,$4,$5,$6,1000,$7,$8,CASE WHEN $9=0 THEN NULL ELSE date '2026-01-01'+$9 END, \
                 CASE WHEN $9=0 THEN NULL ELSE timestamp '2026-01-01'+$9*interval '1 day' END,$10) RETURNING id",
                &[&format!("{root}\\image-{n:03}.png"),&root,&format!("image-{n:03}.png"),
                    &(1000_i64+i64::from(n)),&format!("{root}-{}",n/2),&(n+500),
                    &if n%2==0 { "PNG" } else { "JPEG" }, &if n%3==0 { Some("Camera A") } else { None }, &(n%4), &((n%6) as i16)],
            )?.get(0);
            expected_ids.insert(id);
            if n%2==0 {
                let vector = format!("[{}]",std::iter::once("1").chain(std::iter::repeat_n("0",127)).collect::<Vec<_>>().join(","));
                client.execute("INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id) VALUES ($1,0,0,10,10,$2::text::halfvec,$3,$4)", &[&id,&vector,&group,&person])?;
            }
        }
        let result = (|| -> anyhow::Result<()> {
            for grouping in super::GroupBy::ALL {
                for descending in [false,true] {
                    let sort = super::SortSpec { primary: super::SortKey::Bytes, primary_desc: descending,
                        secondary: super::SortKey::Filename, secondary_desc: false, group_by: grouping };
                    let mut all = result_page(&mut client,&root,sort,None,false)?;
                    anyhow::ensure!(all.len()==200,"first page length for {grouping:?}");
                    let last = all.last().unwrap();
                    let cursor = super::PageCursor { id:last.id, primary:last.cursor_primary.clone(), secondary:last.cursor_secondary.clone(), group:last.image_group.clone() };
                    all.extend(result_page(&mut client,&root,sort,Some(cursor),false)?);
                    anyhow::ensure!(all.len()==270,"missing images for {grouping:?}");
                    anyhow::ensure!(all.iter().map(|p|p.id).collect::<std::collections::HashSet<_>>()==expected_ids,"duplicate or lost images");
                    let rows=crate::grouped_rows(&all,3);
                    let covered:Vec<_>=rows.iter().flat_map(|row|row.start..row.end).collect();
                    anyhow::ensure!(covered==(0..all.len()).collect::<Vec<_>>(),"grouped grid loses images");
                    anyhow::ensure!(rows.iter().all(|row|row.end-row.start<=3 && all[row.start..row.end].iter().all(|p|p.image_group.as_deref()==Some(row.label.as_str()))),"mixed group row");
                    for pair in all.windows(2) {
                        let (a,b)=(&pair[0],&pair[1]);
                        anyhow::ensure!(a.image_group<=b.image_group,"group order");
                        if a.image_group==b.image_group { anyhow::ensure!(if descending { a.size_bytes>=b.size_bytes } else { a.size_bytes<=b.size_bytes },"sort order in group"); }
                    }
                    let stacks = result_page(&mut client,&root,sort,None,true)?;
                    anyhow::ensure!(stacks.len()==135 && stacks.iter().all(|p|p.duplicate_count==2),"stack grouping/sorting");
                }
            }
            for key in super::SortKey::ALL {
                for descending in [false,true] {
                    let sort = super::SortSpec { primary:key, primary_desc:descending, ..Default::default() };
                    let mut all=result_page(&mut client,&root,sort,None,false)?;
                    let last=all.last().unwrap();
                    let cursor=super::PageCursor { id:last.id, primary:last.cursor_primary.clone(), secondary:last.cursor_secondary.clone(), group:last.image_group.clone() };
                    all.extend(result_page(&mut client,&root,sort,Some(cursor),false)?);
                    anyhow::ensure!(all.len()==270 && all.iter().map(|p|p.id).collect::<std::collections::HashSet<_>>()==expected_ids,"paging for {key:?} {descending}");
                }
            }
            let people=result_page(&mut client,&root,super::SortSpec { group_by:super::GroupBy::People, ..Default::default() },None,false)?;
            anyhow::ensure!(people.iter().any(|p|p.image_group.as_deref()==Some("No faces")) && people.iter().any(|p|p.image_group.as_ref().is_some_and(|g|g.contains(&format!("(#{person})")))),"person groups");
            Ok(())
        })();
        client.execute("DELETE FROM images WHERE root_path=$1",&[&root])?;
        client.execute("DELETE FROM face_groups WHERE id=$1",&[&group])?;
        client.execute("DELETE FROM people WHERE id=$1",&[&person])?;
        result
    }

    #[test]
    fn reference_vector_without_text_returns_only_similar_images() -> anyhow::Result<()> {
        let Ok(dsn) = std::env::var("IMAGE_CLI_TEST_DSN") else { return Ok(()); };
        let mut client = Client::connect(&dsn, NoTls)?;
        let root = format!("reference-search-{}",std::process::id());
        let mut ids = Vec::new();
        for (index, first) in [1.0_f32,0.8].into_iter().enumerate() {
            let mut vector=vec![0.0_f32;512];vector[0]=first;vector[1]=(1.0-first*first).sqrt();
            let value=format!("[{}]",vector.iter().map(f32::to_string).collect::<Vec<_>>().join(","));
            ids.push(client.query_one("INSERT INTO images(path,root_path,size_bytes,mtime_ns,embedding,embedding_model) VALUES ($1,$2,1,1,$3::text::halfvec,'open_clip:ViT-B-32:laion2b_s34b_b79k') RETURNING id",&[&format!("{root}/{index}.png"),&root,&value])?.get::<_,i64>(0));
            let mut gemma=vec![0.0_f32;512];gemma[0]=if index==0 {0.8}else{1.0};gemma[1]=(1.0-gemma[0]*gemma[0]).sqrt();
            let gemma=format!("[{}]",gemma.iter().map(f32::to_string).collect::<Vec<_>>().join(","));
            client.execute("UPDATE images SET gemma_embedding=$1::text::halfvec,gemma_embedding_model='google:embeddinggemma-2:512' WHERE id=$2",&[&gemma,&ids[index]])?;
        }
        let result=(|| -> anyhow::Result<()> {
            for (model,expected) in [(crate::embedding_models::EmbeddingModel::OpenClip,0),(crate::embedding_models::EmbeddingModel::EmbeddingGemma2,1)] { for phase in [1,2] {
                let (tx,rx)=unbounded();let mut vector=vec![0.0;512];vector[0]=1.0;
                run(&mut client,Request {
                    model, generation:9,phase,query:String::new(),folder:None,from_day:None,to_day:None,
                    vector:Some(vector),source_image_id:None,show_missing:false,offset:0,cursor:None,
                    include_facets:true,relevance:0.9,filters:vec![super::FilterChip{field:"root".into(),value:root.clone(),exclude:false}],
                    sort:super::SortSpec::default(),need_unique:false,need_geo:false,need_scatter:false,need_folder_month:false,
                    page_limit:200,heatmap_year:2026,duplicate_stacks:false,
                },&tx,&AtomicU64::new(9))?;
                let events:Vec<_>=rx.try_iter().collect();
                assert!(events.iter().any(|event|matches!(event,Event::ExplorePage{photos,..} if photos.len()==1&&photos[0].id==ids[expected])));
                assert!(events.iter().any(|event|matches!(event,Event::ExploreFacets{total,..} if *total==1)));
            }}
            Ok(())
        })();
        client.execute("DELETE FROM images WHERE id=ANY($1)",&[&ids])?;result
    }

    #[test]
    fn saved_sorts_before_grouping_remain_readable() {
        let saved = r#"{"primary":"Bytes","primary_desc":false,"secondary":"Filename","secondary_desc":true}"#;
        let sort:super::SortSpec=serde_json::from_str(saved).unwrap();
        assert_eq!(sort.group_by,super::GroupBy::None);
        assert_eq!(sort,serde_json::from_str(&serde_json::to_string(&sort).unwrap()).unwrap());
    }

    #[test]
    fn like_special_characters_are_literal() {
        assert_eq!(escape_like("A!_%"), "A!!!_!%");
    }

    #[test]
    #[ignore = "requires explicitly seeded imagecli_bench database"]
    fn benchmark_million_image_explore() -> anyhow::Result<()> {
        let dsn = std::env::var("IMAGE_CLI_BENCH_DSN")?;
        anyhow::ensure!(dsn.ends_with("/imagecli_bench"), "benchmark must use imagecli_bench");
        let mut client = Client::connect(&dsn, NoTls)?;
        let (tx, rx) = unbounded();
        let started = std::time::Instant::now();
        let request = Request {
            model: Default::default(), generation: 1, phase: 0, query: String::new(), folder: None,
            from_day: None, to_day: None, vector: None, source_image_id: None,
            show_missing: false, offset: 0, cursor: None, include_facets: true,
            relevance: 0.22, filters: Vec::new(), sort: super::SortSpec::default(),
            need_unique: false, need_geo: false, need_scatter: false,
            need_folder_month: false, page_limit:200, heatmap_year: 2024,
            duplicate_stacks: false,
        };
        let worker = std::thread::spawn(move || {
            let active = AtomicU64::new(1);
            run(&mut client, request, &tx, &active)
        });
        let Event::ExplorePage { photos, .. } = rx.recv()? else { anyhow::bail!("missing page") };
        let page_ms = started.elapsed().as_secs_f64()*1000.0;
        let Event::ExploreFacets { total, .. } = rx.recv()? else { anyhow::bail!("missing facets") };
        worker.join().map_err(|_| anyhow::anyhow!("benchmark worker panicked"))??;
        println!("viewer query: {} images, {} page rows, {:.1} ms first page, {:.1} ms all facets",
            total,photos.len(),page_ms,started.elapsed().as_secs_f64()*1000.0);
        anyhow::ensure!(total>=1_000_000 && photos.len()==200);
        Ok(())
    }

    #[test]
    fn linked_facets_match_pages_and_filters() -> anyhow::Result<()> {
        let Ok(dsn) = std::env::var("IMAGE_CLI_TEST_DSN") else {
            return Ok(());
        };
        let mut client = Client::connect(&dsn, NoTls)?;
        let root = format!("C:\\viewer-facets-{}", std::process::id());
        let first = format!("{root}\\A\\dog.png");
        let second = format!("{root}\\B\\cat.png");
        for (path, folder, bytes, day) in [
            (&first, format!("{root}\\A"), 10_i64, "2024-01-03"),
            (&second, format!("{root}\\B"), 20_i64, "2025-02-04"),
        ] {
            let id: i64 = client.query_one(
                "INSERT INTO images(path,root_path,folder_path,capture_day,date_source,size_bytes,mtime_ns) \
                 VALUES ($1,$2,$3,$4::text::date,'exif',$5,1) RETURNING id",
                &[path, &root, &folder, &day, &bytes],
            )?.get(0);
            client.execute(
                "INSERT INTO image_search(image_id,search_text) VALUES ($1,$2)",
                &[&id, path],
            )?;
        }
        let result = (|| -> anyhow::Result<()> {
            let (tx, rx) = unbounded();
            let active = AtomicU64::new(1);
            run(
                &mut client,
                Request {
                    model: Default::default(), generation: 1,
                    phase: 0,
                    query: String::new(),
                    folder: Some(root.clone()),
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
                    sort: super::SortSpec::default(),
                    need_unique: false, need_geo: false, need_scatter: false,
                    need_folder_month: false, page_limit:200, heatmap_year: 2024,
                    duplicate_stacks: false,
                },
                &tx,
                &active,
            )?;
            let Event::ExplorePage { photos, .. } = rx.recv()? else {
                anyhow::bail!("missing page")
            };
            let Event::ExploreFacets {
                total, tree, days, ..
            } = rx.recv()?
            else {
                anyhow::bail!("missing facets")
            };
            anyhow::ensure!(
                photos.len() == 2
                    && total == 2
                    && tree.tiles(Some(&root), crate::visuals::Metric::Bytes).len() == 2
                    && days.len() == 2
            );
            let first_id = photos[0].id;
            let cursor = super::PageCursor {
                id: photos[0].id,
                primary: photos[0].cursor_primary.clone(),
                secondary: photos[0].cursor_secondary.clone(), group: photos[0].image_group.clone(),
            };
            run(
                &mut client,
                Request {
                    model: Default::default(), generation: 1,
                    phase: 0,
                    query: String::new(),
                    folder: Some(root.clone()),
                    from_day: None,
                    to_day: None,
                    vector: None,
                    source_image_id: None,
                    show_missing: false,
                    offset: 1,
                    cursor: Some(cursor),
                    include_facets: false,
                    relevance: 0.22,
                    filters: Vec::new(),
                    sort: super::SortSpec::default(),
                    need_unique: false, need_geo: false, need_scatter: false,
                    need_folder_month: false, page_limit:200, heatmap_year: 2024,
                    duplicate_stacks: false,
                },
                &tx,
                &active,
            )?;
            let Event::ExplorePage { photos, offset, .. } = rx.recv()? else {
                anyhow::bail!("missing second page")
            };
            anyhow::ensure!(offset == 1 && photos.len() == 1 && photos[0].id != first_id);
            run(
                &mut client,
                Request {
                    model: Default::default(), generation: 1,
                    phase: 0,
                    query: "dog".into(),
                    folder: Some(root.clone()),
                    from_day: Some("2024-01-01".into()),
                    to_day: Some("2024-01-31".into()),
                    vector: None,
                    source_image_id: None,
                    show_missing: false,
                    offset: 0,
                    cursor: None,
                    include_facets: true,
                    relevance: 0.22,
                    filters: Vec::new(),
                    sort: super::SortSpec::default(),
                    need_unique: false, need_geo: false, need_scatter: false,
                    need_folder_month: false, page_limit:200, heatmap_year: 2024,
                    duplicate_stacks: false,
                },
                &tx,
                &active,
            )?;
            let Event::ExplorePage { photos, .. } = rx.recv()? else {
                anyhow::bail!("missing filtered page")
            };
            let Event::ExploreFacets {
                total, tree, days, ..
            } = rx.recv()?
            else {
                anyhow::bail!("missing filtered facets")
            };
            anyhow::ensure!(
                photos.len() == 1
                    && total == 1
                    && tree.tiles(Some(&root), crate::visuals::Metric::Bytes)[0].bytes == 10
                    && days[0].day == "2024-01-03"
            );
            let source_id: i64 = client.query_one("SELECT id FROM images WHERE path=$1", &[&first])?.get(0);
            client.execute("UPDATE images SET dhash_num=15,width=100,height=100,format='PNG' WHERE path=$1", &[&first])?;
            client.execute("UPDATE images SET dhash_num=7,width=200,height=200,format='JPG' WHERE path=$1", &[&second])?;
            run(&mut client, Request {
                model: Default::default(), generation: 1, phase: 0, query: String::new(), folder: Some(root.clone()),
                from_day: None, to_day: None, vector: None, source_image_id: None,
                show_missing: false, offset: 0, cursor: None, include_facets: false,
                relevance: 0.22,
                filters: vec![super::FilterChip { field: "near_of".into(),
                    value: source_id.to_string(), exclude: false }],
                sort: super::SortSpec { primary: super::SortKey::Bytes,
                    primary_desc: false, secondary: super::SortKey::DateTaken, secondary_desc: true, group_by: super::GroupBy::None },
                need_unique: false, need_geo: false, need_scatter: false,
                need_folder_month: false, page_limit:200, heatmap_year: 2024,
                duplicate_stacks: false,
            }, &tx, &active)?;
            let Event::ExplorePage { photos, .. } = rx.recv()? else { anyhow::bail!("missing near matches") };
            anyhow::ensure!(photos.len()==1 && photos[0].path==second);
            client.execute("UPDATE images SET sha256='same-hash' WHERE root_path=$1", &[&root])?;
            run(&mut client, Request {
                model: Default::default(), generation: 1, phase: 0, query: String::new(), folder: Some(root.clone()),
                from_day: None, to_day: None, vector: None, source_image_id: None,
                show_missing: false, offset: 0, cursor: None, include_facets: true,
                relevance: 0.22, filters: Vec::new(), sort: super::SortSpec::default(),
                need_unique: false, need_geo: false, need_scatter: false,
                need_folder_month: false, page_limit:200, heatmap_year: 2024, duplicate_stacks: true,
            }, &tx, &active)?;
            let Event::ExplorePage { photos, .. } = rx.recv()? else { anyhow::bail!("missing duplicate stack") };
            anyhow::ensure!(photos.len()==1 && photos[0].duplicate_count==2);
            let Event::ExploreFacets { total, .. } = rx.recv()? else { anyhow::bail!("missing duplicate facets") };
            anyhow::ensure!(total==1);
            run(&mut client, Request {
                model: Default::default(), generation: 1, phase: 0, query: String::new(), folder: Some(root.clone()),
                from_day: None, to_day: None, vector: None, source_image_id: None,
                show_missing: false, offset: 0, cursor: None, include_facets: true,
                relevance: 0.22, filters: Vec::new(), sort: super::SortSpec::default(),
                need_unique: true, need_geo: true, need_scatter: true,
                need_folder_month: true, page_limit:200, heatmap_year: 2024,
                duplicate_stacks: false,
            }, &tx, &active)?;
            let Event::ExplorePage { .. } = rx.recv()? else { anyhow::bail!("missing heatmap page") };
            let Event::ExploreFacets { geo, scatter, .. } = rx.recv()? else { anyhow::bail!("missing heatmap facets") };
            anyhow::ensure!(geo.is_empty() && !scatter.is_empty());
            let Event::FolderMonths { rows, .. } = rx.recv()? else { anyhow::bail!("missing folder months") };
            anyhow::ensure!(rows.len()==1 && rows[0].month==1 && rows[0].count==1);
            Ok(())
        })();
        client.execute("DELETE FROM images WHERE root_path=$1", &[&root])?;
        result
    }
}
