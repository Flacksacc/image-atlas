use std::{process::Command,time::Duration};

pub fn settings(dsn:&str)->anyhow::Result<(postgres::Config,postgres_native_tls::MakeTlsConnector)>{
    let python=std::env::var("IMAGE_CLI_PYTHON").unwrap_or_else(|_|std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.venv/Scripts/python.exe").to_string_lossy().into_owned());
    let mut command=Command::new(python);command.args(["-m","image_atlas.viewer_connection"]);
    crate::actions::scrub_environment(&mut command);
    if let Ok(root)=std::env::var("IMAGE_CLI_MODULE_ROOT"){command.env("PYTHONPATH",root);}
    let output=crate::bridge::output_with_input(&mut command,Duration::from_secs(15),1024*1024,format!("{dsn}\n").into_bytes()).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(output.status.success(),"Native connection configuration failed; check SSL options and certificate/key files");
    let options:serde_json::Value=serde_json::from_slice(&output.stdout)?;
    let config=options["native_dsn"].as_str().ok_or_else(||anyhow::anyhow!("Missing connection settings"))?.parse::<postgres::Config>()?;
    let mut builder=native_tls::TlsConnector::builder();
    builder.danger_accept_invalid_hostnames(options["verify_hostname"].as_bool()==Some(false));
    if let Some(pem)=options["root_pem"].as_str(){
        builder.disable_built_in_roots(true);
        for block in pem.split_inclusive("-----END CERTIFICATE-----"){if block.contains("-----BEGIN CERTIFICATE-----"){
            builder.add_root_certificate(native_tls::Certificate::from_pem(block.trim().as_bytes())?);
        }}
    }
    if let (Some(cert),Some(key))=(options["certificate_pem"].as_str(),options["key_pem"].as_str()){
        builder.identity(native_tls::Identity::from_pkcs8(cert.as_bytes(),key.as_bytes())?);
    }
    Ok((config,postgres_native_tls::MakeTlsConnector::new(builder.build()?)))
}
