fn main() {
    let icon = "../src/image_atlas/branding/image-atlas.ico";
    println!("cargo:rerun-if-changed={icon}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon(icon)
            .set("ProductName", "ImageAtlas")
            .set("FileDescription", "ImageAtlas image library viewer")
            .compile()
            .expect("Could not embed the ImageAtlas Windows icon");
    }
}
