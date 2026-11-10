use super::*;
use postgres::NoTls;

#[test] fn readonly_legacy_dimensions_normalize_without_writes()->anyhow::Result<()> {
    let Ok(dsn)=std::env::var("IMAGE_CLI_TEST_DSN") else{return Ok(());};
    anyhow::ensure!(dsn.ends_with("/imagecli_test"),"Dedicated database required");
    let mut client=Client::connect(&dsn,NoTls)?;let path=format!("legacy-dimensions-{}",std::process::id());
    let id:i64=client.query_one("INSERT INTO images(path,root_path,size_bytes,mtime_ns,width,height,metadata_version,exif) VALUES ($1,$1,1,1,200,100,4,'{\"Orientation\":6}') RETURNING id",&[&path])?.get(0);
    let result=(||->anyhow::Result<()>{
        client.batch_execute("SET default_transaction_read_only=on")?;
        let sql=explore::oriented_sql("SELECT i.width AS width,i.height AS height,CASE WHEN i.width>i.height THEN 'Landscape' ELSE 'Portrait' END AS orientation FROM images i WHERE i.id=$1");
        let row=client.query_one(&sql,&[&id])?;assert_eq!(row.get::<_,i32>("width"),100);assert_eq!(row.get::<_,i32>("height"),200);assert_eq!(row.get::<_,String>("orientation"),"Portrait");
        let row=client.query_one("SELECT width,height FROM images WHERE id=$1",&[&id])?;assert_eq!((row.get::<_,i32>(0),row.get::<_,i32>(1)),(200,100));Ok(())
    })();
    client.batch_execute("SET default_transaction_read_only=off")?;client.execute("DELETE FROM images WHERE id=$1",&[&id])?;result
}

#[test]
fn person_corrections_and_unicode_tags()->anyhow::Result<()> {
    let Ok(dsn)=std::env::var("IMAGE_CLI_TEST_DSN") else{return Ok(());};
    anyhow::ensure!(dsn.ends_with("/imagecli_test"),"Only the dedicated test database is allowed");
    let mut client=Client::connect(&dsn,NoTls)?;let root=format!("deep-qa-{}",std::process::id());
    let image:i64=client.query_one("INSERT INTO images(path,root_path,size_bytes,mtime_ns) VALUES ($1,$1,10,1) RETURNING id",&[&root])?.get(0);
    let a:i64=client.query_one("INSERT INTO people(name) VALUES ($1) RETURNING id",&[&format!("{root}-A")])?.get(0);
    let b:i64=client.query_one("INSERT INTO people(name) VALUES ($1) RETURNING id",&[&format!("{root}-B")])?.get(0);
    let group:i64=client.query_one("INSERT INTO face_groups DEFAULT VALUES RETURNING id",&[])?.get(0);
    let vector=format!("[{}]",std::iter::once("1").chain(std::iter::repeat_n("0",127)).collect::<Vec<_>>().join(","));
    let face:i64=client.query_one("INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id) VALUES ($1,0,0,10,10,$2::text::halfvec,$3,$4) RETURNING id",&[&image,&vector,&group,&a])?.get(0);
    let(tx,rx)=crossbeam_channel::unbounded();let mut created_groups=vec![group];
    let result=(||->anyhow::Result<()> {
        query(&mut client,Request::Groups,&tx)?;
        query(&mut client,Request::SaveView{name:root.clone(),state:serde_json::json!({"filters":[{"field":"person","value":format!("{root}-A"),"exclude":false}]})},&tx)?;
        query(&mut client,Request::AddTag{id:image,tag:"é".repeat(50)},&tx)?;
        anyhow::ensure!(client.query_one("SELECT count(*) FROM image_tags WHERE image_id=$1",&[&image])?.get::<_,i64>(0)==1,"Unicode character limit");
        query(&mut client,Request::PersonIgnore{id:a,ignored:true},&tx)?;
        query(&mut client,Request::PeopleForImage(image),&tx)?;
        anyhow::ensure!(rx.try_iter().filter_map(|event|if let Event::PeopleForImage(_,names)=event{Some(names)}else{None}).last().unwrap().is_empty(),"Ignored identity still offered");
        query(&mut client,Request::PersonIgnore{id:a,ignored:false},&tx)?;
        query(&mut client,Request::PersonRename{id:a,name:format!("東京 {root}")},&tx)?;
        query(&mut client,Request::GroupFaces{group,offset:0},&tx)?;
        anyhow::ensure!(rx.try_iter().any(|e|matches!(e,Event::GroupFaces{samples,..} if samples.len()==1)),"Group inspection");
        query(&mut client,Request::FaceMove{ids:vec![face],person_id:Some(b)},&tx)?;
        created_groups.push(client.query_one("SELECT group_id FROM faces WHERE id=$1",&[&face])?.get(0));
        anyhow::ensure!(client.query_one("SELECT person_id,group_id FROM faces WHERE id=$1",&[&face])?.get::<_,i64>(0)==b,"Reassignment");
        query(&mut client,Request::PersonMerge{source:a,target:b},&tx)?;
        let saved:String=client.query_one("SELECT state::text FROM saved_views WHERE name=$1",&[&root])?.get(0);
        let saved:serde_json::Value=serde_json::from_str(&saved)?;
        anyhow::ensure!(saved["filters"][0]["field"]=="person_id"&&saved["filters"][0]["value"]==b.to_string(),"Saved person filter did not follow merge");
        anyhow::ensure!(client.query_opt("SELECT id FROM people WHERE id=$1",&[&a])?.is_none(),"Merge source retained");
        query(&mut client,Request::FaceReject(vec![face]),&tx)?;
        anyhow::ensure!(client.query_opt("SELECT id FROM images WHERE id=$1",&[&image])?.is_some(),"Reject deleted original image record");
        let snapshot=rx.try_iter().find_map(|e|if let Event::FaceUndoReady(s)=e{Some(s)}else{None}).expect("Undo snapshot");
        query(&mut client,Request::FaceRestore(snapshot),&tx)?;
        anyhow::ensure!(client.query_one("SELECT person_id FROM faces WHERE id=$1",&[&face])?.get::<_,i64>(0)==b,"Undo did not restore assignment");
        Ok(())
    })();
    let mut groups:Vec<i64>=client.query("SELECT DISTINCT group_id FROM faces WHERE image_id=$1 AND group_id IS NOT NULL",&[&image])?.iter().map(|r|r.get(0)).collect();
    groups.extend(created_groups);
    client.execute("DELETE FROM images WHERE id=$1",&[&image])?;client.execute("DELETE FROM people WHERE id=ANY($1)",&[&vec![a,b]])?;
    client.execute("DELETE FROM saved_views WHERE name=$1",&[&root])?;
    client.execute("DELETE FROM face_groups WHERE id=$1 OR id=ANY($2)",&[&group,&groups])?;
    result
}
