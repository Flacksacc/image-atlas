"""Local collection detection and durable review; originals are never opened here."""
from collections import defaultdict
from bisect import bisect_left, bisect_right
from datetime import timedelta
from itertools import product
import json
import math
import time
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen

import numpy as np
from psycopg.types.json import Jsonb

from .db import connect, normalized_path

VERSION = 1
DEFAULTS = {"place_radius_m": 250, "event_gap_hours": 2, "event_distance_km": 5,
            "trip_gap_hours": 72, "trip_distance_km": 10, "trip_max_days": 21,
            "geocoder_url": "https://nominatim.openstreetmap.org/reverse"}
KINDS = {"place", "event", "trip"}
OCCASIONS = {"wedding", "birthday", "concert", "sports", "party", "graduation"}


def settings(conn, changes=None):
    if changes:
        if changes.keys() - DEFAULTS.keys():
            raise ValueError("Unknown detection setting")
        for key, value in changes.items():
            if key == "geocoder_url":
                if not isinstance(value, str) or urlparse(value).scheme != "https":
                    raise ValueError("Geocoder URL must use HTTPS")
            elif isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
                raise ValueError("Detection thresholds must be positive finite numbers")
        for key, value in changes.items():
            conn.execute("INSERT INTO collection_settings VALUES (%s,%s) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                         (key, Jsonb(value)))
    return DEFAULTS | {r["key"]: r["value"] for r in conn.execute("SELECT * FROM collection_settings WHERE key=ANY(%s)",(list(DEFAULTS),))}


def distance(a, b):
    lat1, lon1, lat2, lon2 = map(math.radians, (*a, *b))
    h = math.sin((lat2-lat1)/2)**2 + math.cos(lat1)*math.cos(lat2)*math.sin((lon2-lon1)/2)**2
    return 6371000 * 2 * math.asin(min(1, math.sqrt(h)))


def coordinate(row):
    a, b = row["gps_lat"], row["gps_lon"]
    if a is not None and b is not None and math.isfinite(a) and math.isfinite(b) and -90 <= a <= 90 and -180 <= b <= 180:
        return a, b
    return None


def geo_cell(point, radius):
    lat, lon = map(math.radians, point)
    return tuple(math.floor(v / radius) for v in
                 (6371000*math.cos(lat)*math.cos(lon), 6371000*math.cos(lat)*math.sin(lon), 6371000*math.sin(lat)))


def clock(row):
    stamp = row["capture_at"]
    offset = row.get("capture_offset_minutes")
    # Old indexed records can supply offsets without reopening the original.
    if offset is None:
        raw = row.get("exif", {}).get("OffsetTimeOriginal")
        if isinstance(raw, str) and len(raw) == 6 and raw[0] in "+-" and raw[3] == ":":
            try:
                hours, minutes = int(raw[1:3]), int(raw[4:6])
                if hours <= 23 and minutes < 60:
                    offset = (hours * 60 + minutes) * (-1 if raw[0] == "-" else 1)
            except ValueError:
                pass
    return (stamp - timedelta(minutes=offset) if stamp and offset is not None else stamp,
            "utc" if offset is not None else "unknown")


def candidate(kind, rows, name, evidence, point=None):
    times = [clock(r)[0] for r in rows if r["capture_at"]]
    return {"kind": kind, "rows": rows, "name": name, "evidence": evidence,
            "point": point, "start": min(times) if times else None,
            "end": max(times) if times else None, "confidence": .85 if kind == "place" else .7,
            "children": []}


def build_candidates(rows, config, check=lambda: None, progress=lambda n: None):
    # Exact copies contribute one observation; expand them again before publication.
    copies = defaultdict(list)
    for row in rows:
        copies[row["sha256"] or f"id:{row['id']}"].append(row)
    unique = [min(items, key=lambda r: (r["capture_at"] is None, coordinate(r) is None, r["id"]))
              for items in copies.values()]
    places, cells, image_place = [], defaultdict(list), {}
    radius = config["place_radius_m"]
    for number, row in enumerate(unique):
        check()
        point = coordinate(row)
        if point:
            here = geo_cell(point, radius)
            matches = [p for delta in product((-1, 0, 1), repeat=3)
                       for p in cells[tuple(a+b for a, b in zip(here, delta))]
                       if distance(p["point"], point) <= radius]
            if matches:
                place = min(matches, key=lambda p: distance(p["point"], point))
                place["rows"].append(row)
            else:
                # Fixed anchor bounds every cluster's diameter to twice the radius.
                place = candidate("place", [row], f"Place {point[0]:.4f}, {point[1]:.4f}",
                                  {"reason": "GPS proximity", "radius_m": radius}, point)
                places.append(place)
                cells[here].append(place)
            image_place[row["id"]] = place
        progress(number+1)
    events = []
    streams = defaultdict(list)
    for row in unique:
        stamp, basis = clock(row)
        if stamp:
            streams[basis].append(row)
    for basis, stream in streams.items():
        stream.sort(key=lambda r: (clock(r)[0], r["id"]))
        batch, last_gps = [], None
        def finish():
            if len(batch) < 2:
                return
            labels = defaultdict(int)
            for item in batch:
                for label in item.get("labels", []):
                    if label in OCCASIONS:
                        labels[label] += 1
            occasion = max(labels, key=labels.get) if labels else None
            hint = occasion.title() if occasion and labels[occasion] >= max(2, math.ceil(len(batch)*.5)) else "Photo session"
            event = candidate("event", list(batch), f"{hint} · {clock(batch[0])[0]:%Y-%m-%d}",
                              {"reason": "Capture-time session", "clock_basis": basis,
                               "occasion_hint": occasion if hint != "Photo session" else None,
                               "duration_precise": basis == "utc"}, last_gps)
            event['children'] = list({id(image_place[r['id']]):image_place[r['id']] for r in batch if r['id'] in image_place}.values())
            events.append(event)
        for row in stream:
            check()
            point = coordinate(row)
            if batch and ((clock(row)[0]-clock(batch[-1])[0]).total_seconds() > config["event_gap_hours"]*3600
                          or (point and last_gps and distance(point, last_gps) > config["event_distance_km"]*1000)):
                finish()
                batch, last_gps = [], None
            batch.append(row)
            last_gps = point or last_gps
        finish()
    # Infer missing-date membership only through strong local evidence, with bounded candidates.
    people_events = defaultdict(list)
    for event in events:
        people = {person for row in event["rows"] for person in row.get("people", [])}
        event["people"] = people
        vectors = [np.fromstring(r["embedding"].strip("[]"), sep=",") for r in event["rows"]
                   if r.get("embedding") and r.get("embedding_model")]
        event["vectors"] = [v for v in vectors[:8] if v.shape == (512,)]
        for person in people:
            people_events[person].append(event)
    for row in unique:
        if row["capture_at"] or not row.get("embedding") or len(row.get("people", [])) < 2:
            continue
        check()
        options = {id(e): e for p in row["people"] for e in people_events[p][-32:]}
        vector = np.fromstring(row["embedding"].strip("[]"), sep=",")
        if vector.shape != (512,):
            continue
        matches = []
        for event in options.values():
            if len(set(row["people"]) & event["people"]) < 2:
                continue
            point = coordinate(row)
            if point and event["point"] and distance(point, event["point"]) > config["event_distance_km"]*1000:
                continue
            scores = [float(vector @ v / max(1e-12, np.linalg.norm(vector)*np.linalg.norm(v)))
                      for v in event["vectors"]
                      if row["embedding_model"] == event["rows"][0].get("embedding_model")]
            if scores and max(scores) >= .9:
                matches.append((max(scores), event))
        matches.sort(key=lambda pair: pair[0], reverse=True)
        if matches and (len(matches) == 1 or matches[0][0]-matches[1][0] >= .05):
            row["inferred"] = True
            matches[0][1]["rows"].append(row)
    # Visits include singleton GPS observations, so a day outing needn't contain two photos per stop.
    trips = []
    event_streams = {basis: sorted((e for e in events if e["evidence"]["clock_basis"] == basis), key=lambda e:e["start"])
                     for basis in streams}
    event_times = {basis: [e["start"] for e in items] for basis, items in event_streams.items()}
    for basis, stream in streams.items():
        visits = [r for r in stream if r["id"] in image_place]
        batches, batch = [], []
        for row in visits:
            if batch and ((clock(row)[0]-clock(batch[-1])[0]).total_seconds() > config["trip_gap_hours"]*3600
                          or clock(row)[0]-clock(batch[0])[0] > timedelta(days=config["trip_max_days"])):
                batches.append(batch)
                batch = []
            batch.append(row)
        if batch:
            batches.append(batch)
        for batch in batches:
            check()
            stops = list({id(image_place[r["id"]]): image_place[r["id"]] for r in batch}.values())
            if len(stops) < 2:
                continue
            anchor = stops[0]["point"]
            # Linear check from first stop avoids a quadratic distance matrix.
            if max(distance(anchor, p["point"]) for p in stops) < config["trip_distance_km"]*1000:
                continue
            start, end = clock(batch[0])[0], clock(batch[-1])[0]
            children = event_streams[basis][bisect_left(event_times[basis], start):bisect_right(event_times[basis], end)]
            included = {r["id"]: r for r in batch}
            for event in children:
                included.update({r["id"]: r for r in event["rows"]})
            trip = candidate("trip", list(included.values()), f"Day outing · {start:%Y-%m-%d}" if start.date() == end.date()
                             else f"Journey · {start:%Y-%m-%d} – {end:%Y-%m-%d}",
                             {"reason": "Chronological visits to distinct places", "clock_basis": basis,
                              "duration_precise": basis == "utc", "place_count": len(stops)}, anchor)
            trip["children"] = children + stops
            trip['route'] = []
            for row in batch:
                place = image_place[row['id']]
                if not trip['route'] or trip['route'][-1][0] is not place:
                    trip['route'].append((place,clock(row)[0]))
            trips.append(trip)
    candidates = places + events + trips
    for item in candidates:
        expanded = {r["id"]: r for row in item["rows"] for r in copies[row["sha256"] or f"id:{row['id']}"]}
        item["rows"] = list(expanded.values())
        times = [clock(r)[0] for r in item['rows'] if r['capture_at']]
        item['start'], item['end'] = (min(times), max(times)) if times else (None, None)
    return candidates


def detect(conn, root=None, check=lambda: None, progress=lambda n: None):
    with conn.transaction():
        return _detect(conn, root, check, progress)


def _detect(conn, root=None, check=lambda: None, progress=lambda n: None):
    scope = normalized_path(root) if root else ""
    config = settings(conn)
    while not conn.execute("SELECT pg_try_advisory_xact_lock(73412861) AS acquired").fetchone()['acquired']:
        check()
        time.sleep(.1)
    rows = conn.execute("""SELECT i.id,i.sha256,i.capture_at,i.capture_offset_minutes,i.gps_lat,i.gps_lon,
        i.exif,i.embedding::text AS embedding,i.embedding_model,
        coalesce((SELECT array_agg(DISTINCT f.person_id) FROM faces f JOIN people p ON p.id=f.person_id
                  WHERE f.image_id=i.id AND NOT p.ignored),'{}') AS people,
        coalesce((SELECT array_agg(l.label) FROM image_labels l WHERE l.image_id=i.id AND l.score>=0.25),'{}') AS labels
        FROM images i WHERE NOT i.missing AND (%s='' OR i.root_path=%s) ORDER BY i.id""", (scope, scope)).fetchall()
    candidates = build_candidates(rows, config, check, progress)
    check()
    # Serialize detector publication and review mutations across all frontends.
    old = conn.execute("SELECT * FROM collections WHERE scope_key=%s ORDER BY id FOR UPDATE", (scope,)).fetchall()
    memberships = defaultdict(set)
    inverted = defaultdict(set)
    for r in conn.execute("SELECT collection_id,image_id FROM collection_images WHERE source<>'excluded'"):
        memberships[r["collection_id"]].add(r["image_id"])
        inverted[r["image_id"]].add(r["collection_id"])
    old_by_id = {r["id"]: r for r in old}
    old_places = defaultdict(set)
    for previous in old:
        if previous['kind'] == 'place' and previous['latitude'] is not None:
            old_places[geo_cell((previous['latitude'], previous['longitude']), config['place_radius_m'])].add(previous['id'])
    used, counts = set(), {k: 0 for k in KINDS}
    for item in candidates:
        check()
        if item['kind'] == 'event':
            named = [p['name'] for p in item['children'] if p.get('id') and not p['name'].startswith('Place ')]
            if named:
                hint = (item['evidence'].get('occasion_hint') or 'Photo session').title()
                item['name'] = f"{hint} · {named[0][:120]} · {item['start']:%Y-%m-%d}"
        ids = {r["id"] for r in item["rows"]}
        possible = {c for image in ids for c in inverted[image]} & old_by_id.keys()
        if item['kind'] == 'place':
            here = geo_cell(item['point'], config['place_radius_m'])
            for delta in product((-1,0,1), repeat=3):
                possible |= old_places[tuple(a+b for a,b in zip(here,delta))]
        matches = []
        for cid in possible-used:
            previous = old_by_id[cid]
            if previous["kind"] != item["kind"]:
                continue
            overlap = len(ids & memberships[cid]) / max(1, len(ids | memberships[cid]))
            nearby = (item['kind'] == 'place' and previous['latitude'] is not None and
                      distance(item['point'], (previous['latitude'],previous['longitude'])) <= config['place_radius_m'])
            named_growth = bool(previous['user_name']) and memberships[cid] <= ids
            if overlap >= .5 or nearby or named_growth:
                matches.append((overlap, -cid, previous))
        previous = max(matches, key=lambda m: (m[0], m[1]))[2] if matches else None
        if previous:
            cid = previous["id"]
            used.add(cid)
            if previous["locked"] or previous["status"] == "dismissed":
                item["id"] = cid
                item['name'] = previous['user_name'] or previous['suggested_name']
                if previous['locked']:
                    _refresh_manual(conn,cid)
                conn.execute('UPDATE collections SET stale=false WHERE id=%s',(cid,))
                continue
            lookup_name = previous['evidence'].get('lookup_name')
            if lookup_name:
                item['name'] = lookup_name
                item['evidence']['lookup_name'] = lookup_name
            point = item["point"] or (None, None)
            conn.execute("""UPDATE collections SET suggested_name=%s,start_at=%s,end_at=%s,latitude=%s,
                longitude=%s,confidence=%s,evidence=%s,version=%s,stale=false,updated_at=now() WHERE id=%s""",
                         (item["name"], item["start"], item["end"], *point, item["confidence"], Jsonb(item["evidence"]), VERSION, cid))
            conn.execute("DELETE FROM collection_images WHERE collection_id=%s AND source='automatic'", (cid,))
        else:
            # Never resurrect subsets of manually split or dismissed collections.
            blockers = [old_by_id[c] for c in possible if old_by_id[c]["kind"] == item["kind"] and (old_by_id[c]["locked"] or old_by_id[c]["status"] == "dismissed")
                        and ids & memberships[c]]
            if blockers:
                continue
            cid = conn.execute("""INSERT INTO collections(kind,suggested_name,scope_key,start_at,end_at,latitude,longitude,confidence,evidence)
                VALUES (%s,%s,%s,%s,%s,%s,%s,%s,%s) RETURNING id""",
                (item["kind"], item["name"], scope, item["start"], item["end"], *(item["point"] or (None, None)),
                 item["confidence"], Jsonb(item["evidence"]))).fetchone()["id"]
        item["id"] = cid
        if previous and previous['user_name']:
            item['name'] = previous['user_name']
        entries = []
        for row in item["rows"]:
            inferred = not row["capture_at"] and item["kind"] != "place"
            entries.append((cid, row["id"], .55 if inferred else item["confidence"],
                          Jsonb({"reason": "Two known people and strong visual similarity" if inferred else item["evidence"]["reason"]})))
        with conn.cursor() as cursor:
            cursor.executemany("""INSERT INTO collection_images(collection_id,image_id,source,confidence,evidence)
                VALUES (%s,%s,'automatic',%s,%s) ON CONFLICT DO NOTHING""",
                         entries)
        counts[item["kind"]] += 1
    for item in candidates:
        if not item.get("id"):
            continue
        cid = item["id"]
        if cid in old_by_id and (old_by_id[cid]["locked"] or old_by_id[cid]["status"] == "dismissed"):
            continue
        conn.execute("DELETE FROM collection_links WHERE parent_id=%s", (cid,))
        for pos, child in enumerate(item["children"]):
            if child.get("id"):
                conn.execute("INSERT INTO collection_links VALUES (%s,%s,%s) ON CONFLICT DO NOTHING", (cid, child["id"], pos))
        if item['kind'] == 'trip':
            destinations = [p['name'] for p in item['children'] if p['kind']=='place' and p.get('id')]
            if destinations:
                label = ' → '.join(dict.fromkeys(destinations))
                label = label[:180]
                route = [{'place_id':p['id'],'at':stamp.isoformat()} for p,stamp in item['route'] if p.get('id')]
                conn.execute('UPDATE collections SET suggested_name=%s,evidence=evidence || %s WHERE id=%s',
                             (f"{label} · {item['start']:%Y-%m-%d}", Jsonb({'route':route}),cid))
    for previous in old:
        if previous["id"] not in used and not previous["locked"] and not previous["user_name"] and previous["status"] == "suggested":
            conn.execute("DELETE FROM collections WHERE id=%s", (previous["id"],))
    check()
    totals = {k:0 for k in KINDS}
    for row in conn.execute("SELECT kind,count(*) AS n FROM collections WHERE scope_key=%s AND status<>'dismissed' GROUP BY kind",(scope,)):
        totals[row['kind']] = row['n']
    return {"images_examined": len(rows), "collections": totals, "refreshed": counts, "version": VERSION, "scope": scope}


def listing(conn, kind=None, include_dismissed=False, limit=100, offset=0):
    if kind is not None and kind not in KINDS:
        raise ValueError("Unknown collection kind")
    return conn.execute("""SELECT c.*,coalesce(user_name,suggested_name) AS name,
        count(m.image_id) AS image_count,min(m.image_id) AS cover_image_id
        FROM collections c LEFT JOIN collection_images m ON m.collection_id=c.id AND m.source<>'excluded'
        WHERE (%s::text IS NULL OR c.kind=%s) AND (%s OR c.status<>'dismissed')
        GROUP BY c.id ORDER BY c.start_at DESC NULLS LAST,c.id DESC LIMIT %s OFFSET %s""",
        (kind, kind, include_dismissed, limit, offset)).fetchall()


def detail(conn, cid, *, limit=None, offset=0):
    row = conn.execute("SELECT *,coalesce(user_name,suggested_name) AS name FROM collections WHERE id=%s", (cid,)).fetchone()
    if not row:
        raise ValueError("Collection not found")
    row["images"] = conn.execute("""SELECT m.*,i.path,i.capture_at,i.gps_lat,i.gps_lon,i.missing FROM collection_images m
        JOIN images i ON i.id=m.image_id WHERE collection_id=%s AND (%s OR m.source<>'excluded')
        ORDER BY i.capture_at NULLS LAST,i.id LIMIT %s OFFSET %s""", (cid, limit is None, limit, offset)).fetchall()
    row["image_offset"] = offset
    row["image_count"] = conn.execute("SELECT count(*) AS n FROM collection_images WHERE collection_id=%s AND source<>'excluded'", (cid,)).fetchone()['n']
    row["image_more"] = offset + len(row['images']) < row['image_count']
    row["children"] = conn.execute("""SELECT c.id,c.kind,coalesce(c.user_name,c.suggested_name) AS name,c.latitude,c.longitude,c.start_at
        FROM collection_links l JOIN collections c ON c.id=l.child_id WHERE l.parent_id=%s ORDER BY l.position,c.id""", (cid,)).fetchall()
    places = {c['id']:c for c in row['children'] if c['kind']=='place'}
    row['route'] = [places[visit['place_id']] | {'at':visit['at']} for visit in row['evidence'].get('route',[]) if visit['place_id'] in places]
    return row


def review(conn, cid, action, *, name=None, image_ids=None, other_id=None, detail_limit=None):
    with conn.transaction():
        return _review(conn, cid, action, name=name, image_ids=image_ids, other_id=other_id, detail_limit=detail_limit)


def _review(conn, cid, action, *, name=None, image_ids=None, other_id=None, detail_limit=None):
    conn.execute("SELECT pg_advisory_xact_lock(73412861)")
    row = detail(conn, cid, limit=0)
    image_ids = list(dict.fromkeys(image_ids or []))
    if action == "rename":
        if not name or not name.strip() or len(name.strip()) > 240:
            raise ValueError("Name must contain 1–240 characters")
        conn.execute("UPDATE collections SET user_name=%s WHERE id=%s", (name.strip(), cid))
    elif action in ("dismiss", "restore", "confirm"):
        status = {"dismiss": "dismissed", "restore": "suggested", "confirm": "confirmed"}[action]
        conn.execute("UPDATE collections SET status=%s WHERE id=%s", (status, cid))
    elif action in ("add", "remove", "split"):
        if not image_ids or len(conn.execute("SELECT id FROM images WHERE id=ANY(%s)", (image_ids,)).fetchall()) != len(image_ids):
            raise ValueError("Select existing image IDs")
        present = {r["image_id"] for r in conn.execute("SELECT image_id FROM collection_images WHERE collection_id=%s AND source<>'excluded' AND image_id=ANY(%s)",(cid,image_ids))}
        if action in ("remove", "split") and not set(image_ids) <= present:
            raise ValueError("Selected images must belong to this collection")
        if action == "split" and len(image_ids) >= row["image_count"]:
            raise ValueError("Split must leave images in both collections")
        target = cid
        if action == "split":
            target = conn.execute("""INSERT INTO collections(kind,suggested_name,user_name,scope_key,status,locked,evidence)
                VALUES (%s,%s,%s,%s,'confirmed',true,%s) RETURNING id""",
                (row["kind"], row["suggested_name"], name.strip() if name and name.strip() else row["name"]+" · split",
                 row["scope_key"], Jsonb({"reason": "User split"}))).fetchone()["id"]
        for iid in image_ids:
            source = "manual" if action == "add" else "excluded"
            conn.execute("""INSERT INTO collection_images(collection_id,image_id,source) VALUES (%s,%s,%s)
                ON CONFLICT(collection_id,image_id) DO UPDATE SET source=excluded.source,confidence=1,evidence='{}'""", (cid, iid, source))
            if action == "split":
                conn.execute("INSERT INTO collection_images(collection_id,image_id,source) VALUES (%s,%s,'manual')", (target, iid))
        conn.execute("UPDATE collections SET locked=true WHERE id=%s", (cid,))
        if action == "split":
            _refresh_manual(conn, target)
            conn.execute("""INSERT INTO collection_links(parent_id,child_id,position)
                SELECT parent_id,%s,position+1 FROM collection_links WHERE child_id=%s
                AND parent_id<>%s ON CONFLICT DO NOTHING""",(target,cid,target))
            conn.execute("""INSERT INTO collection_links(parent_id,child_id,position)
                SELECT %s,l.child_id,l.position FROM collection_links l WHERE l.parent_id=%s AND EXISTS(
                    SELECT 1 FROM collection_images a JOIN collection_images b ON a.image_id=b.image_id
                    WHERE a.collection_id=%s AND b.collection_id=l.child_id AND a.source<>'excluded' AND b.source<>'excluded')
                ON CONFLICT DO NOTHING""",(target,cid,target))
            _refresh_manual(conn, cid)
            return detail(conn, target, limit=detail_limit)
    elif action == "merge":
        if other_id == cid:
            raise ValueError("Select another collection")
        other = detail(conn, other_id, limit=0)
        if other["kind"] != row["kind"]:
            raise ValueError("Merge collections of the same kind")
        conn.execute("""INSERT INTO collection_images(collection_id,image_id,source)
            SELECT %s,image_id,'manual' FROM collection_images WHERE collection_id=%s AND source<>'excluded'
            ON CONFLICT(collection_id,image_id) DO UPDATE SET source='manual',confidence=1,evidence='{}'""", (cid, other_id))
        conn.execute("UPDATE collections SET locked=true,status='dismissed' WHERE id=%s", (other_id,))
        conn.execute("UPDATE collections SET locked=true WHERE id=%s", (cid,))
        conn.execute("""INSERT INTO collection_links SELECT %s,child_id,position FROM collection_links WHERE parent_id=%s
            AND child_id<>%s ON CONFLICT DO NOTHING""", (cid, other_id, cid))
        parents = conn.execute("SELECT parent_id FROM collection_links WHERE child_id=%s",(other_id,)).fetchall()
        conn.execute("""INSERT INTO collection_links SELECT parent_id,%s,position FROM collection_links
            WHERE child_id=%s AND parent_id<>%s ON CONFLICT DO NOTHING""",(cid,other_id,cid))
        conn.execute('DELETE FROM collection_links WHERE child_id=%s',(other_id,))
        for parent in parents:
            evidence = conn.execute('SELECT evidence FROM collections WHERE id=%s',(parent['parent_id'],)).fetchone()['evidence']
            for visit in evidence.get('route',[]):
                if visit['place_id']==other_id:
                    visit['place_id']=cid
            conn.execute('UPDATE collections SET evidence=%s WHERE id=%s',(Jsonb(evidence),parent['parent_id']))
    else:
        raise ValueError("Unknown review action")
    _refresh_manual(conn, cid)
    return detail(conn, cid, limit=detail_limit)


def _refresh_manual(conn, cid):
    conn.execute('UPDATE collections SET updated_at=now() WHERE id=%s',(cid,))
    collection = conn.execute('SELECT locked,kind FROM collections WHERE id=%s',(cid,)).fetchone()
    if not collection['locked']:
        return
    rows = conn.execute("""SELECT i.capture_at,i.capture_offset_minutes,i.exif,i.gps_lat,i.gps_lon FROM collection_images m
        JOIN images i ON i.id=m.image_id WHERE m.collection_id=%s AND m.source<>'excluded' ORDER BY i.id""",(cid,)).fetchall()
    times = [clock(r)[0] for r in rows if r['capture_at']]
    conn.execute('UPDATE collections SET start_at=%s,end_at=%s WHERE id=%s',
                 (min(times) if times else None,max(times) if times else None,cid))
    if collection['kind']=='place':
        point = next((coordinate(r) for r in rows if coordinate(r)),(None,None))
        conn.execute('UPDATE collections SET latitude=%s,longitude=%s WHERE id=%s',(*point,cid))


def _geocode(provider, lat, lon):
    # A separate session preserves throttle state even if a request fails.
    with connect() as cache:
        cache.execute('SELECT pg_advisory_lock(73412862)')
        try:
            cached = cache.execute("SELECT response FROM place_lookup_cache WHERE provider=%s AND latitude=%s AND longitude=%s",
                                   (provider,lat,lon)).fetchone()
            if cached:
                return cached['response'], True
            last = cache.execute("SELECT value FROM collection_settings WHERE key='geocoder_last_request'").fetchone()
            if last:
                time.sleep(max(0, 1.1-(time.time()-float(last['value']))))
            cache.execute("""INSERT INTO collection_settings VALUES ('geocoder_last_request',%s)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value""", (Jsonb(time.time()),))
            cache.commit()
            request = Request(provider + '?' + urlencode({'lat':lat,'lon':lon,'format':'jsonv2','zoom':14,'addressdetails':1}),
                              headers={'User-Agent':'image-atlas/0.1 (local photo collection review)','Accept':'application/json'})
            with urlopen(request, timeout=15) as result:
                response = json.loads(result.read(1_000_000))
            if not isinstance(response,dict) or not response.get('display_name'):
                raise ValueError('No place name returned')
            cache.execute('INSERT INTO place_lookup_cache(provider,latitude,longitude,response) VALUES (%s,%s,%s,%s)',
                          (provider,lat,lon,Jsonb(response)))
            cache.commit()
            return response, False
        finally:
            if cache.info.transaction_status.name == 'INERROR':
                cache.rollback()
            cache.execute('SELECT pg_advisory_unlock(73412862)')


def lookup_place(conn, cid):
    row = detail(conn, cid, limit=0)
    if row["kind"] != "place" or row["latitude"] is None:
        raise ValueError("Select a GPS place")
    provider = settings(conn)["geocoder_url"]
    lat, lon = round(row["latitude"], 5), round(row["longitude"], 5)
    response, cached = _geocode(provider,lat,lon)
    address = response.get("address", {})
    town = next((address.get(k) for k in ("city", "town", "village", "municipality") if address.get(k)), None)
    name = response.get("name") or town or response["display_name"].split(",")[0]
    if town and name != town:
        name += " · " + town
    conn.execute('SELECT pg_advisory_xact_lock(73412861)')
    conn.execute("UPDATE collections SET suggested_name=%s,evidence=evidence || %s,updated_at=now() WHERE id=%s",
                 (name, Jsonb({'lookup_name':name}), cid))
    return {"id": cid, "suggested_name": name, "attribution": "© OpenStreetMap contributors", "cached": bool(cached)}
