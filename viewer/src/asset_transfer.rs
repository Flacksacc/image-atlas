//! CLI-backed asset transfers. No network credentials or filesystem mounts in UI code.
use crossbeam_channel::{Receiver, unbounded};
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub enum Message { Event(serde_json::Value), Done(Result<(),String>) }

pub struct Transfer {
    pub messages:Receiver<Message>,
    pub cancel:Arc<AtomicBool>,
}

pub fn start(asset_id:i64, project:Option<String>, availability:bool,dsn:&str)->Transfer {
    launch(asset_id,project,availability,false,false,dsn)
}

pub fn start_open(asset_id:i64,project:Option<String>,download_pack:bool,dsn:&str)->Transfer {
    launch(asset_id,project,false,true,download_pack,dsn)
}

fn arguments(asset_id:i64,project:Option<&str>,availability:bool,opening:bool,download_pack:bool,cancel_file:&std::path::Path)->Vec<std::ffi::OsString> {
    let mut args:Vec<std::ffi::OsString>=vec![if availability{"availability"}else if opening{"open-asset"}else{"fetch"}.into(),asset_id.to_string().into(),"--events".into()];
    if !availability {args.extend(["--cancel-file".into(),cancel_file.as_os_str().to_owned()]);}
    if let Some(path)=project {args.extend(["--project".into(),path.into()]);}
    if opening&&download_pack {args.push("--download-pack".into());}
    args
}

fn launch(asset_id:i64, project:Option<String>, availability:bool,opening:bool,download_pack:bool,dsn:&str)->Transfer {
    let dsn=dsn.to_owned();
    let (sender,messages)=unbounded();
    let cancel=Arc::new(AtomicBool::new(false));
    let requested=cancel.clone();
    std::thread::spawn(move || {
        let outcome=(||->Result<(),String>{
            let executable=std::env::var_os("ASSET_CLI_BIN").unwrap_or_else(|| {
                let local=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../asset-cli/.venv/Scripts/asset-cli.exe");
                if local.is_file(){local.into_os_string()}else{"asset-cli".into()}
            });
            let folder=std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir).join("asset-cli/viewer");
            std::fs::create_dir_all(&folder).map_err(|e|e.to_string())?;
            let nonce=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
            let cancel_file=folder.join(format!("cancel-{}-{nonce}",std::process::id()));
            let mut command=Command::new(executable);
            command.args(arguments(asset_id,project.as_deref(),availability,opening,download_pack,&cancel_file));
            // Bind every operation to the displayed catalog, independent of launch mode.
            crate::actions::scrub_environment(&mut command);
            command.env("ASSET_CLI_DSN",&dsn);
            command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            #[cfg(windows)] {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            let mut child=command.spawn().map_err(|e|format!("Cannot start asset-cli: {e}. Set ASSET_CLI_BIN to its executable."))?;
            let output=child.stdout.take().ok_or("Missing CLI output")?;
            let errors=child.stderr.take().ok_or("Missing CLI diagnostics")?;
            let events=sender.clone();
            let open_cancel=requested.clone();
            let reader=std::thread::spawn(move || -> Result<(),String> {
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    if let Ok(mut value)=serde_json::from_str::<serde_json::Value>(&line) {
                        if opening&&value["event"]=="result"&&value["open_native"]==true {
                            if open_cancel.load(Ordering::Relaxed) {continue;}
                            let path=value["open_path"].as_str().ok_or("The CLI did not return the selected asset path")?;
                            crate::actions::open_original(std::path::Path::new(path))?;
                            value["opened"]=true.into();
                        }
                        let _=events.send(Message::Event(value));
                    }
                }
                Ok(())
            });
            let diagnostics=std::thread::spawn(move || {
                let mut input=errors;
                let mut tail=Vec::new();let mut buffer=[0;4096];
                while let Ok(count)=input.read(&mut buffer) {
                    if count==0 {break;}
                    tail.extend_from_slice(&buffer[..count]);
                    if tail.len()>8192 {tail.drain(..tail.len()-8192);}
                }
                String::from_utf8_lossy(&tail).into_owned()
            });
            let mut stopping=None;
            let status=loop {
                if let Some(status)=child.try_wait().map_err(|e|e.to_string())? {break status;}
                if requested.load(Ordering::Relaxed) && stopping.is_none() {
                    std::fs::write(&cancel_file,b"cancel").map_err(|e|e.to_string())?;
                    stopping=Some(Instant::now());
                }
                if stopping.is_some_and(|start:Instant|start.elapsed()>Duration::from_secs(35)) {
                    #[cfg(windows)] {
                        use std::os::windows::process::CommandExt;
                        let _=Command::new("taskkill").args(["/PID",&child.id().to_string(),"/T","/F"])
                            .creation_flags(0x08000000).stdout(Stdio::null()).stderr(Stdio::null()).status();
                    }
                    let _=child.kill();
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let opened=reader.join().map_err(|_|"Asset output worker stopped unexpectedly".to_owned())?;
            let detail=diagnostics.join().unwrap_or_default();
            let _=std::fs::remove_file(cancel_file);
            if requested.load(Ordering::Relaxed){Err("Operation cancelled; archived files were preserved.".into())}
            else if status.success(){opened}
            else {Err(format!("asset-cli exited with {status}: {detail}"))}
        })();
        let _=sender.send(Message::Done(outcome));
    });
    Transfer{messages,cancel}
}

impl Drop for Transfer {
    fn drop(&mut self) { self.cancel.store(true,Ordering::Relaxed); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_requires_explicit_whole_pack_flag_and_preserves_project_arguments() {
        let _starter:fn(i64,Option<String>,bool,&str)->Transfer=start_open;
        let cancel=std::path::Path::new("D:/cache/cancel marker");
        let project="D:/My Game/Project.uproject";
        let args=arguments(42,Some(project),false,true,false,cancel);
        assert_eq!(args[0],"open-asset");
        assert!(!args.iter().any(|a|a=="--download-pack"));
        assert!(args.windows(2).any(|a|a[0]=="--project"&&a[1]==project));
        assert!(arguments(42,None,false,true,true,cancel).iter().any(|a|a=="--download-pack"));
        assert!(!arguments(42,None,false,false,true,cancel).iter().any(|a|a=="--download-pack"));
    }
}
