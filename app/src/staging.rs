//! A `root.run` answer too large for the reply: a file the executor wrote
//! in the package's staging directory, which the App reads and removes.

use super::internal;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use syno_pkg_land_app::Error;

/// The policy's `run.max_output`.
const MAX_OUTPUT: u64 = 64 << 20;

/// Reads and removes `name`, the bare name of a staging file of this
/// package; the executor answers the name and the client joins the folder.
pub fn take(data_dir: Option<&Path>, name: &str) -> Result<String, Error> {
    let staged = name
        .strip_prefix("root-")
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if !staged {
        eprintln!("staging is not a staging name: {name}");
        return Err(internal());
    }
    let path = data_dir
        .ok_or_else(internal)?
        .join("executor-staging")
        .join(name);
    let read = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .and_then(|file| {
            let mut text = String::new();
            file.take(MAX_OUTPUT)
                .read_to_string(&mut text)
                .map(|_| text)
        });
    let removed = std::fs::remove_file(&path);
    let text = read.map_err(|e| {
        eprintln!("staging {}: {e}", path.display());
        internal()
    })?;
    if let Err(e) = removed {
        eprintln!("staging {}: {e}", path.display());
    }
    Ok(text)
}
