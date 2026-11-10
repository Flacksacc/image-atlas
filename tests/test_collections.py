from datetime import datetime, timedelta
import io
import json
import time
import threading

from fastapi.testclient import TestClient
from psycopg.types.json import Jsonb
import pytest
from cli_helpers import JsonCliRunner

from image_atlas import collections as c
from image_atlas.cli import app as cli
from image_atlas.db import connect, vector_literal
from image_atlas.jobs import MiningCancelled
from image_atlas.web import app


def seed(conn, hours, points=None, hashes=None, offsets=None):
    ids = []
    for n, hour in enumerate(hours):
        point = (points or [None]*len(hours))[n]
        row = conn.execute("""INSERT INTO images(path,root_path,folder_path,size_bytes,mtime_ns,sha256,
            capture_at,gps_lat,gps_lon,capture_offset_minutes) VALUES (%s,'test','test',100,1,%s,%s,%s,%s,%s) RETURNING id""",
            (f"photo-{n}-{time.time_ns()}.jpg", (hashes or [f"hash-{j}" for j in range(len(hours))])[n],
             datetime(2026, 6, 1)+timedelta(hours=hour) if hour is not None else None,
             *(point or (None, None)), (offsets or [None]*len(hours))[n])).fetchone()
        ids.append(row['id'])
    return ids


def active_ids(conn, cid):
    return {r['image_id'] for r in c.detail(conn, cid)['images'] if r['source'] != 'excluded'}


def test_events_trips_places_and_repeated_visits(monkeypatch):
    monkeypatch.setattr(c, 'urlopen', lambda *a, **k: pytest.fail('Detection must stay offline'))
    with connect() as conn:
        ids = seed(conn, [10,10.5,14,14.5,110,110.5,None],
                   [(30,-81),(30.0001,-81),(30.3,-81),(30.3001,-81),(30,-81),(30.0001,-81),None])
        result = c.detect(conn)
        assert result['collections'] == {'place':2,'event':3,'trip':1}
        places = c.listing(conn, 'place')
        assert sorted(p['image_count'] for p in places) == [2,4]
        trip = c.listing(conn, 'trip')[0]
        assert active_ids(conn, trip['id']) == set(ids[:4])
        assert len(c.detail(conn, trip['id'])['children']) == 4
        assert ids[-1] not in {r['image_id'] for r in conn.execute('SELECT * FROM collection_images')}
        before = {r['id'] for r in c.listing(conn)}
        c.detect(conn)
        assert before == {r['id'] for r in c.listing(conn)}


def test_edits_split_merge_dismiss_survive_redetection():
    with connect() as conn:
        ids = seed(conn, [10,10.5,11,11.5,80,80.5])
        c.detect(conn)
        events = c.listing(conn,'event')
        first = next(e for e in events if e['image_count']==4)['id']
        second = next(e for e in events if e['image_count']==2)['id']
        c.review(conn,first,'rename',name='Our birthday')
        c.detect(conn)
        assert c.detail(conn,first)['name'] == 'Our birthday'
        new = c.review(conn,first,'split',image_ids=ids[:2],name='Dinner')
        c.detect(conn)
        assert active_ids(conn,first) == set(ids[2:4])
        assert active_ids(conn,new['id']) == set(ids[:2])
        assert len(c.listing(conn,'event')) == 3
        c.review(conn,first,'merge',other_id=new['id'])
        c.review(conn,first,'remove',image_ids=[ids[0]])
        c.review(conn,first,'add',image_ids=[ids[-1]])
        c.review(conn,second,'dismiss')
        c.detect(conn)
        assert active_ids(conn,first) == {ids[1],ids[2],ids[3],ids[-1]}
        assert len(c.listing(conn,'event')) == 1
        c.review(conn,second,'restore')
        assert len(c.listing(conn,'event')) == 2


