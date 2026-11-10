const originalFetch = window.fetch.bind(window);
let catalogId = sessionStorage.getItem('image-atlas-catalog') || CATALOG_DEFAULT;
let catalogPrefix = catalogId ? '/api/catalogs/' + catalogId : '';
window.fetch = (url, options) => originalFetch(typeof url === 'string' && url.startsWith('/api/') && !url.startsWith('/api/catalogs') && catalogId ? catalogPrefix + url.slice(4) : url, options);
function readable(value) {
  if(value===null||value===undefined)return '—';
  if(Array.isArray(value))return value.map(readable).join('\n\n');
  if(typeof value==='object')return Object.entries(value).map(([key,item])=>key.replaceAll('_',' ')+': '+readable(item)).join('\n');
  return String(value);
}
async function responseError(response){
  const text=await response.text();try{const value=JSON.parse(text);return readable(value.detail??value);}catch{return text;}
}
async function refreshCatalogSelector() {
  const response=await originalFetch('/api/catalogs');if(!response.ok)throw Error('Gallery session unavailable; reopen the session link');
  const data=await response.json();const selector=document.getElementById('catalog-selector');
  selector.replaceChildren(...data.catalogs.map(c=>new Option(c.name+(c.readonly?' (read-only)':''),c.id)));
  if(!data.catalogs.some(c=>c.id===catalogId)&&data.catalogs.length){sessionStorage.setItem('image-atlas-catalog',data.default||data.catalogs[0].id);location.reload();return;}
  selector.value=catalogId||'';selector.onchange=()=>{sessionStorage.setItem('image-atlas-catalog',selector.value);location.reload();};return data;
}
async function manageCatalogs() {
  activeView='catalogs';++imageGeneration;document.getElementById('sort-tools').replaceChildren();
  const data=await refreshCatalogSelector();const selected=data.catalogs.find(c=>c.id===catalogId);
  const panel=document.createElement('section');panel.className='card';
  panel.innerHTML='<h2>Databases</h2><p>Each database owns separate folders and identities.</p>'+
    '<p><label><input id="catalog-new" type="checkbox"> Create/register another database</label></p>'+
    '<p><label>Name<br><input id="catalog-name" title="Friendly name for a new database"></label></p>'+
    '<p><label>Registration connection (optional)<br><input id="catalog-dsn" type="password" autocomplete="off" title="Password saved in Windows Credential Manager"></label></p>'+
    '<p><label>Allowed folders, one per line<br><textarea id="catalog-roots" rows="5" style="width:90%" title="Current owned folders; cannot overlap another database"></textarea></label></p>'+
    '<p><label>Backup folder or restore archive<br><input id="catalog-path" title="Existing folder for backup or .dump archive for restore"></label></p>'+
    '<p><label>Job ID<br><input id="catalog-job" title="Select a job below or enter its ID"></label><select id="catalog-jobs" title="Select an active job"><option value="">Choose a job</option></select></p>'+
    '<div id="catalog-actions" class="toolbar"></div><pre id="catalog-result" role="status"></pre>';
  main.replaceChildren(panel);
  const roots=document.getElementById('catalog-roots');let existingRoots=(selected?.roots||[]).join('\n'),newRoots='';
  roots.value=existingRoots;
  document.getElementById('catalog-new').onchange=e=>{if(e.target.checked){existingRoots=roots.value;roots.value=newRoots;}else{newRoots=roots.value;roots.value=existingRoots;}updateActions();};
  document.getElementById('catalog-jobs').onchange=e=>document.getElementById('catalog-job').value=e.target.value;
  const definitions=[['create','Create'],['register','Register existing'],['roots','Save folders'],['default','Set CLI default'],['check','Health and storage'],['backup','Back up'],['restore','Restore as new'],['jobs','Jobs'],['cancel','Cancel job']];
  function updateActions(){for(const button of document.querySelectorAll('#catalog-actions button'))button.disabled=['create','register'].includes(button.dataset.action)?!document.getElementById('catalog-new').checked:button.dataset.action==='roots'&&document.getElementById('catalog-new').checked;}
  function showJobs(jobs){const selector=document.getElementById('catalog-jobs');selector.replaceChildren(new Option('Choose a job',''),...jobs.map(j=>new Option(j.id+' · '+(j.source||'')+' · '+(j.stage||''),j.id)));}
  for(const [action,label] of definitions){
    const button=document.createElement('button');button.textContent=label;button.dataset.action=action;
    button.title={create:'Create a separate database using the new folders',register:'Register a database with its own folders',roots:'Replace current allowed folders',default:'Use this database for future CLI commands',check:'Inspect identity, counts and storage',backup:'Save a consistent archive without original files',restore:'Restore into a separate read-only database',jobs:'List active jobs and select one',cancel:'Cancel the job in the Job ID field'}[action];
    button.onclick=async()=>{
      const output=document.getElementById('catalog-result');output.textContent='Working…';const pinnedId=catalogId;let polling;
      try{
        if(['backup','restore'].includes(action))polling=setInterval(async()=>{try{const jobs=await (await originalFetch('/api/catalogs/'+pinnedId+'/jobs')).json();showJobs(jobs);output.textContent=readable(jobs);}catch(error){output.textContent=error.message;}},1000);
        let response;
        if(['check','jobs'].includes(action))response=await originalFetch('/api/catalogs/'+pinnedId+'/'+action);
        else if(action==='cancel')response=await originalFetch('/api/catalogs/'+pinnedId+'/cancel/'+encodeURIComponent(document.getElementById('catalog-job').value),{method:'POST'});
        else response=await originalFetch('/api/catalogs',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action,catalog_id:pinnedId,name:document.getElementById('catalog-name').value,dsn:document.getElementById('catalog-dsn').value||null,roots:roots.value.split('\n').map(s=>s.trim()).filter(Boolean),path:document.getElementById('catalog-path').value})});
        const result=await response.json();if(!response.ok)throw Error(result.detail||'Operation failed');output.textContent=readable(result);if(action==='jobs')showJobs(result);await refreshCatalogSelector();document.getElementById('catalog-dsn').value='';
      }catch(error){output.textContent=error.message;}finally{if(polling)clearInterval(polling);}
    };document.getElementById('catalog-actions').append(button);
  }updateActions();
}
refreshCatalogSelector().catch(error=>main.textContent=error.message);
