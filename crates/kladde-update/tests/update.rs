use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use kladde_update::{check, install_to, platform, Release, UpdateError};
use sha2::{Digest, Sha256};

/// Listener on a free 127.0.0.1 port and its base URL; the port must be known before routes are built.
fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    (listener, base)
}

/// Serves `routes` (path -> body) over HTTP/1.1 for the rest of the test process.
fn serve(listener: TcpListener, routes: Vec<(String, Vec<u8>)>) {
    let routes: HashMap<String, Vec<u8>> = routes.into_iter().collect();
    thread::spawn(move || listener.incoming().flatten().for_each(|stream| respond(stream, &routes)));
}

fn respond(mut stream: TcpStream, routes: &HashMap<String, Vec<u8>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request = String::new();
    reader.read_line(&mut request).expect("request line");
    let mut header = String::from("-");
    while !header.trim_end().is_empty() {
        header.clear();
        reader.read_line(&mut header).expect("header");
    }
    let path = request.split_whitespace().nth(1).unwrap_or("");
    let (status, body) = routes.get(path).map_or(("404 Not Found", &[][..]), |b| ("200 OK", &b[..]));
    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    // The client may hang up early (cancel test); nothing to do about it here.
    let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body));
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn payload() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

fn manifest(version: &str, platform_key: &str, url: &str, sha: &str) -> Vec<u8> {
    format!(
        r#"{{ "version": "{version}", "notes": "Fixes.", "pub_date": "2026-10-22T12:00:00Z",
             "platforms": {{ "{platform_key}": {{ "url": "{url}", "sha256": "{sha}" }} }} }}"#
    )
    .into_bytes()
}

fn current_platform() -> &'static str {
    platform().expect("tests run on linux-x86_64 or windows-x86_64")
}

/// Server with a 0.2.0 manifest for this platform and the matching payload.
fn release_server() -> (String, Release) {
    let body = payload();
    let sha = sha256_hex(&body);
    let base = serve_with_manifest(body, &sha, "0.2.0", current_platform());
    let release =
        Release { version: "0.2.0".into(), notes: "Fixes.".into(), url: format!("{base}/kladde"), sha256: sha };
    (base, release)
}

fn serve_with_manifest(body: Vec<u8>, sha: &str, version: &str, platform_key: &str) -> String {
    let (listener, base) = bind();
    let manifest = manifest(version, platform_key, &format!("{base}/kladde"), sha);
    serve(listener, vec![("/latest.json".into(), manifest), ("/kladde".into(), body)]);
    base
}

fn fake_exe(dir: &Path) -> PathBuf {
    let exe = dir.join(if cfg!(windows) { "kladde.exe" } else { "kladde" });
    fs::write(&exe, b"old executable").expect("write exe");
    exe
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(dir).expect("read dir").map(|e| e.expect("entry").file_name().to_string_lossy().into()).collect();
    names.sort();
    names
}

fn always(_: u64, _: Option<u64>) -> bool {
    true
}

#[test]
fn check_finds_newer_release() {
    let (base, expected) = release_server();
    let found = check(&format!("{base}/latest.json"), "0.1.0").expect("check");
    assert_eq!(found, Some(expected.clone()));
    let found = check(&format!("{base}/latest.json"), "v0.1.9").expect("check");
    assert_eq!(found, Some(expected));
}

#[test]
fn check_same_or_older_is_none() {
    let (base, _) = release_server();
    for current in ["0.2.0", "v0.2.0", "0.2.1", "0.10.0", "1.0.0"] {
        assert_eq!(check(&format!("{base}/latest.json"), current).expect("check"), None, "{current}");
    }
}

#[test]
fn check_missing_platform_is_none() {
    let body = payload();
    let base = serve_with_manifest(body.clone(), &sha256_hex(&body), "0.2.0", "plan9-mips");
    assert_eq!(check(&format!("{base}/latest.json"), "0.1.0").expect("check"), None);
}

#[test]
fn check_malformed_versions_error() {
    let (base, _) = release_server();
    let url = format!("{base}/latest.json");
    assert!(matches!(check(&url, "0.1"), Err(UpdateError::InvalidVersion(_))));
    assert!(matches!(check(&url, "latest"), Err(UpdateError::InvalidVersion(_))));
    let body = payload();
    let bad = serve_with_manifest(body.clone(), &sha256_hex(&body), "0.2.0-beta", current_platform());
    assert!(matches!(check(&format!("{bad}/latest.json"), "0.1.0"), Err(UpdateError::InvalidVersion(_))));
}

#[test]
fn check_reports_http_errors_and_bad_json() {
    let (listener, base) = bind();
    serve(listener, vec![("/latest.json".into(), b"not json".to_vec())]);
    assert!(matches!(check(&format!("{base}/latest.json"), "0.1.0"), Err(UpdateError::Manifest(_))));
    assert!(matches!(check(&format!("{base}/missing.json"), "0.1.0"), Err(UpdateError::Http(404))));
}

#[test]
fn install_replaces_exe() {
    let (_, release) = release_server();
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = fake_exe(dir.path());
    let last = Cell::new((0u64, None));
    let path = install_to(&release, &exe, &|done, total| {
        last.set((done, total));
        true
    })
    .expect("install");
    assert_eq!(path, exe);
    assert_eq!(fs::read(&exe).expect("read exe"), payload());
    assert_eq!(last.get(), (payload().len() as u64, Some(payload().len() as u64)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&exe).expect("metadata").permissions().mode() & 0o777, 0o755);
        assert_eq!(dir_names(dir.path()), ["kladde"]);
    }
    #[cfg(windows)]
    {
        assert_eq!(fs::read(dir.path().join("kladde.exe.old")).expect("old"), b"old executable");
        assert_eq!(dir_names(dir.path()), ["kladde.exe", "kladde.exe.old"]);
    }
}

#[test]
fn install_checksum_mismatch_leaves_exe_untouched() {
    let (_, mut release) = release_server();
    release.sha256 = sha256_hex(b"something else");
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = fake_exe(dir.path());
    let err = install_to(&release, &exe, &always).expect_err("mismatch");
    assert!(matches!(err, UpdateError::Checksum { .. }), "{err}");
    assert_eq!(fs::read(&exe).expect("read exe"), b"old executable");
    assert_eq!(dir_names(dir.path()), [exe.file_name().expect("name").to_string_lossy()]);
}

#[test]
fn install_cancel_leaves_exe_untouched() {
    let (_, release) = release_server();
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = fake_exe(dir.path());
    let err = install_to(&release, &exe, &|done, _| done < 70_000).expect_err("cancel");
    assert!(matches!(err, UpdateError::Cancelled), "{err}");
    assert_eq!(fs::read(&exe).expect("read exe"), b"old executable");
    assert_eq!(dir_names(dir.path()), [exe.file_name().expect("name").to_string_lossy()]);
}

#[test]
fn install_into_missing_dir_names_it() {
    let (_, release) = release_server();
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing");
    let err = install_to(&release, &missing.join("kladde"), &always).expect_err("not writable");
    assert!(matches!(&err, UpdateError::NotWritable { dir, .. } if *dir == missing), "{err}");
    assert!(err.to_string().contains(&missing.display().to_string()));
}

#[test]
fn install_rejects_insecure_url() {
    let release = Release {
        version: "0.2.0".into(),
        notes: String::new(),
        url: "http://example.org/kladde".into(),
        sha256: sha256_hex(b""),
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = fake_exe(dir.path());
    assert!(matches!(install_to(&release, &exe, &always), Err(UpdateError::InsecureUrl(_))));
    assert_eq!(fs::read(&exe).expect("read exe"), b"old executable");
}
