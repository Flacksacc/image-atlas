"""Local review gallery. It serves only images already recorded in the database."""
import io
import psycopg
from pathlib import Path
import threading

from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.responses import HTMLResponse, Response
from fastapi.responses import JSONResponse
from PIL import Image, ImageOps
from pydantic import BaseModel

from . import __version__
from .db import connect
from .faces import name_group
from .gallery_sort import GROUP_FIELDS, SORT_FIELDS, oriented_sql
from .indexer import THUMBNAIL_VERSION

app = FastAPI(title="image-atlas gallery", docs_url=None, redoc_url=None)
import secrets
from urllib.parse import urlsplit
app.state.session_token = secrets.token_urlsafe(32)
app.state.allowed_hosts = {'localhost', '127.0.0.1', '::1'}
app.state.secure_cookie = False

def configure_security(host, *, secure=False, allowed_hosts=()):
    app.state.session_token = secrets.token_urlsafe(32)
    app.state.secure_cookie = secure
    app.state.allowed_hosts = {'localhost', '127.0.0.1', '::1'}
    if host not in {'0.0.0.0', '::'}:
        app.state.allowed_hosts.add(host.lower())
    app.state.allowed_hosts.update(name.lower() for name in allowed_hosts)
COLLECTION_JOBS = {}
COLLECTION_JOB_LOCK = threading.Lock()
IMAGE_SEARCH_LOCK = threading.Lock()


@app.middleware('http')
async def catalog_context(request, call_next):
    from .catalogs import current, resolve, registry
    token = current.set(None)
    try:
        path = request.scope['path']
        prefix = '/api/catalogs/'
        if path.startswith(prefix) and '/' in path[len(prefix):]:
            identifier, remainder = path[len(prefix):].split('/', 1)
            catalog = resolve(identifier)
            current.set(catalog)
            if (catalog.readonly and request.method not in {'GET', 'HEAD'}
                    and not remainder.startswith('cancel/') and remainder not in {'search/image', 'search/image/page'}):
                return JSONResponse({'detail': 'Catalog is read-only; assign folders before editing'}, status_code=403)
            request.scope['path'] = '/api/' + remainder
            request.scope['raw_path'] = request.scope['path'].encode()
        elif path.startswith('/api/') and not path.startswith('/api/catalogs'):
            if registry()['catalogs']:
                return JSONResponse({'detail': 'A catalog UUID is required'}, status_code=400)
        return await call_next(request)
    except ValueError as exc:
        return JSONResponse({'detail': str(exc)}, status_code=400)
    finally:
        current.reset(token)


@app.middleware('http')
async def session_security(request, call_next):
    try:
        host = urlsplit('http://' + request.headers.get('host', '')).hostname
    except ValueError:
        host = None
    if not host or host.lower() not in app.state.allowed_hosts:
        return JSONResponse({'detail': 'Untrusted gallery host'}, status_code=400)
    origin = request.headers.get('origin')
    if origin and origin != str(request.base_url).rstrip('/'):
        return JSONResponse({'detail': 'Untrusted gallery origin'}, status_code=403)
    supplied = (request.headers.get('x-image-atlas-session') or request.headers.get('x-image-cli-session')
                or request.cookies.get('image-atlas-session', ''))
    authorized = secrets.compare_digest(supplied, app.state.session_token)
    if not authorized:
        if request.url.path == '/' and request.method == 'GET':
            return HTMLResponse(SESSION_LANDING, headers={'Cache-Control': 'no-store'})
        return JSONResponse({'detail': 'Open the gallery session link printed by image-atlas'}, status_code=401)
    response = await call_next(request)
    response.headers['Cache-Control'] = 'no-store'
    response.headers['X-Content-Type-Options'] = 'nosniff'
    response.headers['Referrer-Policy'] = 'no-referrer'
    return response

SESSION_LANDING = """<!doctype html><meta charset="utf-8"><title>ImageAtlas session</title>
<p>Open the session link printed by image-atlas to access this gallery.</p>
<script>const token=new URLSearchParams(location.hash.slice(1)).get('session');
history.replaceState(null,'',location.pathname);
if(token)fetch('/session',{method:'POST',headers:{'X-Image-Atlas-Session':token}})
.then(r=>{if(!r.ok)throw Error('Session link expired');location.reload()})
.catch(e=>document.querySelector('p').textContent=e.message);</script>"""

@app.post('/session')
def start_session():
    response = Response(status_code=204)
    response.set_cookie('image-atlas-session', app.state.session_token, httponly=True, samesite='strict', secure=app.state.secure_cookie)
    return response

@app.get('/api/catalogs')
def catalog_list():
    from .catalogs import registry
    data = registry()
    return {'default': data['default'], 'catalogs': [{k: v for k, v in r.items() if k not in {'connection', 'credential'}} for r in data['catalogs']]}


