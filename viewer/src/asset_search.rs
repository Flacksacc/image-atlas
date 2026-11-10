//! One bounded background worker: queued searches collapse to the newest request.
use super::{Asset, Pack};
use eframe::egui;
use postgres::{Client, NoTls};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::Duration;

#[derive(Clone, Default)]
pub(super) struct Request {
    pub generation:u64, pub catalog_version:u64, pub query:String, pub pack:String,
    pub classes:Vec<String>, pub indexed_only:bool, pub local_only:bool, pub offset:i64,
}
pub(super) struct Results {
    pub packs:Arc<Vec<Pack>>, pub classes:Arc<Vec<String>>, pub assets:Arc<Vec<Asset>>,
    pub total:i64, pub offset:i64, pub files:Vec<(String,String,i64)>,
}
pub(super) struct Reply {pub generation:u64, pub result:Result<Results,String>}
pub(super) struct Worker {
    requests:Sender<Request>, queued:Receiver<Request>, pub replies:Receiver<Reply>,
    latest:Arc<AtomicU64>,
}
impl Worker {
    pub fn start(dsn:String, ctx:egui::Context)->Self {
        let mut catalog:Option<(u64,Arc<Vec<Pack>>,Arc<Vec<String>>,super::asset_local::LocalFiles)>=None;
        Self::with_loader(ctx,move |request,latest| {
            let mut config=dsn.parse::<postgres::Config>().map_err(|e|e.to_string())?;
            config.connect_timeout(Duration::from_secs(5));
            let mut db=config.connect(NoTls).map_err(|e|e.to_string())?;
            db.batch_execute("SET statement_timeout='120s'").map_err(|e|e.to_string())?;
            if catalog.as_ref().map(|c|c.0)!=Some(request.catalog_version) {
                let local=super::asset_local::load(&mut db)?;
                let packs=db.query("SELECT p.id,p.title,p.source,p.scan_state,p.description,p.metadata::text,count(a.id),p.listing_url FROM packs p LEFT JOIN assets a ON a.pack_id=p.id GROUP BY p.id ORDER BY p.title",&[])
                    .map_err(|e|e.to_string())?.iter().map(|r|Pack{id:r.get(0),title:r.get(1),source:r.get(2),state:r.get(3),description:r.get(4),metadata:r.get(5),count:r.get(6),listing_url:r.get(7),local:local.packs.contains(&r.get::<_,String>(0))}).collect();
                let classes=db.query("SELECT DISTINCT class_name FROM assets WHERE class_name<>'' ORDER BY class_name",&[])
                    .map_err(|e|e.to_string())?.iter().map(|r|r.get(0)).collect();
                catalog=Some((request.catalog_version,Arc::new(packs),Arc::new(classes),local));
            }
            if latest.load(Ordering::Relaxed)!=request.generation {return Err("Superseded".into());}
            let (_,packs,classes,local)=catalog.as_ref().unwrap();
            load(&mut db,request,packs.clone(),classes.clone(),local,latest)
        })
    }
    pub(super) fn with_loader(mut_ctx:egui::Context,mut load:impl FnMut(&Request,&AtomicU64)->Result<Results,String>+Send+'static)->Self {
        let (requests,queued)=bounded::<Request>(1);
        let worker_queue=queued.clone();
        let (sender,replies)=bounded(1);
        let old_reply=replies.clone();
        let latest=Arc::new(AtomicU64::new(0));
        let current=latest.clone();
        std::thread::Builder::new().name("asset-search".into()).spawn(move || {
            while let Ok(request)=worker_queue.recv() {
                if current.load(Ordering::Relaxed)!=request.generation {continue;}
                let result=load(&request,&current);
                if current.load(Ordering::Relaxed)!=request.generation {continue;}
                let mut reply=Reply{generation:request.generation,result};
                loop {match sender.try_send(reply) {
                    Ok(())=>break,
                    Err(TrySendError::Full(value))=>{reply=value;let _=old_reply.try_recv();},
                    Err(TrySendError::Disconnected(_))=>return,
                }}
                mut_ctx.request_repaint();
            }
        }).expect("start asset search worker");
        Self{requests,queued,replies,latest}
    }
    pub fn invalidate(&self,generation:u64) {self.latest.store(generation,Ordering::Relaxed);}
    pub fn submit(&self,mut request:Request) {
        self.invalidate(request.generation);
        loop {match self.requests.try_send(request) {
            Ok(())=>break,
            Err(TrySendError::Full(value))=>{request=value;let _=self.queued.try_recv();},
            Err(TrySendError::Disconnected(_))=>break,
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_search_never_blocks_submit_and_only_newest_queued_job_runs() {
        let (started,starts)=bounded(4);
        let (release,releases)=bounded(1);
        let worker=Worker::with_loader(egui::Context::default(),move |r,_| {
            started.send((r.generation,std::thread::current().id())).unwrap();
            if r.generation==1 {releases.recv().unwrap();}
            Err(format!("result {}",r.generation))
        });
        worker.submit(Request{generation:1,..Default::default()});
        let (generation,thread)=starts.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(generation,1);
        assert_ne!(thread,std::thread::current().id());
        for generation in 2..=100 {worker.submit(Request{generation,..Default::default()});}
        assert!(worker.replies.is_empty());
        assert_eq!(worker.queued.len(),1);
        release.send(()).unwrap();
        assert_eq!(starts.recv_timeout(Duration::from_secs(2)).unwrap().0,100);
        let reply=worker.replies.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(reply.generation,100);
        assert_eq!(reply.result.err().unwrap(),"result 100");
        assert!(starts.is_empty());
    }
}

pub(super) fn load(db:&mut Client,r:&Request,packs:Arc<Vec<Pack>>,classes:Arc<Vec<String>>,local:&super::asset_local::LocalFiles,latest:&AtomicU64)->Result<Results,String> {
    let pattern=format!("%{}%",r.query);
    // Exact OR matching across checked types; an empty array means all types.
    let filter=" FROM assets a JOIN packs p ON p.id=a.pack_id WHERE ($1='' OR a.search_text ILIKE $4 OR p.title ILIKE $4) AND ($2='' OR a.pack_id=$2) AND (cardinality($3::text[])=0 OR a.class_name=ANY($3)) AND (NOT $5::boolean OR a.scan_status='indexed') AND (NOT $6::boolean OR a.id=ANY($7::bigint[]))";
    let total:i64=db.query_one(&format!("SELECT count(*){filter}"),&[&r.query,&r.pack,&r.classes,&pattern,&r.indexed_only,&r.local_only,&local.ids]).map_err(|e|e.to_string())?.get(0);
    if latest.load(Ordering::Relaxed)!=r.generation {return Err("Superseded".into());}
    let offset=if total==0{0}else{r.offset.min(((total-1)/200)*200).max(0)};
    let sql=format!("SELECT a.id,a.pack_id,a.name,a.object_path,a.class_name,a.kind,a.scan_status,a.size_bytes,a.thumbnail_source,a.metadata::text,a.thumbnail,a.scan_error{filter} ORDER BY p.title,a.name,a.id LIMIT 200 OFFSET $8");
    let rows=db.query(&sql,&[&r.query,&r.pack,&r.classes,&pattern,&r.indexed_only,&r.local_only,&local.ids,&offset]).map_err(|e|e.to_string())?;
    let local_ids:std::collections::HashSet<i64>=local.ids.iter().copied().collect();
    let mut assets=Vec::with_capacity(rows.len());
    for row in rows {
        if latest.load(Ordering::Relaxed)!=r.generation {return Err("Superseded".into());}
        let bytes:Option<Vec<u8>>=row.get(10);
        let thumbnail=bytes.and_then(|b|image::load_from_memory(&b).ok()).map(|img| {
            let decoded=img.thumbnail(320,320).to_rgba8();
            Arc::new(egui::ColorImage::from_rgba_unmultiplied([decoded.width() as usize,decoded.height() as usize],decoded.as_raw()))
        });
        assets.push(Asset{id:row.get(0),pack_id:row.get(1),name:row.get(2),path:row.get(3),class_name:row.get(4),kind:row.get(5),status:row.get(6),size_bytes:row.get::<_,Option<i64>>(7).unwrap_or(0),thumbnail_source:row.get(8),metadata:row.get(9),thumbnail,scan_error:row.get(11),local:local_ids.contains(&row.get(0))});
    }
    let files=if r.pack.is_empty(){Vec::new()}else{
        db.query("SELECT path,role,size_bytes FROM pack_files WHERE pack_id=$1 ORDER BY path LIMIT 500",&[&r.pack]).map_err(|e|e.to_string())?.iter().map(|r|(r.get(0),r.get(1),r.get::<_,Option<i64>>(2).unwrap_or(0))).collect()
    };
    Ok(Results{packs,classes,assets:Arc::new(assets),total,offset,files})
}
