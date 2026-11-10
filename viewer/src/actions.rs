use std::path::Path;
use std::process::Command;

pub fn scrub_environment(command: &mut Command) {
    for key in ["ASSET_CLI_DSN", "ASSET_CLI_VIEWER_DSN", "IMAGE_CLI_DSN", "IMAGEATLAS_DB_PASSWORD", "IMAGE_CLI_VIEWER_CONTEXT", "IMAGE_CLI_CATALOG_ID", "IMAGE_CLI_ALLOWED_ROOTS", "IMAGE_CLI_READONLY", "PGPASSWORD", "PGPASSFILE", "PGSERVICE", "PGSERVICEFILE", "PGSSLKEY", "PGSSLCERT", "PGSSLPASSWORD"] {
        command.env_remove(key);
    }
    for (key,_) in std::env::vars_os(){if key.to_string_lossy().to_ascii_uppercase().starts_with("PG"){command.env_remove(key);}}
}

#[cfg(test)]
mod catalog_environment_tests {
    use super::*;
    #[test]
    fn external_helpers_cannot_inherit_either_catalog_connection() {
        let mut command=Command::new("external-editor");
        for key in ["ASSET_CLI_DSN","ASSET_CLI_VIEWER_DSN","IMAGE_CLI_DSN", "IMAGEATLAS_DB_PASSWORD","IMAGE_CLI_VIEWER_CONTEXT","PGPASSWORD"] {
            command.env(key,"private fixture");
        }
        scrub_environment(&mut command);
        let environment:std::collections::HashMap<_,_>=command.get_envs().collect();
        for key in ["ASSET_CLI_DSN","ASSET_CLI_VIEWER_DSN","IMAGE_CLI_DSN", "IMAGEATLAS_DB_PASSWORD","IMAGE_CLI_VIEWER_CONTEXT","PGPASSWORD"] {
            assert_eq!(environment.get(std::ffi::OsStr::new(key)),Some(&None));
        }
    }
}

pub fn valid_editor(path:&Path)->bool {
    if !path.is_file(){return false;}
    #[cfg(windows)] {path.extension().is_some_and(|ext|ext.eq_ignore_ascii_case("exe")||ext.eq_ignore_ascii_case("com"))}
    #[cfg(not(windows))] {use std::os::unix::fs::PermissionsExt;path.metadata().is_ok_and(|m|m.permissions().mode()&0o111!=0)}
}

#[cfg(windows)]
fn shell_execute(verb: &str, path: &Path) -> Result<(), String> {
    let python=std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|Path::new(env!("CARGO_MANIFEST_DIR")).join("../.venv/Scripts/python.exe").to_string_lossy().into_owned());
    let mut command=Command::new(python);scrub_environment(&mut command);
    command.args(["-m","image_atlas.viewer_open",verb]).arg(path);
    if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
    let output=crate::bridge::output_detached(&mut command,std::time::Duration::from_secs(15),65536)?;
    if output.status.success(){Ok(())}else{Err(String::from_utf8_lossy(&output.stderr).trim().into())}

}

pub fn open_original(path: &Path) -> Result<(), String> {
    #[cfg(windows)] { shell_execute("open", path) }
    #[cfg(not(windows))] { let _ = path; Err("External opening requires Windows".into()) }
}

pub fn open_with(path: &Path) -> Result<(), String> {
    #[cfg(windows)] { shell_execute("openas", path) }
    #[cfg(not(windows))] { let _ = path; Err("Open with requires Windows".into()) }
}

pub fn open_folder(path: &Path) -> Result<(), String> {
    #[cfg(windows)] { shell_execute("open", path) }
    #[cfg(not(windows))] { let _ = path; Err("Explorer requires Windows".into()) }
}

pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        let mut command=Command::new("explorer.exe");scrub_environment(&mut command);
        command.arg(format!("/select,{}", path.display()))
            .creation_flags(0x08000000)
            .spawn().map(|_| ()).map_err(|error| error.to_string())
    }
    #[cfg(not(windows))] { let _ = path; Err("Explorer requires Windows".into()) }
}

pub fn open_editor(executable: &Path, image: &Path) -> Result<(), String> {
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new(executable);
        scrub_environment(&mut command);
        command.arg(image).creation_flags(0x08000000)
            .spawn().map(|_| ()).map_err(|error| error.to_string())
    }
    #[cfg(not(windows))] { let _ = (executable, image); Err("Editors require Windows".into()) }
}
