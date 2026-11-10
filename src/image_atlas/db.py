"""Database schema and small persistence helpers. Originals never enter PostgreSQL."""
import os
from pathlib import Path
from urllib.parse import quote

import psycopg
from psycopg.rows import dict_row
from dotenv import dotenv_values

# Read local credentials without exporting them to model-worker environments.
_local_settings = dotenv_values(Path(__file__).resolve().parents[2] / '.env')
_password = os.environ.get('IMAGEATLAS_DB_PASSWORD', _local_settings.get('IMAGEATLAS_DB_PASSWORD'))
_authentication = 'imagecli' + (':' + quote(_password, safe='') if _password else '')
DEFAULT_DSN = f'postgresql://{_authentication}@127.0.0.1:54329/imagecli'

SCHEMA = """
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE TABLE IF NOT EXISTS images (
 id bigserial PRIMARY KEY,
 path text NOT NULL UNIQUE,
 root_path text NOT NULL,
 size_bytes bigint NOT NULL,
 mtime_ns bigint NOT NULL,
 sha256 text,
 dhash text,
 width integer,
 height integer,
 format text,
 exif jsonb NOT NULL DEFAULT '{}',
 generation jsonb NOT NULL DEFAULT '{}',
 indexed_at timestamptz NOT NULL DEFAULT now(),
 missing boolean NOT NULL DEFAULT false,
 ocr_text text,
 ocr_model text,
 ocr_status text NOT NULL DEFAULT 'pending',
 ocr_error text,
 ocr_at timestamptz,
 embedding halfvec(512),
 embedding_model text,
 embedding_status text NOT NULL DEFAULT 'pending',
 embedding_error text,
 face_status text NOT NULL DEFAULT 'pending',
 face_error text,
 folder_path text,
 capture_day date,
 date_source text
);
ALTER TABLE images ADD COLUMN IF NOT EXISTS capture_at timestamp;
ALTER TABLE images ADD COLUMN IF NOT EXISTS capture_offset_minutes smallint;
ALTER TABLE images ADD COLUMN IF NOT EXISTS metadata_version smallint NOT NULL DEFAULT 0;
-- Version 4 stored encoded dimensions. Version 5 stores displayed dimensions.
UPDATE images SET width=height,height=width WHERE metadata_version=4
 AND exif->>'Orientation' IN ('5','6','7','8');
UPDATE images SET metadata_version=5 WHERE metadata_version=4;
ALTER TABLE images ADD COLUMN IF NOT EXISTS camera_make text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS camera_model text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS lens text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS gps_lat double precision;
ALTER TABLE images ADD COLUMN IF NOT EXISTS gps_lon double precision;
ALTER TABLE images ADD COLUMN IF NOT EXISTS has_alpha boolean;
ALTER TABLE images ADD COLUMN IF NOT EXISTS animated boolean;
ALTER TABLE images ADD COLUMN IF NOT EXISTS is_raw boolean;
ALTER TABLE images ADD COLUMN IF NOT EXISTS high_bit_depth boolean;
ALTER TABLE images ADD COLUMN IF NOT EXISTS dhash_num bigint;
ALTER TABLE images ADD COLUMN IF NOT EXISTS filename_sort text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS dominant_rgb integer;
ALTER TABLE images ADD COLUMN IF NOT EXISTS brightness real;
ALTER TABLE images ADD COLUMN IF NOT EXISTS sharpness real;
ALTER TABLE images ADD COLUMN IF NOT EXISTS favorite boolean NOT NULL DEFAULT false;
ALTER TABLE images ADD COLUMN IF NOT EXISTS rating smallint NOT NULL DEFAULT 0;
ALTER TABLE images ADD COLUMN IF NOT EXISTS review_state text NOT NULL DEFAULT 'unreviewed';
ALTER TABLE images ADD COLUMN IF NOT EXISTS last_viewed_at timestamptz;
ALTER TABLE images ADD COLUMN IF NOT EXISTS labels_status text NOT NULL DEFAULT 'pending';
ALTER TABLE images ADD COLUMN IF NOT EXISTS labels_version smallint NOT NULL DEFAULT 0;
ALTER TABLE images ADD COLUMN IF NOT EXISTS embedding_error text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS gemma_embedding halfvec(512);
ALTER TABLE images ADD COLUMN IF NOT EXISTS gemma_embedding_model text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS gemma_embedding_status text NOT NULL DEFAULT 'pending';
ALTER TABLE images ADD COLUMN IF NOT EXISTS gemma_embedding_error text;
UPDATE images SET gemma_embedding_status='skipped',gemma_embedding_error=embedding_error
 WHERE embedding_status='skipped' AND gemma_embedding_status='pending';
ALTER TABLE images ADD COLUMN IF NOT EXISTS face_error text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS folder_path text;
ALTER TABLE images ADD COLUMN IF NOT EXISTS capture_day date;
ALTER TABLE images ADD COLUMN IF NOT EXISTS date_source text;
ALTER TABLE images DROP COLUMN IF EXISTS search_vector;
CREATE INDEX IF NOT EXISTS images_path_trgm_idx ON images USING gin(path gin_trgm_ops);
CREATE INDEX IF NOT EXISTS images_sha_idx ON images(sha256);
CREATE INDEX IF NOT EXISTS images_dhash_idx ON images(dhash);
CREATE INDEX IF NOT EXISTS images_root_idx ON images(root_path);
CREATE INDEX IF NOT EXISTS images_folder_idx ON images(folder_path);
CREATE INDEX IF NOT EXISTS images_capture_at_idx ON images(capture_at,id);
CREATE INDEX IF NOT EXISTS images_mtime_idx ON images(mtime_ns,id);
CREATE INDEX IF NOT EXISTS images_size_idx ON images(size_bytes,id);
CREATE INDEX IF NOT EXISTS images_dimensions_idx ON images(width,height);
CREATE INDEX IF NOT EXISTS images_rating_idx ON images(rating,id);
CREATE INDEX IF NOT EXISTS images_favorite_idx ON images(id) WHERE favorite;
CREATE INDEX IF NOT EXISTS images_gps_idx ON images(gps_lat,gps_lon) WHERE gps_lat IS NOT NULL;
CREATE INDEX IF NOT EXISTS images_filename_sort_idx ON images(filename_sort,id);
CREATE INDEX IF NOT EXISTS images_embedding_idx ON images USING hnsw (embedding halfvec_cosine_ops) WHERE embedding IS NOT NULL;
CREATE INDEX IF NOT EXISTS images_gemma_embedding_idx ON images USING hnsw (gemma_embedding halfvec_cosine_ops) WHERE gemma_embedding IS NOT NULL;
CREATE TABLE IF NOT EXISTS image_search (
 image_id bigint PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
 search_text text NOT NULL
);
CREATE INDEX IF NOT EXISTS image_search_trgm_idx ON image_search USING gin(search_text gin_trgm_ops);
DO $$ BEGIN
 IF NOT EXISTS (SELECT 1 FROM image_search LIMIT 1) THEN
  INSERT INTO image_search(image_id,search_text)
  SELECT id,left(path || ' ' || exif::text || ' ' || generation::text,8192) FROM images;
 END IF;
END $$;
CREATE TABLE IF NOT EXISTS thumbnails (
 sha256 text NOT NULL,
 version integer NOT NULL,
 width smallint NOT NULL,
 height smallint NOT NULL,
 mime_type text NOT NULL,
 data bytea NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (sha256,version)
);
CREATE TABLE IF NOT EXISTS image_previews (
 sha256 text NOT NULL,
 version text NOT NULL,
 status text NOT NULL CHECK (status IN ('done','error','unsupported')),
 width integer, height integer,
 mime_type text NOT NULL DEFAULT 'image/jxl',
 data bytea, error text,
 updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (sha256,version)
);
CREATE TABLE IF NOT EXISTS people (
 id bigserial PRIMARY KEY,
 name text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE people DROP CONSTRAINT IF EXISTS people_name_key;
CREATE INDEX IF NOT EXISTS people_name_trgm_idx ON people USING gin(name gin_trgm_ops);
ALTER TABLE people ADD COLUMN IF NOT EXISTS ignored boolean NOT NULL DEFAULT false;
CREATE TABLE IF NOT EXISTS face_groups (
 id bigserial PRIMARY KEY,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS faces (
 id bigserial PRIMARY KEY,
 image_id bigint NOT NULL REFERENCES images(id) ON DELETE CASCADE,
 x integer NOT NULL, y integer NOT NULL, width integer NOT NULL, height integer NOT NULL,
 embedding halfvec(128) NOT NULL,
 group_id bigint NOT NULL REFERENCES face_groups(id),
 person_id bigint REFERENCES people(id) ON DELETE SET NULL,
 suggested_person_id bigint REFERENCES people(id) ON DELETE SET NULL,
 confidence real,
 UNIQUE(image_id,x,y,width,height)
);
CREATE INDEX IF NOT EXISTS faces_image_idx ON faces(image_id);
CREATE INDEX IF NOT EXISTS faces_person_idx ON faces(person_id);
CREATE INDEX IF NOT EXISTS faces_group_idx ON faces(group_id);
CREATE INDEX IF NOT EXISTS faces_embedding_idx ON faces USING hnsw (embedding halfvec_cosine_ops);
CREATE TABLE IF NOT EXISTS sort_proposals (
 id bigserial PRIMARY KEY,
 image_id bigint NOT NULL REFERENCES images(id) ON DELETE CASCADE,
 destination text NOT NULL,
 reason text NOT NULL,
 status text NOT NULL DEFAULT 'pending',
 created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(image_id,destination)
);
CREATE TABLE IF NOT EXISTS image_tags (
 image_id bigint NOT NULL REFERENCES images(id) ON DELETE CASCADE,
 tag text NOT NULL,
 PRIMARY KEY(image_id,tag)
);
CREATE INDEX IF NOT EXISTS image_tags_tag_idx ON image_tags(tag,image_id);
CREATE TABLE IF NOT EXISTS saved_views (
 name text PRIMARY KEY,
 state jsonb NOT NULL,
 updated_at timestamptz NOT NULL DEFAULT now()
);

UPDATE saved_views v SET state=jsonb_set(v.state,'{filters}',
 (SELECT coalesce(jsonb_agg(CASE WHEN chip->>'field'='person' AND
       (SELECT count(*) FROM people WHERE name=chip->>'value')=1
     THEN chip || jsonb_build_object('field','person_id','value',(SELECT id::text FROM people WHERE name=chip->>'value'))
     ELSE chip END),'[]'::jsonb) FROM jsonb_array_elements(v.state->'filters') chip))
 WHERE jsonb_typeof(v.state->'filters')='array' AND EXISTS
 (SELECT 1 FROM jsonb_array_elements(v.state->'filters') chip WHERE chip->>'field'='person');
CREATE TABLE IF NOT EXISTS label_prototypes (
 label text PRIMARY KEY,
 model_id text NOT NULL,
 embedding halfvec(512) NOT NULL
);
CREATE TABLE IF NOT EXISTS image_labels (
 image_id bigint NOT NULL REFERENCES images(id) ON DELETE CASCADE,
 label text NOT NULL,
 score real NOT NULL,
 model_id text NOT NULL,
 version smallint NOT NULL,
 PRIMARY KEY(image_id,label)
);
CREATE INDEX IF NOT EXISTS image_labels_label_idx ON image_labels(label,score DESC,image_id);
CREATE TABLE IF NOT EXISTS collections (
 id bigserial PRIMARY KEY,
 kind text NOT NULL CHECK(kind IN ('place','event','trip')),
 suggested_name text NOT NULL,
 user_name text,
 status text NOT NULL DEFAULT 'suggested' CHECK(status IN ('suggested','confirmed','dismissed')),
 locked boolean NOT NULL DEFAULT false,
 scope_key text NOT NULL DEFAULT '',
 start_at timestamp, end_at timestamp,
 latitude double precision, longitude double precision,
 confidence real NOT NULL DEFAULT 0,
 evidence jsonb NOT NULL DEFAULT '{}',
 version integer NOT NULL DEFAULT 1,
 stale boolean NOT NULL DEFAULT false,
 updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS collections_kind_idx ON collections(kind,status,id);
CREATE TABLE IF NOT EXISTS collection_images (
 collection_id bigint NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
 image_id bigint NOT NULL REFERENCES images(id) ON DELETE CASCADE,
 source text NOT NULL CHECK(source IN ('automatic','manual','excluded')),
 confidence real NOT NULL DEFAULT 1,
 evidence jsonb NOT NULL DEFAULT '{}',
 PRIMARY KEY(collection_id,image_id)
);
CREATE INDEX IF NOT EXISTS collection_images_image_idx ON collection_images(image_id,collection_id);
CREATE TABLE IF NOT EXISTS collection_links (
 parent_id bigint NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
 child_id bigint NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
 position integer NOT NULL DEFAULT 0,
 PRIMARY KEY(parent_id,child_id), CHECK(parent_id<>child_id)
);
CREATE TABLE IF NOT EXISTS collection_settings (
 key text PRIMARY KEY, value jsonb NOT NULL
);
CREATE TABLE IF NOT EXISTS place_lookup_cache (
 provider text NOT NULL, latitude numeric(8,5) NOT NULL, longitude numeric(8,5) NOT NULL,
 response jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(provider,latitude,longitude)
);
CREATE OR REPLACE FUNCTION mark_collections_stale() RETURNS trigger AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  UPDATE collections SET stale=true WHERE id IN
    (SELECT collection_id FROM collection_images WHERE image_id=OLD.id);
  RETURN OLD;
 END IF;
 UPDATE collections SET stale=true WHERE id IN
   (SELECT collection_id FROM collection_images WHERE image_id=NEW.id);
 RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS images_collections_stale ON images;
CREATE TRIGGER images_collections_stale AFTER UPDATE OF sha256,capture_at,gps_lat,gps_lon,embedding,face_status,labels_status,missing
 ON images FOR EACH ROW EXECUTE FUNCTION mark_collections_stale();
DROP TRIGGER IF EXISTS images_collections_delete ON images;
CREATE TRIGGER images_collections_delete BEFORE DELETE ON images FOR EACH ROW EXECUTE FUNCTION mark_collections_stale();
CREATE OR REPLACE FUNCTION mark_face_collections_stale() RETURNS trigger AS $$
DECLARE affected_id bigint;
BEGIN
 IF TG_OP='DELETE' THEN affected_id=OLD.image_id; ELSE affected_id=NEW.image_id; END IF;
 UPDATE collections SET stale=true WHERE id IN
   (SELECT collection_id FROM collection_images WHERE image_id=affected_id);
 IF TG_OP='DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS faces_collections_stale ON faces;
CREATE TRIGGER faces_collections_stale AFTER INSERT OR UPDATE OR DELETE ON faces
 FOR EACH ROW EXECUTE FUNCTION mark_face_collections_stale();
CREATE OR REPLACE FUNCTION mark_person_collections_stale() RETURNS trigger AS $$
BEGIN
 UPDATE collections SET stale=true WHERE id IN
   (SELECT m.collection_id FROM collection_images m JOIN faces f ON f.image_id=m.image_id WHERE f.person_id=NEW.id);
 RETURN NEW;
END; $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS people_collections_stale ON people;
CREATE TRIGGER people_collections_stale AFTER UPDATE OF ignored ON people
 FOR EACH ROW EXECUTE FUNCTION mark_person_collections_stale();
CREATE TABLE IF NOT EXISTS catalog_migrations (version integer PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now());
INSERT INTO catalog_migrations(version) VALUES (2) ON CONFLICT DO NOTHING;
INSERT INTO catalog_migrations(version) VALUES (3) ON CONFLICT DO NOTHING;
INSERT INTO catalog_migrations(version) VALUES (4) ON CONFLICT DO NOTHING;

"""


