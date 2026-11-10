//! Local/share presence and catalogued SSH archives, resolved on the search worker.
use postgres::Client;
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}};

#[derive(Default)]
pub(super) struct LocalFiles {
    pub ids:Vec<i64>,
    pub packs:HashSet<String>,
}

fn storage_path(path:&Path)->bool {
    if !path.is_absolute() {return false;}
    // UNC paths are checked by Presence on the worker, never on the UI thread.
    if path.to_string_lossy().starts_with("\\\\") {return true;}
    #[cfg(windows)] {
        #[link(name="kernel32")]
        unsafe extern "system" {fn GetDriveTypeW(root:*const u16)->u32;}
        use std::os::windows::ffi::OsStrExt;
        let Some(prefix)=path.components().next() else {return false;};
        let root=PathBuf::from(prefix.as_os_str()).join("\\");
        let wide:Vec<u16>=root.as_os_str().encode_wide().chain(Some(0)).collect();
        // Include mapped network drives alongside local disks.
        return matches!(unsafe{GetDriveTypeW(wide.as_ptr())},2|3|4|5|6);
    }
    #[cfg(not(windows))] {true}
}

fn relative_path(relative:&str)->Option<String> {
    let relative=relative.replace('\\',"/");
    if relative.is_empty() || relative.split('/').any(|p|p.is_empty()||p==".."||p=="."||p.contains(':')) {return None;}
    Some(relative)
}
fn under(root:&str,relative:&str)->Option<PathBuf> {
    let relative=relative_path(relative)?;
    let root=Path::new(root);
    storage_path(root).then(||root.join(relative))
}

fn network_location(item:&serde_json::Value)->bool {
    matches!(item["kind"].as_str(),Some("sftp"|"sshfs"))
        && item["alias"].as_str().is_some_and(|s|!s.is_empty())
        && item["remote"].as_str().is_some_and(|s|!s.is_empty())
}
fn network_archive(config:&serde_json::Value,location:Option<&str>,stored:Option<&str>,size:Option<i64>)->bool {
    let Some(location)=location else {return false;};
    stored.and_then(relative_path).is_some()&&size.is_some_and(|s|s>=0)&&network_location(&config["locations"][location])
}

#[derive(Default)]
struct Presence {parents:HashMap<PathBuf,bool>}
impl Presence {
    fn file(&mut self,path:&Path,size:Option<i64>)->bool {
        let Some(parent)=path.parent() else {return false;};
        let available=*self.parents.entry(parent.to_owned()).or_insert_with(||storage_path(parent)&&parent.is_dir());
        available && path.metadata().is_ok_and(|m|m.is_file()&&size.is_none_or(|s|s>=0&&m.len()==s as u64))
    }
}

pub(super) fn load(db:&mut Client)->Result<LocalFiles,String> {
    let config_path=std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir).join("asset-cli/storage.json");
    let config=match std::fs::read_to_string(config_path) {
        Ok(text)=>serde_json::from_str::<serde_json::Value>(&text).map_err(|e|format!("Cannot read storage settings: {e}"))?,
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>serde_json::Value::Null,
        Err(e)=>return Err(format!("Cannot read storage settings: {e}")),
    };
    load_with_config(db,&config)
}