class CatalogOperation(BaseModel):
    action: str
    name: str = ''
    catalog_id: str | None = None
    dsn: str | None = None
    roots: list[str] = []
    path: str = ''


@app.post('/api/catalogs')
def catalog_manage(request: CatalogOperation):
    from .catalogs import create, register, edit_roots, use, resolve, current
    from .catalog_backup import backup, restore
    from .jobs import MiningCancelled
    token = current.set(resolve(request.catalog_id))
    try:
        if request.action == 'create':
            result = create(request.name, request.roots)
        elif request.action == 'register':
            result = register(request.name, request.dsn or resolve().dsn, request.roots or None)
        elif request.action == 'roots':
            result = edit_roots(request.catalog_id, request.roots)
        elif request.action == 'default':
            result = use(request.catalog_id)
        elif request.action == 'backup':
            result = backup(Path(request.path))
        elif request.action == 'restore':
            result = restore(Path(request.path), request.name)
        else:
            raise ValueError('Unknown catalog operation')
        return {k: v for k, v in result.items() if k not in {'connection', 'credential'}}
    except (ValueError, psycopg.Error, OSError) as exc:
        raise HTTPException(400, str(exc)) from exc
    except MiningCancelled:
        return {'catalog_id': resolve().id, 'cancelled': True}
    finally:
        current.reset(token)