def connect():
    from .catalogs import resolve, SCHEMA_VERSION
    catalog = resolve()
    conn = psycopg.connect(catalog.dsn, row_factory=dict_row)
    if catalog.id:
        try:
            identity = conn.execute('SELECT id,schema_version FROM catalog_identity').fetchone()
            if not identity or str(identity['id']) != catalog.id or identity['schema_version'] != SCHEMA_VERSION:
                raise ValueError('Catalog identity or schema version mismatch')
            conn.commit()
            if catalog.readonly:
                conn.execute('SET default_transaction_read_only=on')
                conn.commit()
        except BaseException:
            conn.close()
            raise
    return conn


def initialize():
    with connect() as conn:
        from .catalogs import resolve
        catalog = resolve()
        if catalog.readonly:
            return
        if not catalog.id:
            conn.execute(SCHEMA)
        else:
            table = conn.execute("SELECT to_regclass('public.catalog_migrations') AS name").fetchone()['name']
            if table is None or conn.execute('SELECT count(*) n FROM catalog_migrations WHERE version IN (2,3,4)').fetchone()['n'] != 3:
                conn.execute(SCHEMA)


def normalized_path(path: str | Path) -> str:
    return str(Path(path).expanduser().resolve())


def vector_literal(values) -> str:
    return "[" + ",".join(format(float(v), ".8g") for v in values) + "]"


