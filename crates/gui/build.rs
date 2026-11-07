#[cfg(target_os = "windows")]
fn main() {
    println!("cargo:rerun-if-changed=windows/supersprinter.rc");
    println!("cargo:rerun-if-changed=../../assets/supersprinter.ico");
    let _ = embed_resource::compile("windows/supersprinter.rc", embed_resource::NONE);
}

#[cfg(not(target_os = "windows"))]
fn main() {}
