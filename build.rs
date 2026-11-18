const CGI_IDENTITY_ENV: &str = "SYNORECLAIM_CGI_SCRIPT_FILENAME";

fn main() {
    println!("cargo:rerun-if-env-changed={CGI_IDENTITY_ENV}");

    let Ok(path) = std::env::var(CGI_IDENTITY_ENV) else {
        return;
    };
    if !valid_cgi_identity(&path) {
        panic!("{CGI_IDENTITY_ENV} must be a canonical absolute /cgi/api.cgi path");
    }

    println!("cargo:rustc-env={CGI_IDENTITY_ENV}={path}");
}

fn valid_cgi_identity(path: &str) -> bool {
    path.starts_with('/')
        && path.ends_with("/cgi/api.cgi")
        && !path.contains("//")
        && path
            .split('/')
            .all(|component| component != "." && component != "..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::valid_cgi_identity;

    #[test]
    fn only_the_canonical_transport_is_valid() {
        assert!(valid_cgi_identity(
            "/usr/syno/synoman/webman/3rdparty/SynoReclaim/cgi/api.cgi"
        ));
        assert!(!valid_cgi_identity(
            "/usr/syno/synoman/webman/3rdparty/SynoReclaim/cgi/SynoReclaim.cgi"
        ));
    }

    #[test]
    fn noncanonical_or_unrecognized_transport_is_rejected() {
        for path in [
            "cgi/api.cgi",
            "/cgi/other.cgi",
            "/cgi//api.cgi",
            "/cgi/../api.cgi",
            "/cgi/./api.cgi",
            "/cgi/api.cgi?x=1",
        ] {
            assert!(!valid_cgi_identity(path), "accepted {path}");
        }
    }
}
