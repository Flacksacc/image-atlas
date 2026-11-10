let referenceVector=null,referenceFileName='';
function referenceTools(){document.getElementById('sort-tools').innerHTML='<p>Images like '+escapeHtml(referenceFileName)+' · closest matches first <button onclick="clearReferenceSearch()">Clear image search</button></p>';}
function clearReferenceSearch(){referenceVector=null;referenceFileName='';++imageGeneration;loadImages();}
async function searchByImage(file){
  if(!file)return;
  if(file.size>32*1024*1024){alert('Reference images must be 32 MiB or smaller');return;}
  activeView='images';let generation=++imageGeneration;
  document.getElementById('query').value='';
  referenceVector=null;referenceFileName=file.name;groupField='None';collectionFilter=null;
  main.textContent='Finding similar images…';referenceTools();
  try{
    const response=await fetch('/api/search/image?include_missing='+document.getElementById('show-missing').checked,{method:'POST',headers:{'Content-Type':file.type||'application/octet-stream'},body:file});
    if(!response.ok)throw Error(await responseError(response));
    const data=await response.json();
    if(generation!==imageGeneration||activeView!=='images')return;
    referenceVector=data.vector;imageRows=data.items;hasMore=imageRows.length===imagePageSize;
    renderImages();referenceTools();
    if(!imageRows.length)main.insertAdjacentHTML('afterbegin','<p>No images with compatible visual embeddings were found. Run mining with embeddings enabled first.</p>');
  }catch(error){if(generation===imageGeneration&&activeView==='images')main.textContent=error.message;}
}
async function loadReferenceImages(append=false){
  activeView='images';const generation=append?imageGeneration:++imageGeneration;
  const more=document.getElementById('load-more');if(more)more.disabled=true;
  try{
    const response=await fetch('/api/search/image/page',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({vector:referenceVector,offset:append?imageRows.length:0,include_missing:document.getElementById('show-missing').checked})});
    if(!response.ok)throw Error(await responseError(response));
    const rows=await response.json();if(generation!==imageGeneration||activeView!=='images')return;
    imageRows=append?imageRows.concat(rows):rows;hasMore=rows.length===imagePageSize;renderImages();referenceTools();
  }catch(error){if(generation===imageGeneration&&activeView==='images')main.textContent=error.message;}
  finally{if(more)more.disabled=false;}
}
document.getElementById('image-query').addEventListener('change',event=>{searchByImage(event.target.files[0]);event.target.value='';});
document.addEventListener('dragover',event=>{if(event.dataTransfer.types.includes('Files'))event.preventDefault();});
document.addEventListener('drop',event=>{if(!event.dataTransfer.files.length)return;event.preventDefault();if(event.dataTransfer.files.length!==1){alert('Drop one reference image at a time');return;}searchByImage(event.dataTransfer.files[0]);});
document.getElementById('query').addEventListener('input',()=>{referenceVector=null;referenceFileName='';++imageGeneration;});
