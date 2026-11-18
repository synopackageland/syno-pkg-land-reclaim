fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = synoreclaim_app::hook(&args) {
        std::process::exit(code);
    }
    let data_dir = std::env::var_os("SYNOPKGLAND_DATA_DIR").map(Into::into);
    if let Err(e) = synoreclaim_app::app(data_dir).run() {
        eprintln!("synoreclaim-app: {e}");
        std::process::exit(1);
    }
}
