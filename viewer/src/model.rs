use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use serde::Deserialize;
use std::{io::{BufRead, BufReader, Read, Write}, process::{Child, Command, Stdio}, sync::{Arc, atomic::{AtomicU64, Ordering}}, thread, time::Duration};

pub enum Request { Encode { generation: u64, text: String }, Retry }
pub enum Event { Ready, Vector { generation: u64, vector: Vec<f32> }, Error(String) }
#[derive(Deserialize)] struct WorkerResponse { embedding: Option<Vec<f32>>, error: Option<String> }

struct Worker { child: Child, input: Sender<Vec<u8>>, replies: Receiver<Result<String,String>>,
    #[cfg(windows)] _tree:Option<crate::bridge::ProcessTree>,
}
impl Drop for Worker { fn drop(&mut self) { let _=self.child.kill();let _=self.child.wait(); } }
impl Worker {
    fn spawn(python:Option<&str>) -> anyhow::Result<Self> {
        let mut command=Command::new(python.map(str::to_owned).or_else(||std::env::var("IMAGE_CLI_VISION_PYTHON").ok()).unwrap_or_else(||"python".into()));
        crate::actions::scrub_environment(&mut command);
        command.args(["-m","image_atlas.embedding_worker"]).env("PYTHONUTF8","1").env("PYTHONIOENCODING","utf-8")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
        #[cfg(windows)] { use std::os::windows::process::CommandExt;command.creation_flags(0x08000000); }
        let mut child=command.spawn()?;
        #[cfg(windows)] let tree=match crate::bridge::ProcessTree::attach(&child){Ok(tree)=>Some(tree),Err(error)=>{let _=child.kill();let _=child.wait();return Err(anyhow::Error::msg(error));}};
        let mut stdin=child.stdin.take().ok_or_else(||anyhow::anyhow!("Missing model input"))?;
        let(input,writes)=crossbeam_channel::bounded::<Vec<u8>>(1);
        thread::spawn(move ||{for message in writes{if stdin.write_all(&message).and_then(|_|stdin.flush()).is_err(){break;}}});
        let output=child.stdout.take().ok_or_else(||anyhow::anyhow!("Missing model output"))?;
        let(tx,replies)=crossbeam_channel::bounded(2);
        thread::spawn(move || {
            let mut reader=BufReader::new(output);
            loop {
                let mut line=String::new();
                let result=reader.by_ref().take(1024*1024).read_line(&mut line);
                let result=match result {Ok(0)=>Err("OpenCLIP worker closed".into()),Ok(_) if line.ends_with('\n')=>Ok(line),Ok(_)=>Err("Model response exceeds its size limit".into()),Err(e)=>Err(e.to_string())};
                let failed=result.is_err();if tx.send(result).is_err()||failed{break;}
            }
        });
        Ok(Self{child,input,replies,#[cfg(windows)] _tree:tree})
    }
    fn encode(&mut self,text:&str,timeout:Duration)->anyhow::Result<Vec<f32>> {
        let value=serde_json::json!({"operation":"text","value":text});
        anyhow::ensure!(text.len()<=65536,"Search text is too long");
        let mut bytes=serde_json::to_vec(&value)?;bytes.push(b'\n');
        self.input.send_timeout(bytes,timeout).map_err(|_|anyhow::anyhow!("Model input timed out"))?;
        let line=self.replies.recv_timeout(timeout).map_err(|_|anyhow::anyhow!("OpenCLIP response timed out; use Retry visual search"))?.map_err(anyhow::Error::msg)?;
        let response:WorkerResponse=serde_json::from_str(&line)?;
        if let Some(error)=response.error{anyhow::bail!(error);}
        let vector=response.embedding.ok_or_else(||anyhow::anyhow!("Model returned no vector"))?;
        anyhow::ensure!(vector.len()==512&&vector.iter().all(|n|n.is_finite()),"Model returned invalid embedding");Ok(vector)
    }
}
pub fn start(ui:egui::Context,active:Arc<AtomicU64>,python:Option<String>)->(Sender<Request>,Receiver<Event>){
    let(request_tx,requests)=crossbeam_channel::bounded(32);let(events,event_rx)=crossbeam_channel::unbounded();
    thread::spawn(move || {
        let mut worker:Option<Worker>=None;
        for request in requests {
            if matches!(request,Request::Retry){worker=None;continue;}
            let Request::Encode{generation,text}=request else{continue;};
            if generation!=active.load(Ordering::Relaxed){continue;}
            let result=(|| -> anyhow::Result<Vec<f32>> {
                if worker.is_none(){
                    let mut started=Worker::spawn(python.as_deref())?;
                    started.encode("photo",Duration::from_secs(180))?;
                    worker=Some(started);let _=events.send(Event::Ready);ui.request_repaint();
                }
                worker.as_mut().unwrap().encode(&text,Duration::from_secs(60))
            })();
            let event=match result{Ok(vector)=>Event::Vector{generation,vector},Err(error)=>{worker=None;Event::Error(format!("Visual search unavailable: {error}"))}};
            if events.send(event).is_err(){break;}ui.request_repaint();
        }
    });(request_tx,event_rx)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn unresponsive_exchange_has_a_deadline_and_kills_worker(){
        let mut command=Command::new(std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into()));command.args(["-c","import time;time.sleep(10)"]);
        #[cfg(windows)]{use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
        let child=command.spawn().unwrap();let(input,_writes)=crossbeam_channel::bounded(1);let(_tx,replies)=crossbeam_channel::bounded(1);
        let mut worker=Worker{child,input,replies,#[cfg(windows)] _tree:None};let started=std::time::Instant::now();
        assert!(worker.encode("photo",Duration::from_millis(50)).unwrap_err().to_string().contains("timed out"));drop(worker);assert!(started.elapsed()<Duration::from_secs(2));
    }
}
