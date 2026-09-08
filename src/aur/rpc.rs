//! A read-only client for the AUR's RPC interface (`/rpc/v5`).
//!
//! # Why curl rather than an HTTP crate
//!
//! The tree has no HTTP client and every network fetch so far shells out to
//! `curl` — see the Warp and linux-tkg downloads in
//! [`crate::configure::packages`]. Adding `reqwest` for metadata lookups would
//! pull an async runtime and a TLS stack into a binary that is otherwise a
//! collection of process invocations, so this follows the existing pattern.
//!
//! Fetching sits behind [`HttpGet`] for the same reason `pkgdeps` puts pacman
//! behind `CmdExec`: the whole thing is then testable against canned JSON, with
//! no network and no `curl`, which is what lets these tests run in CI.
//!
//! # Safety of the request
//!
//! The URL is passed to `curl` as its own argv element via `Command`, so no
//! shell parses it and there is nothing to quote. Package names are still
//! validated against the AUR's own charset before being interpolated: a name
//! carrying `&` or `?` would otherwise silently change the query's meaning,
//! which is a correctness bug before it is a security one.
//!
//! Nothing here mutates system state, per the [`MetadataSource`] contract it
//! exists to serve.
//!
//! [`MetadataSource`]: crate::pkgdeps::source::MetadataSource

use crate::pkgdeps::model::{Dep, Package};
use crate::utils::error::{DeploytixError, Result};
use serde::Deserialize;
use std::process::Command;
use tracing::debug;

/// Base of the RPC interface. v5 is the current version.
pub const AUR_RPC_BASE: &str = "https://aur.archlinux.org/rpc/v5";

/// The repo name AUR packages carry in [`Package::repo`].
///
/// Deliberately not a real sync database name: it is how every consumer tells
/// "this must be built" from "pacman can install this".
pub const AUR_REPO: &str = "aur";

/// How many names go in one `info` request.
///
/// The endpoint takes many `arg[]` values, but the practical limit is URL
/// length rather than a documented count, so this stays well inside it. Names
/// are short; 50 is roughly 1 KB of query string.
const INFO_BATCH: usize = 50;

/// Seconds before giving up on the connection, and on the whole transfer.
///
/// A resolve blocks a worker thread and the user is waiting, so failing is
/// better than hanging. Retries are left to the caller: a dependency preview
/// that takes 30 seconds has already failed as far as the user is concerned.
const CONNECT_TIMEOUT: u32 = 10;
const MAX_TIME: u32 = 30;

/// Something that can fetch a URL and return its body.
pub trait HttpGet: Send + Sync {
    fn get(&self, url: &str) -> Result<String>;
}

/// Production fetcher: `curl` in a subprocess.
#[derive(Debug, Default, Clone, Copy)]
pub struct CurlGet;

