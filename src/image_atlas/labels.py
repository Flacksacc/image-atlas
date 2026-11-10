"""Versioned, compact OpenCLIP content labels derived from existing image vectors."""
import sys

import numpy as np

from .db import vector_literal
from .embeddings import MODEL_ID, create_embedder

LABEL_VERSION = 2
LABELS = (
    "animal", "dog", "cat", "bird", "person", "portrait", "group photo",
    "landscape", "mountain", "beach", "ocean", "forest", "flower", "garden",
    "city", "building", "vehicle", "car", "airplane", "food", "drink",
    "sports", "wedding", "birthday", "party", "graduation", "concert", "artwork", "illustration", "screenshot",
    "document", "product photo", "night scene", "sunset", "snow",
)


def _prototype_matrix(conn, *, cpu=False, runtime=None):
    stored = {row["label"]: row for row in conn.execute(
        "SELECT label,embedding::text AS embedding FROM label_prototypes WHERE model_id=%s", (MODEL_ID,))}
    missing = [label for label in LABELS if label not in stored]
    encoder = None
    try:
        if missing:
            encoder = create_embedder(cpu=cpu)
            if runtime is not None:
                runtime(model='OpenCLIP ViT-B/32', model_id=MODEL_ID,
                        device=str(getattr(encoder, 'device', 'unknown')), runtime_state='encoding label prompts')
            for label in missing:
                vector = encoder.text(f"a photo of {label}")
                conn.execute("""INSERT INTO label_prototypes(label,model_id,embedding)
                                VALUES (%s,%s,%s::halfvec)
                                ON CONFLICT(label) DO UPDATE SET model_id=excluded.model_id,
                                    embedding=excluded.embedding""",
                             (label, MODEL_ID, vector_literal(vector)))
                stored[label] = {"embedding": vector_literal(vector)}
            conn.commit()
    finally:
        if encoder is not None:
            if sys.exc_info()[0] is not None and hasattr(encoder, "abort"):
                encoder.abort()
            elif hasattr(encoder, "close"):
                encoder.close()
    matrix = np.asarray([np.fromstring(stored[label]["embedding"].strip("[]"),
                                       sep=",", dtype=np.float32) for label in LABELS])
    if matrix.shape != (len(LABELS), 512):
        raise ValueError("Invalid content-label prototype vectors")
    return matrix


def process_pending(conn, *, cpu=False, retry_errors=False, root_path=None,
                    image_ids=None, progress=None, selected_table=False, runtime=None):
    statuses = ["pending", "error"] if retry_errors else ["pending"]
    scope = "AND EXISTS (SELECT 1 FROM mine_selected_ids s WHERE s.id=i.id)" if selected_table else ""
    conditions = f"""(i.labels_status=ANY(%s) OR
        (i.labels_status='done' AND i.labels_version<{LABEL_VERSION})) AND i.embedding IS NOT NULL
        AND i.embedding_model=%s AND i.missing=false
        AND (%s::text IS NULL OR i.root_path=%s)
        AND (%s::bigint[] IS NULL OR i.id=ANY(%s::bigint[])) {scope}"""
    pending = conn.execute(f"SELECT count(*) AS n FROM images i WHERE {conditions}",
                           (statuses, MODEL_ID, root_path, root_path, image_ids, image_ids)).fetchone()["n"]
    result = {"processed": 0, "labels": 0, "errors": []}
    if not pending:
        return result
    matrix = _prototype_matrix(conn, cpu=cpu, runtime=runtime)
    if runtime is not None:
        runtime(model='OpenCLIP ViT-B/32', model_id=MODEL_ID, device='cpu', runtime_state='scoring cached vectors')
    last_id = 0
    while True:
        rows = conn.execute(f"""SELECT i.id,i.path,i.embedding::text AS embedding FROM images i
                                WHERE {conditions} AND i.id>%s ORDER BY i.id LIMIT 256""",
                            (statuses, MODEL_ID, root_path, root_path, image_ids, image_ids, last_id)).fetchall()
        if not rows:
            break
        for row in rows:
            last_id = row["id"]
            try:
                vector = np.fromstring(row["embedding"].strip("[]"), sep=",", dtype=np.float32)
                if vector.shape != (512,):
                    raise ValueError("Invalid image embedding")
                scores = matrix @ vector
                picks = np.argsort(scores)[-5:][::-1]
                conn.execute("DELETE FROM image_labels WHERE image_id=%s", (row["id"],))
                for index in picks:
                    score = float(scores[index])
                    if score < 0.20:
                        continue
                    conn.execute("""INSERT INTO image_labels(image_id,label,score,model_id,version)
                                    VALUES (%s,%s,%s,%s,%s)""",
                                 (row["id"], LABELS[int(index)], score, MODEL_ID, LABEL_VERSION))
                    result["labels"] += 1
                conn.execute("UPDATE images SET labels_status='done',labels_version=%s WHERE id=%s",
                             (LABEL_VERSION, row["id"]))
                result["processed"] += 1
            except Exception as exc:  # noqa: BLE001 - isolate each image
                conn.execute("UPDATE images SET labels_status='error' WHERE id=%s", (row["id"],))
                result["errors"].append({"path": row["path"], "error": str(exc)})
            conn.commit()
            if progress is not None:
                progress()
    return result