HTML = r"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>ImageAtlas</title><link rel="icon" href="/icon.ico"><style>
body{margin:0;background:#101820;color:#e9eef2;font:16px system-ui}header{padding:20px 28px;background:#17242d;position:sticky;top:0;z-index:1}
h1{margin:0 0 12px;font-size:24px}nav button,button{background:#2688a0;color:#fff;border:0;border-radius:5px;padding:9px 13px;cursor:pointer}
nav button{margin-right:8px}input{padding:9px;border:1px solid #63717a;border-radius:5px;background:#fff;color:#111;min-width:210px}
select{padding:8px;border-radius:5px;max-width:100%}.toolbar{display:flex;flex-wrap:wrap;align-items:center;gap:6px;margin:8px 0}.toolbar button.active{outline:2px solid #8ddded;background:#1b6679}.toolbar svg{width:17px;height:17px;vertical-align:middle;margin-right:5px;fill:none;stroke:currentColor;stroke-width:1.5}.toolbar label{white-space:normal}.card h3{margin:0 0 8px}header p{display:flex;flex-wrap:wrap;align-items:center;gap:6px}h2.group-heading{font-size:18px;overflow-wrap:anywhere}input[type=checkbox]{min-width:0;accent-color:#2688a0}label{white-space:nowrap}
main{padding:20px 28px}.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(220px,1fr));gap:16px}.card{background:#1c2b35;border-radius:8px;padding:10px;overflow:hidden}
.card input,.card select,.card textarea{max-width:100%;box-sizing:border-box;min-width:0}.card input:not([type=checkbox]),.card select{width:100%}.card h3{overflow-wrap:anywhere}.card img{display:block;width:160px;max-width:100%;height:160px;object-fit:contain;background:#0a1117;margin:0 auto 8px}.card small{display:block;word-break:break-all;color:#b9c6cd;margin:6px 0}
.group img{width:120px;height:120px;object-fit:cover} .faces{display:flex;flex-wrap:wrap;gap:6px}.hint{color:#9fb3bf}pre{white-space:pre-wrap;word-break:break-word}
header h1{display:flex;align-items:center;gap:10px}dialog{max-width:min(360px,85vw);max-height:85vh;overflow:auto;text-align:center;background:#17242d;color:#e9eef2;border:1px solid #63717a;border-radius:12px;padding:24px}dialog img{display:block;margin:auto}dialog a{color:#8ddded}dialog::backdrop{background:#0009}
</style></head><body><header><h1><img src="/logo.png" alt="" width="42" height="42"> ImageAtlas</h1><nav><button title="Review, sort, and group indexed images" onclick="loadImages()">Images</button><button title="Review face samples and assign or ignore person identities" onclick="loadGroups()">People</button><button title="Review suggested file destinations without moving originals" onclick="loadProposals()">Sort proposals</button><button onclick="document.getElementById('about-dialog').showModal()">About ImageAtlas</button></nav>
<p><label>Search model <select id="embedding-model"><option value="openclip">OpenCLIP</option><option value="embeddinggemma2">EmbeddingGemma 2</option></select></label> <label><input id="semantic-search" type="checkbox"> Image content</label><input id="query" placeholder="Filename or description"><button onclick="loadImages()">Search</button> <button onclick="document.getElementById('image-query').click()">Search by image…</button><input id="image-query" type="file" accept="image/*" hidden> <label><input id="show-missing" type="checkbox" onchange="loadImages()"> Show missing files</label></p><div id="sort-tools"></div></header><main id="main"></main>
<dialog id="about-dialog" aria-labelledby="about-title"><img src="/logo.png" alt="ImageAtlas globe made of photographs" width="160" height="160"><h2 id="about-title">ImageAtlas</h2><p>Version __IMAGEATLAS_VERSION__</p><p>Find, explore, and organize your image library.</p><p><a href="https://github.com/Flacksacc/image-atlas">GitHub repository</a></p><form method="dialog"><button>Close</button></form></dialog>
<script>
const main=document.getElementById('main');const escapeHtml=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
async function get(path){let r=await fetch(path);if(!r.ok)throw Error(await responseError(r));return r.json()}
let collectionFilter=null;let imageRows=[],imagePageSize=100,imageGeneration=0,activeView='images',sortField='DateTaken',sortDescending=true,secondaryField='Filename',secondaryDescending=false,groupField='None',hasMore=false,imageRequestParams={};
const sortOptions=[["Relevance", "Newest indexed"], ["DateTaken", "Date taken"], ["DateModified", "Modified"], ["DateIndexed", "Indexed"], ["LastViewed", "Last viewed"], ["Filename", "Filename"], ["Path", "Path"], ["Folder", "Folder"], ["Bytes", "File size"], ["Width", "Width"], ["Height", "Height"], ["Pixels", "Pixels"], ["Aspect", "Aspect ratio"], ["Format", "Format"], ["Camera", "Camera"], ["Rating", "Rating"], ["Faces", "Face count"], ["Duplicates", "Exact duplicates"], ["FolderCount", "Folder image count"], ["FolderBytes", "Folder total bytes"], ["Random", "Stable shuffle"]];
const groupOptions=[['None','No grouping'],['People','People'],['FaceGroup','Face sample sets'],['Day','Day'],['Month','Month'],['Year','Year'],['Folder','Folder'],['Camera','Camera'],['Format','Format'],['Rating','Rating'],['Review','Review state'],['FaceCount','Face count'],['Orientation','Orientation'],['Resolution','Resolution'],['FileSize','File size']];
const sortHints={"Relevance": "Newest indexed files first. The web gallery searches filenames and paths; visual relevance is available in the native viewer.", "DateTaken": "Capture timestamp from camera metadata, with file date fallback.", "DateModified": "Filesystem modification time.", "DateIndexed": "When the catalog last indexed the image.", "LastViewed": "When you last opened the image in this viewer; never-viewed images come last.", "Filename": "File name, case insensitive.", "Path": "Full file path, case insensitive.", "Folder": "Containing folder, case insensitive.", "Bytes": "Original file size in bytes.", "Width": "Image width in pixels.", "Height": "Image height in pixels.", "Pixels": "Width \u00d7 height in pixels.", "Aspect": "Width divided by height; wide images have larger values.", "Format": "File format.", "Camera": "Camera model from image metadata.", "Rating": "Your zero-to-five-star rating.", "Faces": "Number of detected faces in the image.", "Duplicates": "Number of catalog files with the same SHA-256 content hash.", "FolderCount": "Total catalog image count in the containing folder.", "FolderBytes": "Total original-file bytes in the containing folder.", "Random": "Deterministic shuffle, stable while paging or reopening a saved view."};const groupHints={"None": "Show one continuous list in the selected sort order.", "People": "Group by the set of confirmed people in each image. Photos of several people stay together; ignored identities are excluded. Unnamed faces and images without faces have separate groups.", "FaceGroup": "Group by the set of detected face sample IDs, including unconfirmed identities. An image appears once even if it contains several faces.", "Year": "Group by camera capture date, with file-date fallback when metadata is missing.", "Folder": "Group by the image's containing folder, without moving files.", "Camera": "Group by camera model; missing metadata appears under Unknown camera.", "Format": "Group by file format.", "Rating": "Group by your zero-to-five-star rating.", "Review": "Group by review state.", "FaceCount": "Group by number of detected faces.", "Orientation": "Separate landscape, portrait, square, and images with unknown dimensions.", "Resolution": "Separate images below 1 MP, 1\u20134 MP, 4\u201312 MP, and at least 12 MP.", "FileSize": "Separate files below 1 MiB, 1\u201310 MiB, 10\u2013100 MiB, and at least 100 MiB.", "Day": "Group by camera capture date, with file-date fallback when metadata is missing.", "Month": "Group by camera capture date, with file-date fallback when metadata is missing."};
const quickSorts=[['DateTaken','Date','M3 5h18v16H3z M3 9h18 M7 2v5 M17 2v5 M7 13h2 M13 13h2 M7 17h2 M13 17h2'],['Filename','Name','M4 20L10 4l6 16 M6 14h8 M19 4v16 M17 18l2 2 2-2'],['Bytes','Size','M3 3h18v18H3z M7 3v6h10V3 M7 15h10v6H7z'],['Pixels','Pixels','M3 3h18v18H3z M7 17L17 7 M12 7h5v5'],['Folder','Folder','M2 6h8l3 3h9v12H2z'],['Camera','Camera','M2 7h20v14H2z M7 7V4h10v3 M16 14a4 4 0 1 1-8 0 4 4 0 0 1 8 0'],['Rating','Rating','M12 2l3 7 7 1-5 5 1 7-6-4-6 4 1-7-5-5 7-1z'],['Faces','Faces','M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0 M8 9h1 M15 9h1 M8 16h8'],['Duplicates','Copies','M3 3h13v13H3z M8 8h13v13H8z'],['Random','Shuffle','M3 5l18 14 M17 19h4v-4 M3 19L21 5 M17 5h4v4']];
function options(items,value){return items.map(([key,label])=>'<option value="'+key+'"'+(key===value?' selected':'')+'>'+escapeHtml(label)+'</option>').join('')}
function sortToolbar(){document.getElementById('sort-tools').innerHTML='<div class="toolbar" aria-label="Sort images"><strong>Sort images</strong>'+quickSorts.map(([key,label,path])=>'<button class="'+(sortField===key?'active':'')+'" title="'+escapeHtml(sortHints[key])+' Click again to reverse." onclick="quickSort(\''+key+'\')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="'+path+'"/></svg>'+label+'</button>').join('')+'</div><div class="toolbar"><label>Order <select id="sort-primary" title="'+escapeHtml(sortHints[sortField])+'" onchange="sortField=this.value;loadImages()">'+options(sortOptions,sortField)+'</select></label><button title="Reverse primary sort" onclick="sortDescending=!sortDescending;loadImages()">'+(sortDescending?'Descending':'Ascending')+'</button><label>then <select id="sort-secondary" title="'+escapeHtml(sortHints[secondaryField])+'" onchange="secondaryField=this.value;loadImages()">'+options(sortOptions,secondaryField)+'</select></label><button title="Reverse secondary sort" onclick="secondaryDescending=!secondaryDescending;loadImages()">'+(secondaryDescending?'Desc':'Asc')+'</button><label>Group by <select id="image-grouping" title="'+escapeHtml(groupHints[groupField])+'" onchange="groupField=this.value;loadImages()">'+options(groupOptions,groupField)+'</select></label><button title="Restore newest date first with no grouping" onclick="sortField=\'DateTaken\';sortDescending=true;secondaryField=\'Filename\';secondaryDescending=false;groupField=\'None\';loadImages()">Reset</button></div>'}
function quickSort(key){if(sortField===key)sortDescending=!sortDescending;else{sortField=key;sortDescending=!['Filename','Path','Folder','Format','Camera'].includes(key)}loadImages()}
function imageCard(x){let hint=x.path+' · '+x.width+' × '+x.height+' · '+x.size_bytes+' bytes · Date '+(x.capture_day||'unknown')+' · Camera '+(x.camera_model||'unknown')+' · '+x.rating+' stars · '+x.review_state;return '<div class="card" title="'+escapeHtml(hint)+'"><img loading="lazy" alt="Image #'+x.id+'" src="/api/images/'+x.id+'/thumb"><b>#'+x.id+(x.missing?' · Missing original':'')+'</b><small>'+escapeHtml(x.path)+'</small><small title="Saved collections">'+escapeHtml(x.collections||'')+'</small></div>'}
function renderImages(){let body='';if(groupField==='None')body='<div class="grid">'+imageRows.map(imageCard).join('')+'</div>';else{let groups=new Map();for(let x of imageRows){let key=x.image_group||'Unknown';if(!groups.has(key))groups.set(key,[]);groups.get(key).push(x)}for(let [key,rows] of groups)body+='<h2 class="group-heading" title="'+escapeHtml(key)+'">'+escapeHtml(key)+'</h2><div class="grid">'+rows.map(imageCard).join('')+'</div>'}main.innerHTML='<p class="hint" title="Sorting and grouping apply before pagination">'+imageRows.length+' images loaded'+(collectionFilter?' · Collection #'+collectionFilter+' <button title="Show all catalog images" onclick="collectionFilter=null;loadImages()">Clear collection filter</button>':'')+'</p>'+body+(hasMore?'<p><button id="load-more" title="Load the next page in the current sort order" onclick="loadImages(true)">Load more</button></p>':'')}
async function loadImages(append=false){if(typeof searchByText==='function'&&document.getElementById('semantic-search').checked&&document.getElementById('query').value.trim()&&!collectionFilter){if(append&&referenceVector)return loadReferenceImages(true);return searchByText();}if(typeof referenceVector!=='undefined'&&referenceVector){if(collectionFilter){referenceVector=null;referenceFileName='';}else return loadReferenceImages(append);}activeView='images';let generation=append?imageGeneration:++imageGeneration;sortToolbar();let q=document.getElementById('query').value;let missing=document.getElementById('show-missing').checked;if(!append)imageRequestParams={q,include_missing:missing,sort:sortField,descending:sortDescending,secondary:secondaryField,secondary_descending:secondaryDescending,group_by:groupField,limit:imagePageSize,...(collectionFilter?{collection_id:collectionFilter}:{})};let params=new URLSearchParams({...imageRequestParams,offset:append?imageRows.length:0});let more=document.getElementById('load-more');if(more)more.disabled=true;try{let data=await get('/api/images?'+params);if(generation!==imageGeneration||activeView!=='images')return;imageRows=append?imageRows.concat(data):data;hasMore=data.length===imagePageSize;renderImages()}catch(e){if(generation===imageGeneration&&activeView==='images')main.innerHTML='<p role="alert">'+escapeHtml(e.message)+'</p>';if(more)more.disabled=false}}
let groupRows=[];
async function loadGroups(append=false){activeView='people';let generation=++imageGeneration;document.getElementById('sort-tools').innerHTML='';if(!append)groupRows=[];let [data,people]=await Promise.all([get('/api/groups?limit=100&offset='+groupRows.length),get('/api/people')]);if(generation!==imageGeneration||activeView!=='people')return;groupRows=append?groupRows.concat(data):data;main.innerHTML='<h2>Face groups</h2><p class="hint">Review each group before applying a name. New matches remain suggestions.</p><div class="grid">'+groupRows.map(g=>
'<div class="card group" title="Review the face samples, then select a person ID or ignore the identity"><b>Group #'+g.group_id+' · '+g.face_count+' faces</b><p>'+escapeHtml(g.person||('Suggested: '+(g.suggested_person||'unassigned')))+'</p><div class="faces">'+g.face_ids.map(id=>'<img title="Face sample #'+id+'" loading="lazy" src="/api/faces/'+id+'/thumb">').join('')+'</div><p><select title="Reuse an existing stable person ID or create a new name" id="person-'+g.group_id+'"><option value="">New person</option>'+people.map(p=>'<option value="'+p.id+'"'+(p.id===(g.person_id||g.suggested_person_id)?' selected':'')+'>'+escapeHtml(p.name)+' (#'+p.id+')</option>').join('')+'</select><input id="name-'+g.group_id+'" placeholder="Person name"><button onclick="tag('+g.group_id+')" title="Assign all current faces in this sample set">Save person</button><button onclick="ignoreGroup('+g.group_id+')" title="Ignore this identity and future confident matches, retaining recognition samples">Ignore person</button></p></div>').join('')+'</div>'+(data.length===100?'<p><button title="Load the next 100 reviewable groups" onclick="loadGroups(true)">Load more face groups</button></p>':'')}
async function tag(id){let person_id=Number(document.getElementById('person-'+id).value);let name=document.getElementById('name-'+id).value.trim();if(!person_id&&!name)return;let r=await fetch('/api/groups/'+id+'/tag',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(person_id?{person_id}:{name})});if(!r.ok){alert(await responseError(r));return}loadGroups()}
async function ignoreGroup(id){let r=await fetch('/api/groups/'+id+'/ignore',{method:'POST'});if(!r.ok){alert(await responseError(r));return}loadGroups()}
async function loadProposals(){activeView='proposals';let generation=++imageGeneration;document.getElementById('sort-tools').innerHTML='';let data=await get('/api/proposals');if(generation!==imageGeneration||activeView!=='proposals')return;main.innerHTML='<h2>Sort proposals</h2><p class="hint">Proposals never move original files.</p>'+data.map(p=>
'<div class="card" title="Review only: approving never moves originals"><b>'+escapeHtml(p.status)+'</b><small>'+escapeHtml(p.source)+' → '+escapeHtml(p.destination)+'</small><p>'+escapeHtml(p.reason)+'</p><button data-decision="approved" onclick="reviewProposal('+p.id+',this.dataset.decision)" title="Record approval; keep the source file in place">Approve</button> <button data-decision="rejected" onclick="reviewProposal('+p.id+',this.dataset.decision)" title="Record rejection of this proposed destination">Reject</button></div>').join('')}
async function reviewProposal(id,decision){let r=await fetch('/api/proposals/'+id+'/review',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({decision})});if(!r.ok){alert(await responseError(r));return}loadProposals()}
loadImages().catch(e=>main.textContent=e.message);
</script></body></html>"""


@app.get("/", response_class=HTMLResponse)
def home():
    import json
    from .catalogs import registry
    initial_catalog = getattr(app.state, 'initial_catalog_id', None) or registry()['default']
    bootstrap = '<script>const CATALOG_DEFAULT=' + json.dumps(initial_catalog) + ';</script><script src="/catalogs.js"></script>'
    return HTML.replace('__IMAGEATLAS_VERSION__', __version__).replace('<script>', bootstrap + '<script>', 1).replace('src="/api/', 'src="\'+catalogPrefix+\'/').replace('</nav>', '<button title="Browse and edit events, trips, and places" onclick="loadCollections()">Collections</button><select id="catalog-selector" title="Catalog for this tab"></select><button title="Create, register, switch, inspect, back up, and restore databases" onclick="manageCatalogs()">Databases</button></nav>').replace(
        'loadImages().catch', "groupOptions.push(['Event','Event'],['Trip','Trip'],['Place','Place']);for(let k of ['Event','Trip','Place'])groupHints[k]='Group by saved '+k.toLowerCase()+' collections; an image appears once.';loadImages().catch").replace(
        '</body>', '<script src="/collections.js"></script><script src="/image-search.js"></script></body>')


@app.get('/catalogs.js')
def catalogs_script():
    return Response(Path(__file__).with_name('web_catalogs.js').read_text(encoding='utf-8'), media_type='text/javascript')


@app.get('/logo.png')
def logo_image():
    return Response((Path(__file__).with_name('branding') / 'image-atlas.png').read_bytes(),
                    media_type='image/png')


@app.get('/icon.ico')
def application_icon():
    return Response((Path(__file__).with_name('branding') / 'image-atlas.ico').read_bytes(),
                    media_type='image/vnd.microsoft.icon')


@app.get('/api/check')
def catalog_check():
    from .catalogs import resolve
    catalog = resolve()
    with connect() as conn:
        result = conn.execute("SELECT current_database() AS database,version() AS server_version,pg_database_size(current_database()) AS bytes,(SELECT count(*) FROM images) AS images,(SELECT count(*) FROM faces) AS faces,(SELECT count(*) FROM people) AS people,(SELECT count(*) FROM collections) AS collections").fetchone()
    return {**result, 'catalog_id': catalog.id, 'name': catalog.name, 'readonly': catalog.readonly, 'roots': catalog.roots}


@app.get('/api/jobs')
def catalog_jobs():
    from .catalogs import resolve
    from .jobs import active_jobs
    return [j for j in active_jobs() if j.get('catalog_id') == resolve().id]


@app.post('/api/cancel/{job_id}')
def catalog_cancel(job_id: str):
    from .jobs import cancel_job
    try:
        return {'id': cancel_job(job_id)}
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc


@app.get("/api/images")
def images(q: str = "", include_missing: bool = False, limit: int = Query(100, ge=1, le=500),
           offset: int = Query(0, ge=0), sort: str = "DateIndexed", descending: bool = True,
           secondary: str = "Filename", secondary_descending: bool = False, group_by: str = "None",
           collection_id: int | None = None):
    if sort not in SORT_FIELDS or secondary not in SORT_FIELDS or group_by not in GROUP_FIELDS:
        raise HTTPException(400, "Unknown sort or grouping field")
    primary = SORT_FIELDS[sort][1]
    tie = SORT_FIELDS[secondary][1]
    group = GROUP_FIELDS[group_by]
    order = f"({group}) ASC,({primary}) {'DESC' if descending else 'ASC'} NULLS LAST,({tie}) {'DESC' if secondary_descending else 'ASC'} NULLS LAST,i.id DESC"
    with connect() as conn:
        return conn.execute(oriented_sql(f"""SELECT i.id,i.path,i.width AS width,i.height AS height,i.missing,i.size_bytes,
                                i.capture_day,i.camera_model,i.format,i.rating,i.review_state,
                                ({group}) AS image_group,
                                (SELECT string_agg(coalesce(c.user_name,c.suggested_name), ', ' ORDER BY c.id)
                                 FROM collection_images m JOIN collections c ON c.id=m.collection_id
                                 WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed') AS collections FROM images i
                                WHERE (%s OR NOT i.missing) AND (%s='' OR i.path ILIKE %s OR EXISTS(
                                    SELECT 1 FROM collection_images m JOIN collections c ON c.id=m.collection_id
                                    WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed'
                                    AND coalesce(c.user_name,c.suggested_name) ILIKE %s))
                                AND (%s::bigint IS NULL OR EXISTS(SELECT 1 FROM collection_images m JOIN collections c ON c.id=m.collection_id
                                     WHERE m.image_id=i.id AND m.collection_id=%s AND m.source<>'excluded' AND c.status<>'dismissed'))
                                ORDER BY {order} LIMIT %s OFFSET %s"""),
                            (include_missing, q.strip(), f"%{q.strip()}%", f"%{q.strip()}%", collection_id, collection_id, limit, offset)).fetchall()


@app.get('/collections.js')
def collections_script():
    script = Path(__file__).with_name('web_collections.js').read_text(encoding='utf-8')
    return Response(script.replace('src="/api/', 'src="\'+catalogPrefix+\'/'), media_type='application/javascript')


@app.post('/api/search/image')
async def search_reference(request: Request, include_missing: bool = False, model: str = 'openclip'):
    import tempfile
    from starlette.concurrency import run_in_threadpool
    from .image_query import MAX_BYTES, encode_reference, search
    size = 0
    with tempfile.TemporaryDirectory(prefix='imageatlas-reference-') as folder:
        path = Path(folder) / 'reference.img'
        with path.open('wb') as output:
            async for chunk in request.stream():
                size += len(chunk)
                if size > MAX_BYTES:
                    raise HTTPException(413, 'Reference images must be 32 MiB or smaller')
                output.write(chunk)
        def query():
            with IMAGE_SEARCH_LOCK:
                vector = encode_reference(path, model=model)
            with connect() as conn:
                rows = search(conn, vector, include_missing=include_missing, model=model)
            return {'items': rows, 'vector': vector, 'model': model}
        try:
            return await run_in_threadpool(query)
        except (OSError, ValueError, RuntimeError) as exc:
            raise HTTPException(400, str(exc)) from exc


@app.get('/api/search/text')
def search_content(q: str = Query(..., min_length=1, max_length=1000), model: str = 'openclip', include_missing: bool = False):
    from .embeddings import create_embedder
    from .image_query import search, validate_vector
    try:
        with IMAGE_SEARCH_LOCK:
            encoder = create_embedder(model=model)
            try:
                vector = validate_vector(encoder.text(q))
            finally:
                encoder.close()
        with connect() as conn:
            rows = search(conn, vector, include_missing=include_missing, model=model)
        return {'items': rows, 'vector': vector, 'model': model}
    except (OSError, ValueError, RuntimeError) as exc:
        raise HTTPException(400, str(exc)) from exc


class ReferencePage(BaseModel):
    vector: list[float]
    model: str = 'openclip'
    offset: int = 0
    include_missing: bool = False


@app.post('/api/search/image/page')
def search_reference_page(request: ReferencePage):
    from .image_query import search
    try:
        with connect() as conn:
            return search(conn, request.vector, offset=request.offset, include_missing=request.include_missing, model=request.model)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc


@app.get('/image-search.js')
def image_search_script():
    return Response(Path(__file__).with_name('web_image_search.js').read_text(encoding='utf-8'),
                    media_type='application/javascript')


class CollectionReview(BaseModel):
    action: str
    name: str | None = None
    image_ids: list[int] = []
    other_id: int | None = None


@app.get('/api/collections')
def collections_list(kind: str | None = None, include_dismissed: bool = False,
                     limit: int = Query(100, ge=1, le=500), offset: int = Query(0, ge=0)):
    from .collections import listing
    try:
        with connect() as conn:
            return listing(conn, kind, include_dismissed, limit, offset)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc


@app.post('/api/collections/detect')
def collections_detect(root: str | None = None):
    from .collections import detect
    from .jobs import MineJob, MiningCancelled
    from .catalogs import resolve, current
    catalog = resolve()
    with COLLECTION_JOB_LOCK:
        if any(s['status'] == 'running' and s.get('catalog_id') == catalog.id for s in COLLECTION_JOBS.values()):
            raise HTTPException(409, 'Detection is already running')
        while len(COLLECTION_JOBS) >= 64:
            del COLLECTION_JOBS[next(iter(COLLECTION_JOBS))]
        job = MineJob('collections:' + (root or 'catalog'))
        state = {'id': job.id, 'catalog_id': catalog.id, 'status': 'running', 'images_examined': 0}
        COLLECTION_JOBS[job.id] = state
    def work():
        current.set(catalog)
        try:
            with connect() as conn:
                result = detect(conn, root, check=job.check, progress=lambda n: state.update(images_examined=n))
            state.update(status='done', result=result)
        except MiningCancelled:
            state.update(status='cancelled')
        except Exception as exc:
            state.update(status='error', error=str(exc))
        finally:
            job.close()
    threading.Thread(target=work, daemon=True).start()
    return state


@app.get('/api/collections/jobs/{job_id}')
def collections_job(job_id: str):
    from .catalogs import resolve
    if job_id not in COLLECTION_JOBS or COLLECTION_JOBS[job_id].get('catalog_id') != resolve().id:
        raise HTTPException(404, 'Detection job not found')
    return COLLECTION_JOBS[job_id]


@app.post('/api/collections/jobs/{job_id}/cancel')
def collections_cancel(job_id: str):
    from .jobs import cancel_job
    try:
        return {'id': cancel_job(job_id)}
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc


@app.get('/api/collections/{collection_id}')
def collection_detail(collection_id: int, limit: int = Query(100, ge=1, le=500), offset: int = Query(0, ge=0)):
    from .collections import detail
    try:
        with connect() as conn:
            return detail(conn, collection_id, limit=limit, offset=offset)
    except ValueError as exc:
        raise HTTPException(404, str(exc)) from exc


@app.post('/api/collections/{collection_id}/review')
def collection_review(collection_id: int, request: CollectionReview):
    from .collections import review
    try:
        with connect() as conn:
            return review(conn, collection_id, request.action, name=request.name,
                          image_ids=request.image_ids, other_id=request.other_id, detail_limit=100)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc


@app.post('/api/collections/{collection_id}/lookup')
def collection_lookup(collection_id: int):
    from .collections import lookup_place
    try:
        with connect() as conn:
            return lookup_place(conn, collection_id)
    except (ValueError, OSError) as exc:
        raise HTTPException(400, str(exc)) from exc



def _image_bytes(path: Path, box=None):
    try:
        from .catalogs import resolve
        catalog = resolve()
        catalog.require_path(path)
        with Image.open(path) as source:
            image = ImageOps.exif_transpose(source).convert("RGB")
            if box:
                x, y, w, h = box
                image = image.crop((x, y, x+w, y+h))
            image.thumbnail((400, 400), Image.Resampling.LANCZOS)
            output = io.BytesIO()
            image.save(output, "JPEG", quality=80)
            return output.getvalue()
    except (OSError, ValueError) as exc:
        raise HTTPException(404, str(exc)) from exc


@app.get("/api/images/{image_id}/thumb")
def image_thumb(image_id: int):
    with connect() as conn:
        row = conn.execute("""SELECT i.path,t.data,t.mime_type FROM images i
                              LEFT JOIN thumbnails t ON t.sha256=i.sha256 AND t.version=%s
                              WHERE i.id=%s""", (THUMBNAIL_VERSION, image_id)).fetchone()
    if not row:
        raise HTTPException(404)
    if row["data"] is not None:
        return Response(bytes(row["data"]), media_type=row["mime_type"],
                        headers={"Cache-Control": "private, max-age=3600"})
    return Response(_image_bytes(Path(row["path"])), media_type="image/jpeg",
                    headers={"Cache-Control": "private, max-age=3600"})


@app.get("/api/faces/{face_id}/thumb")
def face_thumb(face_id: int):
    with connect() as conn:
        row = conn.execute("""SELECT i.path,f.x,f.y,f.width,f.height FROM faces f
                              JOIN images i ON i.id=f.image_id WHERE f.id=%s AND i.missing=false""", (face_id,)).fetchone()
    if not row:
        raise HTTPException(404)
    box = (row["x"], row["y"], row["width"], row["height"])
    return Response(_image_bytes(Path(row["path"]), box), media_type="image/jpeg")


@app.get("/api/groups")
def groups(limit: int = Query(100, ge=1, le=500), offset: int = Query(0, ge=0)):
    with connect() as conn:
        rows = conn.execute("""SELECT f.group_id,count(*) AS face_count,
                              (array_agg(f.id ORDER BY f.id))[1:8] AS face_ids,
                              max(p.name) AS person,max(p.id) AS person_id,
                              max(sp.name) AS suggested_person,max(sp.id) AS suggested_person_id
                              FROM faces f JOIN images i ON i.id=f.image_id
                              LEFT JOIN people p ON p.id=f.person_id
                              LEFT JOIN people sp ON sp.id=f.suggested_person_id
                              WHERE i.missing=false AND NOT coalesce(p.ignored,false)
                              AND NOT coalesce(sp.ignored,false) GROUP BY f.group_id
                              ORDER BY face_count DESC,f.group_id LIMIT %s OFFSET %s""", (limit, offset)).fetchall()
    return rows


class TagRequest(BaseModel):
    name: str | None = None
    person_id: int | None = None


@app.post("/api/groups/{group_id}/tag")
def tag(group_id: int, request: TagRequest):
    with connect() as conn:
        try:
            return name_group(conn, group_id, request.name, person_id=request.person_id)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc


@app.get("/api/people")
def people():
    with connect() as conn:
        return conn.execute("SELECT id,name FROM people WHERE NOT ignored ORDER BY lower(name),id").fetchall()


@app.post("/api/groups/{group_id}/ignore")
def ignore_group(group_id: int):
    with connect() as conn:
        try:
            return name_group(conn, group_id, ignore=True)
        except ValueError as exc:
            raise HTTPException(400, str(exc)) from exc


@app.get("/api/proposals")
def proposals(limit: int = Query(100, ge=1, le=500)):
    with connect() as conn:
        return conn.execute("""SELECT p.id,i.path AS source,p.destination,p.reason,p.status,p.created_at
                               FROM sort_proposals p JOIN images i ON i.id=p.image_id
                               ORDER BY p.created_at DESC LIMIT %s""", (limit,)).fetchall()


class ReviewRequest(BaseModel):
    decision: str


@app.post("/api/proposals/{proposal_id}/review")
def review_proposal(proposal_id: int, request: ReviewRequest):
    if request.decision not in {"approved", "rejected"}:
        raise HTTPException(400, "Decision must be approved or rejected")
    with connect() as conn:
        row = conn.execute("UPDATE sort_proposals SET status=%s WHERE id=%s RETURNING id,status",
                           (request.decision, proposal_id)).fetchone()
        if not row:
            raise HTTPException(404)
        conn.commit()
        return row
