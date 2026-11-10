//! Bounded, UTF-8 subprocess protocol shared by native/Python services.
use std::{io::{Read,Write}, process::{Command, Output, Stdio}, time::{Duration, Instant}};

pub fn output(command: &mut Command, timeout: Duration, limit: usize) -> Result<Output, String> {
    output_until(command,timeout,limit,||false)
}

pub fn output_until(command:&mut Command,timeout:Duration,limit:usize,cancelled:impl Fn()->bool)->Result<Output,String>{
    run_output(command,timeout,limit,cancelled,None,true)
}

/// Shell helpers intentionally launch applications that must survive their parent.
pub fn output_detached(command:&mut Command,timeout:Duration,limit:usize)->Result<Output,String>{
    run_output(command,timeout,limit,||false,None,false)
}

pub fn output_with_input(command:&mut Command,timeout:Duration,limit:usize,input:Vec<u8>)->Result<Output,String>{
    run_output(command,timeout,limit,||false,Some(input),true)
}

fn run_output(command:&mut Command,timeout:Duration,limit:usize,cancelled:impl Fn()->bool,input:Option<Vec<u8>>,manage_tree:bool)->Result<Output,String>{
    command.env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1")
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    if input.is_some(){command.stdin(Stdio::piped());}
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let _job = if manage_tree {match ProcessTree::attach(&child) {Ok(job)=>Some(job),Err(error)=>{let _=child.kill();let _=child.wait();return Err(error);}}}else{None};
    #[cfg(not(windows))] let _=manage_tree;
    if let Some(bytes)=input {if let Some(mut stdin)=child.stdin.take(){std::thread::spawn(move ||{let _=stdin.write_all(&bytes);});}}
    let stdout = child.stdout.take().ok_or("Missing stdout")?;
    let stderr = child.stderr.take().ok_or("Missing stderr")?;
    let read = move |stream: Box<dyn Read + Send>| {
        let mut data=Vec::new();
        stream.take(limit as u64 + 1).read_to_end(&mut data).map_err(|e|e.to_string())?;
        if data.len()>limit { return Err("Service output exceeds its size limit".into()); }
        Ok(data)
    };
    let (tx,rx)=std::sync::mpsc::channel();
    let out_tx=tx.clone();
    std::thread::spawn(move || {let _=out_tx.send((true,read(Box::new(stdout))));});
    std::thread::spawn(move || {let _=tx.send((false,read(Box::new(stderr))));});
    let started=Instant::now();
    let mut out=None; let mut err=None; let mut status=None;
    loop {
        if cancelled(){let _=child.kill();let _=child.wait();return Err("Operation cancelled".into());}
        while let Ok((is_out,result))=rx.try_recv() {
            match result { Ok(data)=>if is_out {out=Some(data)} else {err=Some(data)},
                Err(error)=>{let _=child.kill();let _=child.wait();return Err(error);} }
        }
        if status.is_none() {status=child.try_wait().map_err(|e|e.to_string())?;}
        if let (Some(status),Some(stdout),Some(stderr))=(status,out.as_ref(),err.as_ref()) {
            return Ok(Output{status,stdout:stdout.clone(),stderr:stderr.clone()});
        }
        if started.elapsed()>=timeout {let _=child.kill();let _=child.wait();return Err(format!("Service timed out after {} seconds; retry when it is available",timeout.as_secs()));}
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
pub(crate) struct ProcessTree(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl ProcessTree {
    pub(crate) fn attach(child:&std::process::Child)->Result<Self,String>{
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::CloseHandle,System::JobObjects::*};
        unsafe {
            let handle=CreateJobObjectW(std::ptr::null(),std::ptr::null());
            if handle.is_null(){return Err(std::io::Error::last_os_error().to_string());}
            let mut limits:JOBOBJECT_EXTENDED_LIMIT_INFORMATION=std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(handle,JobObjectExtendedLimitInformation,&limits as *const _ as *const _,std::mem::size_of_val(&limits) as u32)==0
                || AssignProcessToJobObject(handle,child.as_raw_handle() as _)==0 {
                let error=std::io::Error::last_os_error().to_string();CloseHandle(handle);return Err(error);
            }
            Ok(Self(handle))
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {fn drop(&mut self){unsafe{windows_sys::Win32::Foundation::CloseHandle(self.0);}}}

pub fn readable(value:&serde_json::Value)->String {
    match value {
        serde_json::Value::Object(fields)=>fields.iter().map(|(key,value)|format!("{}: {}",key.replace('_'," "),readable(value))).collect::<Vec<_>>().join("\n"),
        serde_json::Value::Array(rows)=>rows.iter().map(readable).collect::<Vec<_>>().join("\n\n"),
        serde_json::Value::String(text)=>text.clone(),
        serde_json::Value::Null=>"—".into(),
        other=>other.to_string(),
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn readable_status_omits_json_syntax(){assert_eq!(readable(&serde_json::json!({"image_count":3,"name":"東京 · café"})),"image count: 3\nname: 東京 · café");}
    #[test] fn hung_process_is_bounded(){
        let mut command=Command::new(std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into()));
        command.args(["-c","import time;time.sleep(10)"]);
        let start=Instant::now(); assert!(output(&mut command,Duration::from_millis(100),1024).unwrap_err().contains("timed out"));assert!(start.elapsed()<Duration::from_secs(3));
    }
    #[test] fn cancelled_process_exits_without_waiting_for_its_deadline(){
        let mut command=Command::new(std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into()));command.args(["-c","import time;time.sleep(10)"]);
        let start=Instant::now();assert!(output_until(&mut command,Duration::from_secs(30),1024,||true).unwrap_err().contains("cancelled"));assert!(start.elapsed()<Duration::from_secs(3));
    }
    #[cfg(windows)]
    #[test] fn timeout_terminates_descendants(){
        let marker=std::env::temp_dir().join(format!("imagecli-descendant-{}-{}.txt",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut command=Command::new(std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into()));
        command.args(["-c","import subprocess,sys,time;subprocess.Popen([sys.executable,'-c','import time,sys;from pathlib import Path;time.sleep(1);Path(sys.argv[1]).write_text(\"survived\")',sys.argv[1]],creationflags=subprocess.CREATE_NO_WINDOW);from pathlib import Path;Path(sys.argv[1]+'.started').write_text('started');time.sleep(10)"]).arg(&marker);
        assert!(output(&mut command,Duration::from_millis(500),1024).unwrap_err().contains("timed out"));
        let started=std::path::PathBuf::from(format!("{}.started",marker.display()));let was_started=started.exists();let _=std::fs::remove_file(&started);assert!(was_started,"Child startup was not exercised");
        std::thread::sleep(Duration::from_millis(1200));let survived=marker.exists();if survived{let _=std::fs::remove_file(&marker);}assert!(!survived,"Bridge left a child process running");
    }
}