impl HttpGet for CurlGet {
    fn get(&self, url: &str) -> Result<String> {
        debug!("aur rpc: GET {url}");
        let output = Command::new("curl")
            .args([
                "-fsSL",
                "--connect-timeout",
                &CONNECT_TIMEOUT.to_string(),
                "--max-time",
                &MAX_TIME.to_string(),
                url,
            ])
            .output()
            .map_err(|e| DeploytixError::CommandFailed {
                command: "curl (AUR RPC)".to_string(),
                stderr: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(DeploytixError::CommandFailed {
                command: format!("curl (AUR RPC, exit {})", output.status),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Whether `name` is a legal AUR package name.
///
/// Arch's own rule: alphanumerics plus `@ . _ + -`, not starting with a hyphen
/// or dot. Anything else cannot be a package, so rejecting it early avoids both
/// a pointless request and a malformed query.
pub fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-'))
}

/// One result row from `/rpc/v5/info`.
///
/// Only the fields that map onto [`Package`]; the endpoint returns more
/// (maintainer, vote counts, timestamps) that dependency resolution never uses.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InfoResult {
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "URL")]
    url: Option<String>,
    #[serde(default)]
    license: Vec<String>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    depends: Vec<String>,
    #[serde(default)]
    make_depends: Vec<String>,
    #[serde(default)]
    check_depends: Vec<String>,
    #[serde(default)]
    opt_depends: Vec<String>,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    conflicts: Vec<String>,
    #[serde(default)]
    replaces: Vec<String>,
}

/// The envelope every v5 response comes in.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
struct RpcResponse {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    results: Vec<InfoResult>,
}

impl InfoResult {
    fn into_package(self) -> Package {
        let mut pkg = Package::new(self.name, self.version, AUR_REPO.to_string());
        pkg.description = self.description.unwrap_or_default();
        pkg.url = self.url.unwrap_or_default();
        pkg.licenses = self.license;
        pkg.groups = self.groups;
        pkg.depends = parse_deps(&self.depends);
        pkg.makedepends = parse_deps(&self.make_depends);
        pkg.checkdepends = parse_deps(&self.check_depends);
        pkg.optdepends = parse_deps(&self.opt_depends);
        pkg.provides = parse_deps(&self.provides);
        pkg.conflicts = parse_deps(&self.conflicts);
        pkg.replaces = parse_deps(&self.replaces);
        pkg
    }
}

/// AUR dep tokens use the same syntax as pacman's, so the existing parser
/// handles version constraints and optdepend descriptions unchanged.
fn parse_deps(tokens: &[String]) -> Vec<Dep> {
    tokens.iter().map(|t| Dep::parse(t)).collect()
}

/// Client for the subset of the RPC this needs.
pub struct AurRpc<H: HttpGet> {
    http: H,
    base: String,
}

impl<H: HttpGet> AurRpc<H> {
    pub fn new(http: H) -> Self {
        Self {
            http,
            base: AUR_RPC_BASE.to_string(),
        }
    }

    /// Point at a different base URL. Tests use it; so could a mirror.
    pub fn with_base(http: H, base: impl Into<String>) -> Self {
        Self {
            http,
            base: base.into(),
        }
    }

    /// The `info` URL for one batch of names.
    fn info_url(&self, names: &[&str]) -> String {
        let args: Vec<String> = names.iter().map(|n| format!("arg[]={n}")).collect();
        format!("{}/info?{}", self.base, args.join("&"))
    }

    /// Look up many packages at once.
    ///
    /// Names the AUR does not have are simply absent from the result, which is
    /// how the endpoint reports them — there is no per-name error. Invalid
    /// names are dropped before the request rather than being sent.
    pub fn info(&self, names: &[&str]) -> Result<Vec<Package>> {
        let valid: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| is_valid_package_name(n))
            .collect();
        if valid.is_empty() {
            return Ok(Vec::new());
        }

        let mut packages = Vec::new();
        for chunk in valid.chunks(INFO_BATCH) {
            let body = self.http.get(&self.info_url(chunk))?;
            packages.extend(parse_info_response(&body)?);
        }
        Ok(packages)
    }

    /// Packages whose `provides` includes `virtual_name`.
    ///
    /// Distinct from [`Self::info`]: a virtual name is not a package name, so
    /// `info` would return nothing for it.
    pub fn providers_of(&self, virtual_name: &str) -> Result<Vec<Package>> {
        if !is_valid_package_name(virtual_name) {
            return Ok(Vec::new());
        }
        let url = format!("{}/search/{virtual_name}?by=provides", self.base);
        let body = self.http.get(&url)?;
        parse_info_response(&body)
    }
}

/// Parse a v5 response envelope into packages.
///
/// An `error` type is surfaced as an error rather than an empty result: "the
/// AUR said no" and "the AUR is unreachable" both need to reach the user, and
/// silently returning nothing would render as "package does not exist".
pub fn parse_info_response(body: &str) -> Result<Vec<Package>> {
    let parsed: RpcResponse =
        serde_json::from_str(body).map_err(|e| DeploytixError::CommandFailed {
            command: "AUR RPC response".to_string(),
            stderr: format!("could not parse: {e}"),
        })?;

    if parsed.r#type == "error" {
        return Err(DeploytixError::CommandFailed {
            command: "AUR RPC".to_string(),
            stderr: parsed
                .error
                .unwrap_or_else(|| "unspecified error".to_string()),
        });
    }

