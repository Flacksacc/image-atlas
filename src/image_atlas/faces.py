"""Persistent face grouping; native inference runs in an isolated worker."""
from pathlib import Path
from .cloud import unavailable_cloud_file
from .db import pending_images, vector_literal
from .face_client import FaceWorkerClient, FaceSetupError

FaceEngine = FaceWorkerClient
SAME_PERSON_COSINE = 0.55


def _group_for(conn, embedding):
    literal = vector_literal(embedding)
    nearest = conn.execute("""SELECT group_id,person_id,suggested_person_id,embedding <=> %s::halfvec AS distance
                              FROM faces ORDER BY embedding <=> %s::halfvec LIMIT 1""",
                           (literal, literal)).fetchone()
    if nearest and nearest["distance"] <= 1 - SAME_PERSON_COSINE and nearest["person_id"] is None:
        return nearest["group_id"], nearest["suggested_person_id"]
    new_group = conn.execute("INSERT INTO face_groups DEFAULT VALUES RETURNING id").fetchone()["id"]
    suggestion = nearest["person_id"] if nearest and nearest["distance"] <= 1 - SAME_PERSON_COSINE else None
    return new_group, suggestion


def process_pending(conn, *, limit: int | None = None, retry_errors: bool = False,
                    root_path: str | None = None, image_ids: list[int] | None = None, progress=None,
                    selected_table: bool = False, check=None, counts=None, job=None, runtime=None):
    engine = None
    check = check or (lambda: None)
    result = counts if counts is not None else {}
    result.update({"images": 0, "faces": 0, "skipped_cloud": 0, "worker_restarts": 0, "errors": []})
    try:
        for row in pending_images(conn, "face_status", limit=limit, retry_errors=retry_errors,
                                  root_path=root_path, image_ids=image_ids, selected_table=selected_table):
            check()
            if unavailable_cloud_file(Path(row["path"])):
                result["skipped_cloud"] += 1
                if progress is not None:
                    progress()
                continue
            if job is not None:
                job.update(stage='faces', image_id=row['id'], path=row['path'], images_completed=result['images'])
            if engine is None:
                engine = FaceEngine(check=check) if FaceEngine is FaceWorkerClient else FaceEngine()
                if runtime is not None:
                    runtime(model='YuNet + SFace', model_id='opencv:yunet-2023mar:sface-2021dec',
                            device=str(getattr(engine, 'device', 'unknown')))
            try:
                detections = list(engine.detect(Path(row["path"])))
                check()
                for face in detections:
                    group_id, suggested_id = _group_for(conn, face["embedding"])
                    x, y, width, height = face["box"]
                    conn.execute("""INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,suggested_person_id,confidence,person_id)
                                    VALUES (%s,%s,%s,%s,%s,%s::halfvec,%s,%s,%s,
                                            (SELECT id FROM people WHERE id=%s AND ignored))
                                    ON CONFLICT(image_id,x,y,width,height) DO NOTHING""",
                                 (row["id"], x, y, width, height, vector_literal(face["embedding"]),
                                  group_id, suggested_id, face["confidence"], suggested_id))
                conn.execute("UPDATE images SET face_status='done',face_error=NULL WHERE id=%s", (row["id"],))
                conn.commit()
                result["images"] += 1
                result["faces"] += len(detections)
            except FaceSetupError:
                raise
            except Exception as exc:
                conn.rollback()
                conn.execute("UPDATE images SET face_status='error',face_error=%s WHERE id=%s", (str(exc)[:1000], row["id"]))
                conn.commit()
                result["errors"].append({"path": row["path"], "error": str(exc)})
            finally:
                result['worker_restarts'] = getattr(engine, 'restarts', 0)
                if progress is not None:
                    progress()
    finally:
        if engine is not None and hasattr(engine, 'close'):
            engine.close()
    return result


def name_group(conn, group_id: int, name: str | None = None, *, person_id: int | None = None,
               ignore: bool = False):
    if person_id is not None and name is not None:
        raise ValueError("Choose an existing person or enter a new name")
    if person_id is None and not ignore and (name is None or not name.strip()):
        raise ValueError("Person name cannot be empty")
    group = conn.execute("SELECT id FROM face_groups WHERE id=%s", (group_id,)).fetchone()
    if not group:
        raise ValueError(f"No face group {group_id}")
    if person_id is not None:
        person = conn.execute("SELECT id,name,ignored FROM people WHERE id=%s", (person_id,)).fetchone()
        if not person:
            raise ValueError(f"No person {person_id}")
        if person["ignored"] and not ignore:
            raise ValueError("Person is ignored")
    elif ignore:
        person = conn.execute("""SELECT p.id,p.name,p.ignored FROM faces f JOIN people p ON p.id=f.person_id
                                 WHERE f.group_id=%s LIMIT 1""", (group_id,)).fetchone()
        if not person:
            # Each ignored identity has its own ID and recognition samples.
            from uuid import uuid4
            person = conn.execute("INSERT INTO people(name,ignored) VALUES (%s,true) RETURNING id,name,ignored",
                                  (f"Ignored person {uuid4()}",)).fetchone()
    else:
        if len(name.strip()) > 200:
            raise ValueError("Person name must contain 1–200 characters")
        person = conn.execute("INSERT INTO people(name) VALUES (%s) RETURNING id,name,ignored", (name.strip(),)).fetchone()
        if person["ignored"]:
            raise ValueError("Person is ignored")
    if ignore:
        conn.execute("UPDATE people SET ignored=true WHERE id=%s", (person["id"],))
        conn.execute("""UPDATE faces SET person_id=%s,suggested_person_id=NULL
                        WHERE person_id IS NULL AND suggested_person_id=%s""", (person["id"], person["id"]))
    count = conn.execute("""UPDATE faces SET person_id=%s,suggested_person_id=NULL
                            WHERE group_id=%s RETURNING id""", (person["id"], group_id)).fetchall()
    conn.commit()
    return {"group_id": group_id, "person_id": person["id"], "name": person["name"],
            "ignored": ignore or person["ignored"], "faces_named": len(count)}