pub(super) fn load_with_config(db:&mut Client,config:&serde_json::Value)->Result<LocalFiles,String> {
    let roots:HashMap<String,String>=config["locations"].as_object().into_iter().flatten()
        .filter(|(_,v)|v["kind"]=="local")
        .filter_map(|(k,v)|Some((k.clone(),v["path"].as_str()?.to_owned()))).collect();
    let rows=db.query("SELECT a.id,a.pack_id,a.source_path,a.size_bytes,a.metadata->>'relative_path',p.local_path,af.location,af.stored_path,af.size_bytes,c.local_path FROM assets a JOIN packs p ON p.id=a.pack_id LEFT JOIN archive_files af ON af.revision_id=p.archive_revision AND af.path=a.metadata->>'relative_path' LEFT JOIN asset_file_cache c ON c.revision_id=af.revision_id AND c.path=af.path AND c.sha256=af.sha256 WHERE a.source_path IS NOT NULL OR p.local_path IS NOT NULL OR af.revision_id IS NOT NULL",&[]).map_err(|e|e.to_string())?;
    let mut result=LocalFiles::default();
    let mut presence=Presence::default();
    for row in rows {
        let source:Option<String>=row.get(2);
        let size:Option<i64>=row.get(3);
        let relative:Option<String>=row.get(4);
        let pack_root:Option<String>=row.get(5);
        let location:Option<String>=row.get(6);
        let stored:Option<String>=row.get(7);
        let archive_size:Option<i64>=row.get(8);
        let cached:Option<String>=row.get(9);
        // A published SSH archive is stored even when the host is currently offline.
        // Retrieval performs connectivity and checksum validation; browsing does no SSH IO.
        let archived=network_archive(config,location.as_deref(),stored.as_deref(),archive_size);
        let paths=[cached.map(|p|(PathBuf::from(p),archive_size)),
            location.as_ref().and_then(|l|roots.get(l)).zip(stored.as_ref()).and_then(|(r,p)|under(r,p)).map(|p|(p,archive_size)),
            pack_root.as_ref().zip(relative.as_ref()).and_then(|(r,p)|under(r,p)).map(|p|(p,archive_size.or(size))),
            source.map(|p|(PathBuf::from(p),size))];
        if archived||paths.into_iter().flatten().any(|(path,size)|presence.file(&path,size)) {
            result.ids.push(row.get(0));result.packs.insert(row.get(1));
        }
    }
    // A downloaded pack can be stored on the LAN before its assets are indexed.
    let network_locations:Vec<String>=config["locations"].as_object().into_iter().flatten()
        .filter(|(_,item)|network_location(item)).map(|(name,_)|name.clone()).collect();
    if !network_locations.is_empty() {
        for row in db.query("SELECT p.id,f.location,f.stored_path,f.size_bytes FROM packs p JOIN archive_files f ON f.revision_id=p.archive_revision WHERE f.location=ANY($1)",&[&network_locations]).map_err(|e|e.to_string())? {
            let location:String=row.get(1);let stored:String=row.get(2);
            if network_archive(config,Some(&location),Some(&stored),Some(row.get(3))){result.packs.insert(row.get(0));}
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presence_rejects_deleted_wrong_size_and_unsafe_paths() {
        let root=std::env::temp_dir().join(format!("asset-presence-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file=root.join("asset.uasset");std::fs::write(&file,b"asset").unwrap();
        let mut presence=Presence::default();
        assert!(presence.file(&file,Some(5)));
        assert!(!presence.file(&file,Some(4)));
        assert!(!presence.file(&root, None));
        std::fs::remove_file(&file).unwrap();
        assert!(!presence.file(&file,Some(5)));
        #[cfg(windows)] assert!(storage_path(Path::new(r"\\server\share\asset.uasset")));
        for bad in ["../asset","/asset","C:/asset","a/../../asset",r"a\..\asset"] {assert!(under(root.to_str().unwrap(),bad).is_none());}
        assert!(under(root.to_str().unwrap(),"Content/asset.uasset").is_some());
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn network_archive_requires_a_configured_location_and_valid_archive_record() {
        let config=serde_json::json!({"locations":{
            "lan":{"kind":"sftp","alias":"archive","remote":"D:/assets"},
            "mounted":{"kind":"sshfs","alias":"archive","remote":"D:/assets"},
            "disk":{"kind":"local","path":"D:/assets"},
            "invalid":{"kind":"sftp"}
        }});
        for location in ["lan","mounted"] {assert!(network_archive(&config,Some(location),Some("packs/asset.uasset"),Some(5)));}
        for location in ["disk","invalid","unknown"] {assert!(!network_archive(&config,Some(location),Some("packs/asset.uasset"),Some(5)));}
        for path in [None,Some("../asset.uasset"),Some("/asset.uasset"),Some("C:/asset.uasset")] {
            assert!(!network_archive(&config,Some("lan"),path,Some(5)));
        }
        assert!(!network_archive(&config,Some("lan"),Some("packs/a"),None));
        assert!(!network_archive(&config,Some("lan"),Some("packs/a"),Some(-1)));
    }
}