def pending_images(conn, column: str, *, limit: int | None = None, retry_errors: bool = False,
                   root_path: str | None = None, image_ids: list[int] | None = None,
                   selected_table: bool = False):
    """Walk a large backlog with keyset pagination while callers commit each result."""
    if column not in {"embedding_status", "gemma_embedding_status", "face_status"}:
        raise ValueError("Unknown status column")
    last_id = 0
    count = 0
    statuses = ["pending", "error"] if retry_errors else ["pending"]
    while limit is None or count < limit:
        batch_size = min(200, (limit - count) if limit is not None else 200)
        scope = "AND EXISTS (SELECT 1 FROM mine_selected_ids s WHERE s.id=images.id) " if selected_table else ""
        rows = conn.execute(
            f"SELECT id,path FROM images WHERE missing=false AND {column}=ANY(%s) AND id>%s "
            "AND (%s::text IS NULL OR root_path=%s) "
            f"AND (%s::bigint[] IS NULL OR id=ANY(%s::bigint[])) {scope} ORDER BY id LIMIT %s",
            (statuses, last_id, root_path, root_path, image_ids, image_ids, batch_size),
        ).fetchall()
        if not rows:
            break
        for row in rows:
            last_id = row["id"]
            from .catalogs import resolve, under
            catalog = resolve()
            if catalog.id and not any(under(row['path'], p) for p in catalog.roots):
                continue
            count += 1
            yield row
