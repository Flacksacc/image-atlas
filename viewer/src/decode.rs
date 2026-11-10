use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use image::GenericImageView;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock,Arc,atomic::{AtomicBool,Ordering}};
use std::thread;

static SOURCE_DECODE: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn parse_pixels(bytes:&[u8])->Result<egui::ColorImage,String>{
    let split=bytes.iter().position(|b|*b==b'\n').ok_or("Missing pixel header")?;
    let dims=std::str::from_utf8(&bytes[..split]).map_err(|e|e.to_string())?
        .split_whitespace().map(str::parse::<usize>).collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    if dims.len()!=2||dims[0]==0||dims[1]==0||dims[0]>1024||dims[1]>1024||bytes.len()-split-1!=dims[0]*dims[1]*4{return Err("Invalid preview pixel dimensions".into());}
    Ok(egui::ColorImage::from_rgba_unmultiplied([dims[0],dims[1]],&bytes[split+1..]))
}

pub enum Job {
    Thumb(i64, Vec<u8>,Option<String>),
    Preview(i64, Vec<u8>,Option<String>),
    Full(i64, PathBuf, u32,u32,Option<String>),
    Face(i64, PathBuf, (i32, i32, i32, i32),Option<String>),
}

pub struct Decoded {
    pub id: i64,
    pub full: bool,
    pub face: bool,
    pub revision:Option<String>,
    pub image: Result<egui::ColorImage, String>,
}

fn color_image(image: image::DynamicImage) -> egui::ColorImage {
    let rgba = image.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    )
}

fn orientation(path: &PathBuf) -> Option<u32> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)
}

fn orient(image: image::DynamicImage, value: Option<u32>) -> image::DynamicImage {
    match value {
        Some(2) => image.fliph(),
        Some(3) => image.rotate180(),
        Some(4) => image.flipv(),
        Some(5) => image.rotate270().fliph(),
        Some(6) => image.rotate90(),
        Some(7) => image.rotate90().fliph(),
        Some(8) => image.rotate270(),
        _ => image,
    }
}

fn python_fallback(
    path: &PathBuf,
    edge: u32,
    page: u32,
    face: Option<(i32, i32, i32, i32)>,
    cancelled:&AtomicBool,
) -> anyhow::Result<egui::ColorImage> {
    let python = std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.venv/Scripts/python.exe").to_string_lossy().into_owned());
    let mut command = Command::new(python);
    crate::actions::scrub_environment(&mut command);
    command
        .args(["-m","image_atlas.viewer_decode"])
        .arg(path)
        .arg(edge.to_string());
    if let Some((x, y, w, h)) = face {
        for value in [x, y, w, h] {
            command.arg(value.to_string());
        }
    }
    command.arg("--page").arg(page.to_string());
    if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT") {command.env("PYTHONPATH",root);}
    let result=crate::bridge::output_until(&mut command,std::time::Duration::from_secs(30),edge as usize*edge as usize*4+1024,||cancelled.load(Ordering::Acquire)).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(result.status.success(),"Image decoder failed: {}",String::from_utf8_lossy(&result.stderr));
    let mut output=BufReader::new(result.stdout.as_slice());
    let mut header=String::new();output.read_line(&mut header)?;
    let dims:Vec<usize>=header.split_whitespace().map(str::parse).collect::<Result<_,_>>()?;
    anyhow::ensure!(dims.len()==2&&dims[0]>0&&dims[1]>0&&dims[0]<=edge as usize&&dims[1]<=edge as usize,"Invalid decoder dimensions");
    let mut bytes=vec![0;dims[0]*dims[1]*4];output.read_exact(&mut bytes)?;
    Ok(egui::ColorImage::from_rgba_unmultiplied([dims[0],dims[1]],&bytes))
}