    Ok(parsed
        .results
        .into_iter()
        .map(InfoResult::into_package)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers with canned bodies and records the URLs it was asked for.
    struct CannedHttp {
        body: String,
        calls: Mutex<Vec<String>>,
    }

    impl CannedHttp {
        fn new(body: &str) -> Self {
            Self {
                body: body.to_string(),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn urls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl HttpGet for CannedHttp {
        fn get(&self, url: &str) -> Result<String> {
            self.calls.lock().unwrap().push(url.to_string());
            Ok(self.body.clone())
        }
    }

    struct FailingHttp;
    impl HttpGet for FailingHttp {
        fn get(&self, _url: &str) -> Result<String> {
            Err(DeploytixError::CommandFailed {
                command: "curl".into(),
                stderr: "no network".into(),
            })
        }
    }

    const HHD_INFO: &str = r#"{
      "resultcount": 1,
      "results": [{
        "Name": "hhd-git",
        "Version": "3.1.3-1",
        "Description": "Handheld Daemon",
        "URL": "https://github.com/hhd-dev/hhd",
        "License": ["GPL-3.0-or-later"],
        "Depends": ["python>=3.10", "python-evdev", "hhd-ui"],
        "MakeDepends": ["git", "python-build"],
        "OptDepends": ["adjustor: TDP control"],
        "Provides": ["hhd"],
        "Conflicts": ["hhd"]
      }],
      "type": "multiinfo",
      "version": 5
    }"#;

    #[test]
    fn info_maps_every_dependency_class_onto_the_package_model() {
        let pkgs = parse_info_response(HHD_INFO).unwrap();
        assert_eq!(pkgs.len(), 1);
        let p = &pkgs[0];
        assert_eq!(p.name, "hhd-git");
        assert_eq!(p.version, "3.1.3-1");
        assert_eq!(p.repo, AUR_REPO, "AUR packages must be distinguishable");
        assert_eq!(p.depends.len(), 3);
        assert_eq!(p.makedepends.len(), 2);
        assert_eq!(p.provides[0].name, "hhd");
        assert_eq!(p.conflicts[0].name, "hhd");
    }

    #[test]
    fn version_constraints_survive_the_mapping() {
        let p = &parse_info_response(HHD_INFO).unwrap()[0];
        let python = p.depends.iter().find(|d| d.name == "python").unwrap();
        assert_eq!(python.constraint.as_deref(), Some(">=3.10"));
    }

    #[test]
    fn optdepend_descriptions_survive_the_mapping() {
        let p = &parse_info_response(HHD_INFO).unwrap()[0];
        let adj = p.optdepends.iter().find(|d| d.name == "adjustor").unwrap();
        assert_eq!(adj.description.as_deref(), Some("TDP control"));
    }

    #[test]
    fn a_package_the_aur_does_not_have_is_absent_not_an_error() {
        // The endpoint reports a miss by omission, not per-name errors.
        let empty = r#"{"resultcount":0,"results":[],"type":"multiinfo","version":5}"#;
        assert!(parse_info_response(empty).unwrap().is_empty());
    }

    #[test]
    fn an_error_envelope_becomes_an_error_not_an_empty_result() {
        // Otherwise "the AUR is broken" renders to the user as "no such package".
        let err = r#"{"type":"error","error":"Too many package results.","results":[]}"#;
        let e = parse_info_response(err).unwrap_err();
        assert!(format!("{e}").contains("Too many package results"));
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse_info_response("not json at all").is_err());
    }

    #[test]
    fn missing_optional_fields_do_not_break_the_mapping() {
        let minimal =
            r#"{"resultcount":1,"results":[{"Name":"x","Version":"1"}],"type":"multiinfo"}"#;
        let p = &parse_info_response(minimal).unwrap()[0];
        assert_eq!(p.name, "x");
        assert!(p.depends.is_empty());
        assert!(p.description.is_empty());
    }

    #[test]
    fn every_requested_name_reaches_the_query() {
        let http = CannedHttp::new(HHD_INFO);
        let rpc = AurRpc::with_base(http, "https://example.test/rpc/v5");
        rpc.info(&["hhd-git", "hhd-ui"]).unwrap();
        let url = &rpc.http.urls()[0];
        assert!(url.contains("arg[]=hhd-git"), "{url}");
        assert!(url.contains("arg[]=hhd-ui"), "{url}");
    }

    #[test]
    fn large_lookups_are_split_into_batches() {
        let http = CannedHttp::new(r#"{"resultcount":0,"results":[],"type":"multiinfo"}"#);
        let rpc = AurRpc::with_base(http, "https://example.test/rpc/v5");
        let names: Vec<String> = (0..120).map(|i| format!("pkg{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        rpc.info(&refs).unwrap();
        // 120 names at 50 per request.
        assert_eq!(rpc.http.urls().len(), 3);
    }

    #[test]
    fn names_that_cannot_be_packages_are_never_requested() {
        let http = CannedHttp::new(HHD_INFO);
        let rpc = AurRpc::with_base(http, "https://example.test/rpc/v5");
        rpc.info(&["good-name", "bad&name=x", "../../etc/passwd"])
            .unwrap();
        let url = &rpc.http.urls()[0];
        assert!(url.contains("arg[]=good-name"));
        assert!(!url.contains('&') || !url.contains("bad"), "{url}");
        assert!(!url.contains(".."), "{url}");
    }

    #[test]
    fn an_all_invalid_lookup_makes_no_request_at_all() {
        let http = CannedHttp::new(HHD_INFO);
        let rpc = AurRpc::with_base(http, "https://example.test/rpc/v5");
        assert!(rpc.info(&["!!!", ""]).unwrap().is_empty());
        assert!(rpc.http.urls().is_empty());
    }

    #[test]
    fn package_name_validation_matches_the_aur_charset() {
        for ok in [
            "yay",
            "hhd-git",
            "python-evdev",
            "gtk+",
            "a.b_c@d",
            "lib32-glibc",
        ] {
            assert!(is_valid_package_name(ok), "{ok} should be valid");
        }
        for bad in [
            "",
            "-leading",
            ".hidden",
            "has space",
            "amp&",
            "q?x",
            "sl/ash",
        ] {
            assert!(!is_valid_package_name(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn providers_query_uses_the_provides_index_not_info() {
        let http = CannedHttp::new(HHD_INFO);
        let rpc = AurRpc::with_base(http, "https://example.test/rpc/v5");
        rpc.providers_of("hhd").unwrap();
        let url = &rpc.http.urls()[0];
        assert!(url.contains("/search/hhd"), "{url}");
        assert!(url.contains("by=provides"), "{url}");
    }

    #[test]
    fn a_network_failure_surfaces_rather_than_looking_like_a_missing_package() {
        let rpc = AurRpc::new(FailingHttp);
        let e = rpc.info(&["hhd-git"]).unwrap_err();
        assert!(format!("{e}").contains("no network"));
    }

    /// Exercise the real [`CurlGet`] against a local HTTP server.
    ///
    /// Every other test here mocks [`HttpGet`], which means the curl
    /// invocation, the URL it is handed and the parse of a real HTTP response
    /// are otherwise never executed. Bound to loopback, so it needs no
    /// outbound network — it is skipped only if `curl` itself is absent.
    #[test]
    fn curl_fetches_and_parses_a_real_http_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        if Command::new("curl").arg("--version").output().is_err() {
            eprintln!("skipping: curl not installed");
            return;
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = HHD_INFO;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes());
            request
        });

        let rpc = AurRpc::with_base(CurlGet, format!("http://127.0.0.1:{port}"));
        let packages = rpc.info(&["hhd-git"]).expect("curl should succeed");

        let request = server.join().expect("server thread");
        assert!(
            request.contains("arg[]=hhd-git"),
            "the name never reached the wire: {request}"
        );
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "hhd-git");
        assert_eq!(packages[0].repo, AUR_REPO);
        assert_eq!(packages[0].depends.len(), 3);
    }
}