def test_exact_duplicates_do_not_create_event_but_all_copies_are_members():
    with connect() as conn:
        ids = seed(conn,[10,10,10.5],hashes=['same','same','different'])
        c.detect(conn)
        event = c.listing(conn,'event')[0]
        assert active_ids(conn,event['id']) == set(ids)
        conn.execute('DELETE FROM images WHERE id=%s',(ids[-1],))
        c.detect(conn)
        assert c.listing(conn,'event') == []


def test_gps_cluster_extent_outliers_and_date_line():
    with connect() as conn:
        seed(conn,[10,10.2,10.4,10.6,10.8,11],points=[(0,0),(0,.002),(0,.004),(0,.006),(0,179.999),(0,-179.999)])
        c.detect(conn)
        places = c.listing(conn,'place')
        assert sorted(p['image_count'] for p in places) == [2,2,2]


def test_known_offsets_normalize_but_unknown_clocks_stay_separate():
    with connect() as conn:
        ids = seed(conn,[10,12.5,10.2,10.4],offsets=[0,120,None,None])
        c.detect(conn)
        events = c.listing(conn,'event')
        assert len(events)==2
        assert {frozenset(active_ids(conn,e['id'])) for e in events} == {frozenset(ids[:2]),frozenset(ids[2:])}
        assert {e['evidence']['duration_precise'] for e in events} == {True,False}


def test_strong_people_visual_evidence_can_suggest_undated_membership():
    with connect() as conn:
        ids=seed(conn,[10,10.5,None,None],points=[(30,-81),(30,-81),None,(40,-81)])
        group=conn.execute('INSERT INTO face_groups DEFAULT VALUES RETURNING id').fetchone()['id']
        people=[conn.execute('INSERT INTO people(name) VALUES (%s) RETURNING id',(name,)).fetchone()['id'] for name in ('Alice','Bob')]
        for iid in ids:
            conn.execute("UPDATE images SET embedding=%s,embedding_model='test-model' WHERE id=%s",(vector_literal([1.]+[0.]*511),iid))
            for person in people:
                conn.execute('INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id) VALUES (%s,%s,0,10,10,%s,%s,%s)',
                             (iid,person,vector_literal([1.]+[0.]*127),group,person))
        c.detect(conn)
        event=c.listing(conn,'event')[0]
        assert active_ids(conn,event['id']) == set(ids[:3])
        inferred=next(r for r in c.detail(conn,event['id'])['images'] if r['image_id']==ids[2])
        assert inferred['confidence'] < event['confidence']
        assert 'visual' in inferred['evidence']['reason']


def test_cancel_during_publication_rolls_back_entire_refresh():
    with connect() as conn:
        seed(conn,[10,10.5])
        c.detect(conn)
        existing=c.listing(conn)
        conn.commit()
        calls=0
        def cancel():
            nonlocal calls
            calls+=1
            if calls==7:
                raise MiningCancelled()
        with pytest.raises(MiningCancelled):
            c.detect(conn,check=cancel)
        assert c.listing(conn)==existing


def test_review_validation_is_atomic():
    with connect() as conn:
        ids=seed(conn,[10,10.5])
        c.detect(conn)
        cid=c.listing(conn,'event')[0]['id']
        for action,args in [('split',{'image_ids':ids}),('add',{'image_ids':[999]}),('rename',{'name':' '}),('merge',{'other_id':cid})]:
            with pytest.raises(ValueError):c.review(conn,cid,action,**args)
        assert active_ids(conn,cid)==set(ids)
        assert len(c.listing(conn))==1


