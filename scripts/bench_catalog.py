"""Seed and measure a dedicated one-million-image PostgreSQL catalog.

The fixed imagecli_bench database prevents accidental writes to the user's catalog.
Run: python scripts/bench_catalog.py --seed --rows 1000000
"""
import argparse
import json
import os
import statistics
import time

import psycopg

from image_atlas.db import DEFAULT_DSN, initialize


def percentile(values, fraction):
    ordered = sorted(values)
    return round(ordered[min(len(ordered) - 1, int(len(ordered) * fraction))] * 1000, 2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seed", action="store_true", help="Replace synthetic benchmark rows")
    parser.add_argument("--rows", type=int, default=1_000_000)
    parser.add_argument("--repeats", type=int, default=15)
    args = parser.parse_args()
    if args.rows < 1 or args.repeats < 2:
        parser.error("--rows must be positive and --repeats at least 2")
    base = DEFAULT_DSN.rsplit("/", 1)[0]
    dsn = base + "/imagecli_bench"
    with psycopg.connect(base + "/postgres", autocommit=True) as admin:
        if not admin.execute("SELECT 1 FROM pg_database WHERE datname='imagecli_bench'").fetchone():
            admin.execute("CREATE DATABASE imagecli_bench")
    os.environ["IMAGE_CLI_DSN"] = dsn
    initialize()
    with psycopg.connect(dsn) as conn:
        count = conn.execute("SELECT count(*) FROM images").fetchone()[0]
        if args.seed:
            conn.execute("TRUNCATE image_labels,image_tags,sort_proposals,faces,images,thumbnails CASCADE")
            conn.commit()
            for start in range(1, args.rows + 1, 50_000):
                end = min(start + 49_999, args.rows)
                conn.execute("""INSERT INTO images(path,root_path,folder_path,capture_day,date_source,
                    size_bytes,mtime_ns,width,height,format,sha256,review_state)
                    SELECT 'B:\\bench\\f'||(g%%1000)||'\\image-'||g||'.jpg','B:\\bench',
                           'B:\\bench\\f'||(g%%1000),date '2020-01-01'+(g%%2200)::int,
                           'exif',100000+(g%%10000000),1700000000000000000+g,
                           4000+(g%%500),3000+(g%%500),'JPEG',md5(g::text),'unreviewed'
                    FROM generate_series(%s::bigint,%s::bigint) AS g""", (start, end))
                conn.commit()
            conn.execute("INSERT INTO image_search(image_id,search_text) SELECT id,path FROM images")
            conn.execute("ANALYZE images")
            conn.execute("ANALYZE image_search")
            conn.commit()
            count = args.rows
        if count == 0:
            parser.error("Benchmark catalog is empty; rerun with --seed")
        queries = {
            "first_page": "SELECT id,path FROM images WHERE NOT missing ORDER BY id DESC LIMIT 200",
            "folder_day_page": "SELECT id,path FROM images WHERE folder_path='B:\\bench\\f42' AND capture_day>='2024-01-01' ORDER BY capture_day DESC,id DESC LIMIT 200",
            "folder_counts": "SELECT folder_path,count(*),sum(size_bytes) FROM images WHERE NOT missing GROUP BY folder_path",
            "day_counts": "SELECT capture_day,count(*) FROM images WHERE NOT missing GROUP BY capture_day",
            "deep_cursor": "SELECT id,path FROM images WHERE NOT missing AND id < (SELECT max(id)-500000 FROM images) ORDER BY id DESC LIMIT 200",
        }
        results = {}
        for name, sql in queries.items():
            timings = []
            for _ in range(args.repeats + 2):
                started = time.perf_counter()
                conn.execute(sql).fetchall()
                timings.append(time.perf_counter() - started)
            timings = timings[2:]
            results[name] = {"p50_ms": percentile(timings, 0.5),
                             "p95_ms": percentile(timings, 0.95),
                             "mean_ms": round(statistics.mean(timings) * 1000, 2)}
        print(json.dumps({"database": "imagecli_bench", "images": count,
                          "repeats": args.repeats, "queries": results}, indent=2))


if __name__ == "__main__":
    main()