fn decode(job: Job,cancelled:&AtomicBool) -> Decoded {
    match job {
        Job::Preview(id, bytes, revision) => {
            let python=std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|"python".into());
            let mut command=Command::new(python);
            crate::actions::scrub_environment(&mut command);
            command.args(["-m","image_atlas.previews","--stdin"]);
            if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
            let image=crate::bridge::output_with_input(&mut command,std::time::Duration::from_secs(15),1024*1024*4+1024,bytes)
                .and_then(|out|if out.status.success(){parse_pixels(&out.stdout)}else{Err("JPEG XL decode failed".into())});
            Decoded{id,revision,full:true,face:false,image}
        },
        Job::Thumb(id, bytes,revision) => Decoded {
            revision,
            id,
            full: false,
            face: false,
            image: image::load_from_memory(&bytes)
                .map(color_image)
                .map_err(|e| e.to_string()),
        },
        Job::Full(id, path, edge,page,revision) => {
            let _permit = SOURCE_DECODE
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("decode lock");
            if cancelled.load(Ordering::Acquire){return Decoded{id,revision,full:true,face:false,image:Err("Preview cancelled".into())};}
            let large = image::image_dimensions(&path)
                .is_ok_and(|(w, h)| (w as u64) * (h as u64) > 40_000_000 && edge < w.max(h));
            let image = if large {
                python_fallback(&path, edge, page, None,cancelled).map_err(|e| e.to_string())
            } else {
                image::ImageReader::open(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.decode().map_err(|e| e.to_string()))
                    .map(|im| {
                        let im = orient(im, orientation(&path));
                        let (w, h) = im.dimensions();
                        let im = if w > edge || h > edge {
                            im.thumbnail(edge, edge)
                        } else {
                            im
                        };
                        color_image(im)
                    })
                    .or_else(|_| python_fallback(&path, edge, page, None,cancelled).map_err(|e| e.to_string()))
            };
            Decoded {
                revision,id,
                full: true,
                face: false,
                image,
            }
        }
        Job::Face(id, path, (x, y, width, height),revision) => {
            let _permit = SOURCE_DECODE
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("decode lock");
            if cancelled.load(Ordering::Acquire){return Decoded{id,revision,full:false,face:true,image:Err("Preview cancelled".into())};}
            // Decode/crop in a bounded subprocess, never allocate a panorama in the UI process.
            let image = python_fallback(&path,120,0,Some((x,y,width,height)),cancelled).map_err(|e|e.to_string());
            Decoded {
                revision,id,
                full: false,
                face: true,
                image,
            }
        }
    }
}

#[allow(dead_code)]
pub fn start(ui: egui::Context) -> (Sender<Job>, Receiver<Decoded>) {
    let(tx,rx,_cancel)=start_catalog(ui,None);(tx,rx)
}

pub fn start_catalog(ui:egui::Context,catalog:Option<String>)->(Sender<Job>,Receiver<Decoded>,Arc<AtomicBool>){
    let cancelled=Arc::new(AtomicBool::new(false));
    let (jobs_tx, jobs_rx) = crossbeam_channel::bounded(256);
    let (done_tx, done_rx) = crossbeam_channel::unbounded();
    for _ in 0..2 {
        let rx = jobs_rx.clone();
        let tx = done_tx.clone();
        let ui = ui.clone();
        let catalog=catalog.clone();
        let cancelled=cancelled.clone();
        thread::spawn(move || {
            for job in rx {
                if cancelled.load(Ordering::Acquire){break;}
                let source=match &job {Job::Full(_,path,_,_,_)|Job::Face(_,path,_,_)=>Some(path),_=>None};
                if let Some(path)=source {
                    if let Err(error)=crate::catalogs::allowed(path,catalog.as_deref()) {
                        let(id,full,face,revision)=match job {Job::Full(id,_,_,_,revision)=>(id,true,false,revision),Job::Face(id,_,_,revision)=>(id,false,true,revision),_=>unreachable!()};
                        let _=tx.send(Decoded{id,full,face,revision,image:Err(error)});ui.request_repaint();continue;
                    }
                }
                if tx.send(decode(job,&cancelled)).is_err() {break;}
                ui.request_repaint();
            }
        });
    }
    (jobs_tx, done_rx,cancelled)
}
