use serde::{Deserialize, Serialize};
use std::path::{Path,PathBuf};

#[derive(Default, Deserialize, Serialize)]
pub struct Settings {
    pub editors: Vec<PathBuf>,
    #[serde(skip)] baseline:Vec<PathBuf>,
    #[serde(skip)] pub error:Option<String>,
}

fn path() -> Option<PathBuf> {
    std::env::var_os("IMAGE_CLI_CONFIG_DIR").map(PathBuf::from).or_else(||
        std::env::var_os("LOCALAPPDATA").or_else(||std::env::var_os("USERPROFILE")).map(|base|PathBuf::from(base).join("image-cli")))
        .map(|base|base.join("viewer-settings.json"))
}
impl Settings {
    pub fn load()->Self {
        let Some(path)=path() else{return Self::default();};
        Self::load_from(&path)
    }
    fn load_from(path:&Path)->Self {
        match std::fs::read(path) {
            Ok(data)=>match serde_json::from_slice::<Self>(&data) {Ok(mut settings)=>{settings.baseline=settings.editors.clone();settings},Err(error)=>Self{error:Some(format!("Editor settings could not be read: {error}")),..Default::default()}},
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Self::default(),
            Err(error)=>Self{error:Some(format!("Editor settings unavailable: {error}")),..Default::default()},
        }
    }
    pub fn save(&mut self)->Result<(),String> {
        let path=path().ok_or("No user settings directory")?;
        self.save_to(&path)
    }
    fn save_to(&mut self,path:&Path)->Result<(),String>{
        std::fs::create_dir_all(path.parent().ok_or("Invalid settings path")?).map_err(|e|e.to_string())?;
        let lock=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_extension("lock")).map_err(|e|e.to_string())?;
        let started=std::time::Instant::now();
        loop{match lock.try_lock(){Ok(())=>break,Err(std::fs::TryLockError::WouldBlock) if started.elapsed()<std::time::Duration::from_millis(500)=>std::thread::sleep(std::time::Duration::from_millis(10)),Err(error)=>return Err(format!("Editor settings are busy; retry: {error}"))}}

        let mut current=match std::fs::read(&path) {Ok(data)=>serde_json::from_slice::<Self>(&data).map_err(|e|format!("Keep the existing settings intact; could not read them: {e}"))?,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Self::default(),Err(e)=>return Err(e.to_string())};
        let removed:Vec<_>=self.baseline.iter().filter(|p|!self.editors.contains(p)).collect();
        current.editors.retain(|p|!removed.contains(&p));
        for editor in &self.editors {if !self.baseline.contains(editor)&&!current.editors.contains(editor){current.editors.push(editor.clone());}}
        let data=serde_json::to_vec_pretty(&current).map_err(|e|e.to_string())?;
        let temporary=path.with_extension(format!("{}.tmp",std::process::id()));
        std::fs::write(&temporary,data).map_err(|e|e.to_string())?;
        std::fs::rename(&temporary,&path).map_err(|e|e.to_string())?;
        self.editors=current.editors;self.baseline=self.editors.clone();self.error=None;
        Ok(())
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn concurrent_stale_windows_merge_editor_changes_without_overwrite(){
        let folder=std::env::temp_dir().join(format!("imagecli-settings-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir_all(&folder).unwrap();let path=folder.join("settings.json");
        let mut initial=Settings::default();initial.editors.push("initial.exe".into());initial.save_to(&path).unwrap();
        let mut first=Settings::load_from(&path);let mut second=Settings::load_from(&path);first.editors.push("first.exe".into());second.editors.push("second.exe".into());
        let other_path=path.clone();let worker=std::thread::spawn(move||first.save_to(&other_path));second.save_to(&path).unwrap();worker.join().unwrap().unwrap();
        let stored=Settings::load_from(&path);assert!(stored.editors.contains(&PathBuf::from("first.exe"))&&stored.editors.contains(&PathBuf::from("second.exe")));
        std::fs::remove_file(&path).unwrap();std::fs::remove_file(path.with_extension("lock")).unwrap();std::fs::remove_dir(folder).unwrap();
    }
    #[test] fn corrupt_settings_are_reported_and_not_overwritten(){
        let folder=std::env::temp_dir().join(format!("imagecli-corrupt-settings-{}",std::process::id()));std::fs::create_dir_all(&folder).unwrap();let path=folder.join("settings.json");std::fs::write(&path,b"corrupt").unwrap();let mut settings=Settings::load_from(&path);assert!(settings.error.is_some());assert!(settings.save_to(&path).is_err());assert_eq!(std::fs::read(&path).unwrap(),b"corrupt");std::fs::remove_file(&path).unwrap();std::fs::remove_file(path.with_extension("lock")).unwrap();std::fs::remove_dir(folder).unwrap();
    }
}
