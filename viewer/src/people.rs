use crate::db::{Event, FaceSample, Person, Request};
use crossbeam_channel::Sender;
use postgres::Client;

pub fn query(client:&mut Client,request:Request,tx:&Sender<Event>)->anyhow::Result<()> {
    match request {
        Request::PeopleAll=>{
            let people=client.query("SELECT id,name,ignored FROM people ORDER BY ignored,lower(name),id",&[])?.into_iter()
                .map(|r|Person{id:r.get(0),name:r.get(1),ignored:r.get(2)}).collect();
            let _=tx.send(Event::People(people));
        },
        Request::GroupFaces{group,offset}=>{
            let samples=client.query("SELECT f.id,f.image_id,i.path,f.x,f.y,f.width,f.height,i.sha256 FROM faces f JOIN images i ON i.id=f.image_id WHERE f.group_id=$1 ORDER BY f.id LIMIT 100 OFFSET $2",&[&group,&offset])?.into_iter()
                .map(|r|FaceSample{face_id:r.get(0),image_id:r.get(1),path:r.get(2),sha256:r.get("sha256"),box_xywh:(r.get(3),r.get(4),r.get(5),r.get(6))}).collect();
            let _=tx.send(Event::GroupFaces{group,offset,samples});
        },
        Request::PersonRename{id,name}=>{
            let name=name.trim();anyhow::ensure!(!name.is_empty()&&name.chars().count()<=200,"Name must contain 1–200 characters");
            let mut transaction=client.transaction()?;
            crate::db::migrate_people_views(&mut transaction,None)?;
            anyhow::ensure!(transaction.execute("UPDATE people SET name=$1 WHERE id=$2",&[&name,&id])?==1,"Person no longer exists");
            transaction.commit()?;
        },
        Request::PersonIgnore{id,ignored}=>{
            anyhow::ensure!(client.execute("UPDATE people SET ignored=$1 WHERE id=$2",&[&ignored,&id])?==1,"Person no longer exists");
        },
        Request::PersonMerge{source,target}=>{
            anyhow::ensure!(source!=target,"Choose a different target person");
            let mut transaction=client.transaction()?;
            anyhow::ensure!(transaction.query("SELECT id FROM people WHERE id=ANY($1) FOR UPDATE",&[&vec![source,target]])?.len()==2,"Person no longer exists");
            crate::db::migrate_people_views(&mut transaction,Some((source,target)))?;
            transaction.execute("UPDATE faces SET person_id=$1 WHERE person_id=$2",&[&target,&source])?;
            transaction.execute("UPDATE faces SET suggested_person_id=$1 WHERE suggested_person_id=$2",&[&target,&source])?;
            transaction.execute("DELETE FROM people WHERE id=$1",&[&source])?;
            transaction.commit()?;
            let _=tx.send(Event::PersonMerged(source,target));
        },
        Request::FaceMove{ids,person_id}=>{
            anyhow::ensure!(!ids.is_empty(),"Select faces first");
            let mut transaction=client.transaction()?;
            if let Some(id)=person_id {anyhow::ensure!(transaction.query_opt("SELECT id FROM people WHERE id=$1 AND NOT ignored FOR UPDATE",&[&id])?.is_some(),"Person is missing or ignored");}
            let group:i64=transaction.query_one("INSERT INTO face_groups DEFAULT VALUES RETURNING id",&[])?.get(0);
            transaction.execute("UPDATE faces SET group_id=$1,person_id=$2,suggested_person_id=NULL WHERE id=ANY($3)",&[&group,&person_id,&ids])?;
            transaction.commit()?;
        },
        Request::FaceReject(ids)=>{
            anyhow::ensure!(!ids.is_empty(),"Select faces first");
            let mut transaction=client.transaction()?;
            let snapshot:String=transaction.query_one("SELECT coalesce(jsonb_agg(to_jsonb(f)),'[]'::jsonb)::text FROM faces f WHERE id=ANY($1)",&[&ids])?.get(0);
            transaction.execute("DELETE FROM faces WHERE id=ANY($1)",&[&ids])?;
            transaction.commit()?;
            let _=tx.send(Event::FaceUndoReady(snapshot));
        },
        Request::FaceRestore(snapshot)=>{
            let mut transaction=client.transaction()?;
            transaction.execute("INSERT INTO faces SELECT * FROM jsonb_populate_recordset(NULL::faces,$1::text::jsonb) ON CONFLICT DO NOTHING",&[&snapshot])?;
            transaction.commit()?;
            let _=tx.send(Event::FacesRestored);
        },
        _=>unreachable!(),
    }
    Ok(())
}