def test_geocoder_explicit_cached_preserves_user_name(monkeypatch):
    calls=[]
    def lookup(request,timeout):
        calls.append(request)
        return io.BytesIO(json.dumps({'display_name':'Park, Charleston','name':'Park','address':{'city':'Charleston'}}).encode())
    monkeypatch.setattr(c,'urlopen',lookup)
    with connect() as conn:
        seed(conn,[10,10.5],[(30,-81),(30,-81)])
        c.detect(conn)
        place=c.listing(conn,'place')[0]
        c.review(conn,place['id'],'rename',name='Our park')
        assert c.lookup_place(conn,place['id'])['cached'] is False
        assert c.lookup_place(conn,place['id'])['cached'] is True
        assert len(calls)==1
        assert calls[0].get_header('User-agent').startswith('image-atlas')
        assert c.detail(conn,place['id'])['name']=='Our park'
        c.detect(conn)
        assert c.detail(conn,place['id'])['suggested_name']=='Park · Charleston'
        trip_names = c.listing(conn,'event')
        assert 'Our park' in trip_names[0]['name']


def test_failed_geocoder_requests_are_throttled_and_do_not_rename(monkeypatch):
    sleeps=[]
    monkeypatch.setattr(c.time,'sleep',sleeps.append)
    def fail(*args,**kwargs):
        raise OSError('offline')
    monkeypatch.setattr(c,'urlopen',fail)
    with connect() as conn:
        seed(conn,[10],[(30,-81)])
        c.detect(conn)
        place=c.listing(conn,'place')[0]
        for _ in range(2):
            with pytest.raises(OSError):c.lookup_place(conn,place['id'])
        assert any(delay>0 for delay in sleeps)
        assert c.detail(conn,place['id'])['name']==place['name']


def test_place_id_survives_large_membership_growth():
    with connect() as conn:
        seed(conn,[10],[(30,-81)])
        c.detect(conn)
        cid=c.listing(conn,'place')[0]['id']
        seed(conn,[11,11.5,12,12.5],[(30,-81)]*4,hashes=['a','b','c','d'])
        c.detect(conn)
        assert len(c.listing(conn,'place'))==1
        assert c.listing(conn,'place')[0]['id']==cid


def test_named_event_keeps_id_when_more_photos_are_indexed():
    with connect() as conn:
        seed(conn,[10,10.1])
        c.detect(conn)
        cid=c.listing(conn,'event')[0]['id']
        c.review(conn,cid,'rename',name='Our party')
        seed(conn,[10.2+n*.1 for n in range(6)],hashes=[f'new-{n}' for n in range(6)])
        c.detect(conn)
        events=c.listing(conn,'event')
        assert len(events)==1 and events[0]['id']==cid and events[0]['name']=='Our party'


def test_25000_image_database_publication():
    with connect() as conn:
        conn.execute("""INSERT INTO images(path,root_path,size_bytes,mtime_ns,sha256,capture_at,capture_offset_minutes,gps_lat,gps_lon)
            SELECT 'benchmark-'||n,'benchmark',100,1,'benchmark-hash-'||n,
                timestamp '2026-01-01'+n*interval '1 minute',0,30+(n/1000)*.1,-81
            FROM generate_series(0,24999) n""")
        start=time.perf_counter()
        result=c.detect(conn)
        elapsed=time.perf_counter()-start
        print(f'25,000-image database detection/publication: {elapsed:.3f}s')
        assert result['images_examined']==25000
        assert conn.execute('SELECT count(DISTINCT image_id) AS n FROM collection_images').fetchone()['n']==25000


def test_web_and_cli_collection_filters_and_review():
    with connect() as conn:
        ids=seed(conn,[10,10.5,80,80.5])
        c.detect(conn)
    client=TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    event=client.get('/api/collections?kind=event').json()[0]
    cid=event['id']
    response=client.post(f'/api/collections/{cid}/review',json={'action':'rename','name':'Holiday'})
    assert response.status_code==200,response.text
    images=client.get('/api/images',params={'collection_id':cid,'group_by':'Event','limit':1}).json()
    more=client.get('/api/images',params={'collection_id':cid,'group_by':'Event','limit':1,'offset':1}).json()
    assert len(images+more)==2 and images[0]['id']!=more[0]['id']
    assert 'Holiday' in images[0]['image_group']
    assert len(client.get('/api/images?q=Holiday').json())==2
    result=JsonCliRunner().invoke(cli,['collections','show',str(cid)])
    assert result.exit_code==0,result.output
    assert json.loads(result.stdout)['name']=='Holiday'
    assert client.get('/api/collections?kind=invalid').status_code==400
    assert client.get('/api/collections/999').status_code==404
    with connect() as conn:
        conn.execute('UPDATE images SET missing=true WHERE id=%s',(ids[-1],))
    assert client.get(f'/api/collections/{cid}').json()['stale']


def test_25000_image_clustering_benchmark():
    rows=[]
    for n in range(25000):
        rows.append({'id':n,'sha256':str(n),'capture_at':datetime(2026,1,1)+timedelta(minutes=n),
                     'capture_offset_minutes':0,'exif':{},'gps_lat':30+(n//1000)*.1,'gps_lon':-81,
                     'people':[],'labels':[],'embedding':None})
    start=time.perf_counter()
    result=c.build_candidates(rows,c.DEFAULTS)
    elapsed=time.perf_counter()-start
    print(f'25,000-image clustering: {elapsed:.3f}s; {len(result)} collections')
    assert len([r for r in result if r['kind']=='place'])==25
    assert len([r for r in result if r['kind']=='trip'])>=1


def test_return_visits_and_trip_length_cap():
    with connect() as conn:
        seed(conn,[10,14,18],points=[(30,-81),(30.3,-81),(30,-81)])
        c.detect(conn)
        route=c.detail(conn,c.listing(conn,'trip')[0]['id'])['route']
        assert len(route)==3 and route[0]['id']==route[2]['id']!=route[1]['id']
    rows=[{'id':n,'sha256':str(n),'capture_at':datetime(2026,1,1)+timedelta(hours=n*12),
           'capture_offset_minutes':0,'exif':{},'gps_lat':30+(n%2)*.3,'gps_lon':-81,
           'people':[],'labels':[],'embedding':None} for n in range(50)]
    trips=[r for r in c.build_candidates(rows,c.DEFAULTS) if r['kind']=='trip']
    assert len(trips)==2
    assert all(r['end']-r['start']<=timedelta(days=21) for r in trips)


def test_occasion_hint_and_scoped_detection(tmp_path):
    from image_atlas.db import normalized_path
    with connect() as conn:
        ids=seed(conn,[10,10.5,11,11.5])
        scope=normalized_path(tmp_path)
        conn.execute('UPDATE images SET root_path=%s WHERE id=ANY(%s)',(scope,ids[:2]))
        for iid in ids[:2]:
            conn.execute("INSERT INTO image_labels(image_id,label,score,model_id,version) VALUES (%s,'wedding',.3,'test',2)",(iid,))
        result=c.detect(conn,str(tmp_path))
        assert result['images_examined']==2
        event=c.listing(conn,'event')[0]
        assert event['name'].startswith('Wedding')
        assert active_ids(conn,event['id'])==set(ids[:2])


def test_web_detection_job_can_cancel_without_publishing(monkeypatch):
    started,release=threading.Event(),threading.Event()
    original=c.detect
    def delayed(conn,*args,**kwargs):
        started.set()
        assert release.wait(5)
        return original(conn,*args,**kwargs)
    monkeypatch.setattr(c,'detect',delayed)
    with connect() as conn:
        seed(conn,[10,10.5])
        original(conn)
        before=c.listing(conn)
    client=TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    response=client.post('/api/collections/detect')
    assert response.status_code==200,response.text
    job=response.json()['id']
    try:
        assert started.wait(5)
        assert client.post('/api/collections/detect').status_code==409
        assert client.post(f'/api/collections/jobs/{job}/cancel').status_code==200
    finally:
        release.set()
    deadline=time.monotonic()+5
    while time.monotonic()<deadline:
        state=client.get(f'/api/collections/jobs/{job}').json()
        if state['status']!='running':break
        time.sleep(.01)
    assert state['status']=='cancelled'
    with connect() as conn:
        assert c.listing(conn)==before
