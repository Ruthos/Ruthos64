//! DICOMweb against a small server that behaves as each test needs, including
//! badly. It speaks plain HTTP on localhost, or TLS with a throwaway certificate.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ruthos_net::dicomweb::{
    check_address, parse_pin, public_key_info, test_connection, AccessToken, Basic, NoSignIn, Oidc, OidcTokens,
    ServiceToken, SignIn, Transfer, WebClient, WebNode,
};
use ruthos_net::{NetError, RetrieveProgress, StudyQuery};
use serde_json::json;
use sha2::{Digest, Sha256};

/// A request as the test server saw it.
#[derive(Debug, Clone, Default)]
struct Seen {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Seen {
    fn parameter(&self, key: &str) -> Vec<String> {
        url::form_urlencoded::parse(self.query.as_bytes())
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.to_string())
            .collect()
    }
    fn header(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(String::as_str)
    }
}

/// What to answer: status line, headers and body.
#[derive(Clone, Default)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn reply(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status,
        headers: vec![("Content-Type".into(), content_type.into())],
        body: body.into(),
    }
}

fn json_reply(value: serde_json::Value) -> Reply {
    reply(200, "application/dicom+json", serde_json::to_vec(&value).unwrap())
}

type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

struct Server {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn read_request(stream: impl Read) -> Option<Seen> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut words = line.split_whitespace();
    let method = words.next()?.to_string();
    let target = words.next()?.to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut seen = Seen {
        method,
        path: path.to_string(),
        query: query.to_string(),
        ..Seen::default()
    };
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (key, value) = header.split_once(':')?;
        seen.headers
            .insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let length: usize = seen.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    seen.body = body;
    Some(seen)
}

fn write_reply(stream: &mut impl Write, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (key, value) in &reply.headers {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

fn serve(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let handler: Handler = Arc::new(handler);
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (handler, log) = (handler.clone(), log.clone());
            std::thread::spawn(move || {
                let Ok(clone) = stream.try_clone() else { return };
                if let Some(request) = read_request(clone) {
                    log.lock().unwrap().push(request.clone());
                    let mut stream: TcpStream = stream;
                    let _ = write_reply(&mut stream, &handler(&request));
                }
            });
        }
    });
    Server { address, seen }
}

impl Server {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.address.port())
    }
    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn node_with(url: &str, sign_in: Arc<dyn SignIn>) -> WebNode {
    WebNode {
        name: "Test".into(),
        qido: url.to_string(),
        wado: url.to_string(),
        stow: url.to_string(),
        sign_in,
        pin: None,
        client_certificate: None,
        proxy: None,
        transfer: Transfer::Original,
        page_size: 100,
        include_all: None,
        parallel: 4,
        most_parts: ruthos_net::dicomweb::multipart::MOST_PARTS,
    }
}

fn client(server: &Server) -> WebClient {
    WebClient::new(node_with(&server.url(), Arc::new(NoSignIn))).unwrap()
}

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn study(uid: &str, name: &str) -> serde_json::Value {
    json!({
        "0020000D": {"vr": "UI", "Value": [uid]},
        "00100010": {"vr": "PN", "Value": [{"Alphabetic": name}]},
        "00080020": {"vr": "DA", "Value": ["20261105"]},
    })
}

fn series_json(uid: &str, instances: u32) -> serde_json::Value {
    json!({
        "0020000E": {"vr": "UI", "Value": [uid]},
        "00200011": {"vr": "IS", "Value": [1]},
        "00080060": {"vr": "CS", "Value": ["CT"]},
        "00201209": {"vr": "IS", "Value": [instances]},
    })
}

fn instance_json(uid: &str) -> serde_json::Value {
    json!({"00080018": {"vr": "UI", "Value": [uid]}})
}

/// A multipart response of `count` parts, each `size` bytes of `fill`.
fn multipart(count: usize, size: usize, fill: u8) -> Reply {
    let mut body = Vec::new();
    for _ in 0..count {
        body.extend_from_slice(b"--XYZ\r\nContent-Type: application/dicom\r\n\r\n");
        body.extend(std::iter::repeat_n(fill, size));
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--XYZ--\r\n");
    reply(200, "multipart/related; type=\"application/dicom\"; boundary=XYZ", body)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ruthos-web-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect()
}

fn retrieve(client: &WebClient, out: &Path) -> Result<ruthos_net::RetrieveOutcome, NetError> {
    client.retrieve(
        "S",
        None,
        out,
        &|_| false,
        &|_| false,
        &|_: RetrieveProgress| {},
        &never(),
    )
}

/// A server with one series of `count` instances.
fn one_series(count: u32, fill: u8) -> Server {
    serve(move |r| match r.path.as_str() {
        "/studies/S/series" => json_reply(json!([series_json("E", count)])),
        "/studies/S/series/E" => multipart(count as usize, 300, fill),
        _ => reply(404, "text/plain", "no"),
    })
}

#[test]
fn a_search_sends_the_fields_and_reads_the_results() {
    let server = serve(|_| json_reply(json!([study("1.1", "SMITH^Bella")])));
    let q = StudyQuery {
        patient: "bella".into(),
        study_date: "20260101-20261231".into(),
        modality: "CT".into(),
        ..StudyQuery::default()
    };
    let found = client(&server).find_studies(&q, &never()).unwrap();
    assert_eq!(found.studies.len(), 1, "the name and ID searches are merged");
    assert_eq!(found.studies[0].patient_name, "SMITH^Bella");
    assert!(!found.stopped_early);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let name = &requests[0];
    assert_eq!(name.path, "/studies");
    assert_eq!(name.parameter("PatientName"), ["*bella*"]);
    assert_eq!(name.parameter("fuzzymatching"), ["false"]);
    assert_eq!(name.parameter("StudyDate"), ["20260101-20261231"]);
    assert_eq!(name.parameter("ModalitiesInStudy"), ["CT"]);
    assert_eq!(name.parameter("limit"), ["100"]);
    assert_eq!(name.header("accept"), Some("application/dicom+json"));
    assert!(name.parameter("includefield").contains(&"00081030".to_string()));
    assert_eq!(requests[1].parameter("PatientID"), ["bella"]);
}

#[test]
fn pages_are_followed_and_a_server_ignoring_limit_is_stopped() {
    let paged = serve(|r| {
        let offset: usize = r.parameter("offset")[0].parse().unwrap();
        let items: Vec<_> = (offset..(offset + 10).min(25))
            .map(|i| study(&format!("1.{i}"), "A"))
            .collect();
        json_reply(json!(items))
    });
    let mut node = node_with(&paged.url(), Arc::new(NoSignIn));
    node.page_size = 10;
    let found = WebClient::new(node)
        .unwrap()
        .find_studies(&StudyQuery::default(), &never())
        .unwrap();
    assert_eq!(found.studies.len(), 25);
    assert_eq!(paged.requests().len(), 3);

    let flood = serve(|_| {
        let items: Vec<_> = (0..1500).map(|i| study(&format!("2.{i}"), "A")).collect();
        json_reply(json!(items))
    });
    let found = client(&flood).find_studies(&StudyQuery::default(), &never()).unwrap();
    assert_eq!(found.studies.len(), 1000);
    assert!(found.stopped_early, "the person is told to narrow the search");

    let same_page = serve(|_| {
        let items: Vec<_> = (0..10).map(|i| study(&format!("3.{i}"), "A")).collect();
        json_reply(json!(items))
    });
    let mut node = node_with(&same_page.url(), Arc::new(NoSignIn));
    node.page_size = 10;
    let found = WebClient::new(node)
        .unwrap()
        .find_studies(&StudyQuery::default(), &never())
        .unwrap();
    assert_eq!(found.studies.len(), 10, "ignoring offset must not loop for ever");
}

#[test]
fn an_oversized_search_is_stopped() {
    let padding = "x".repeat(65 * 1024 * 1024);
    let server = serve(move |_| reply(200, "application/dicom+json", format!("[\"{padding}\"]")));
    let result = client(&server).find_studies(&StudyQuery::default(), &never());
    assert!(matches!(result, Err(NetError::TooLarge(_))), "{result:?}");
}

#[test]
fn every_part_is_stored_under_a_generated_name() {
    let server = serve(|r| match r.path.as_str() {
        "/studies/S/series" => json_reply(json!([series_json("E", 1)])),
        _ => {
            let body = b"--XYZ\r\nContent-Type: application/dicom\r\n\
                Content-Location: ..\\..\\Windows\\evil.exe\r\n\
                Content-Disposition: attachment; filename=\"../../evil.dcm\"\r\n\r\ndata\r\n--XYZ--\r\n";
            reply(
                200,
                "multipart/related; type=\"application/dicom\"; boundary=XYZ",
                body.to_vec(),
            )
        }
    });
    let parent = temp_dir("names");
    let out = parent.join("INCOMING.noindex");
    let outcome = retrieve(&client(&server), &out).unwrap();
    assert_eq!(outcome.files.len(), 1);
    let name = outcome.files[0].file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("web-") && name.ends_with(".dcm"), "{name}");
    assert_eq!(outcome.files[0].parent().unwrap(), out);
    assert_eq!(std::fs::read(&outcome.files[0]).unwrap(), b"data");
    assert_eq!(files_in(&parent).len(), 1, "nothing escaped the folder");
    std::fs::remove_dir_all(parent).ok();
}

#[test]
fn a_refused_transfer_syntax_falls_back_to_the_original() {
    let server = serve(|r| match r.path.as_str() {
        "/studies/S/series" => json_reply(json!([series_json("E", 2)])),
        _ if r.header("accept").is_some_and(|a| a.contains("transfer-syntax=*")) => multipart(2, 10, 1),
        _ => reply(406, "text/plain", "not that one"),
    });
    let mut node = node_with(&server.url(), Arc::new(NoSignIn));
    node.transfer = Transfer::Jpeg2000Lossless;
    let out = temp_dir("406");
    let outcome = retrieve(&WebClient::new(node).unwrap(), &out).unwrap();
    assert_eq!(outcome.files.len(), 2);
    let wado: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/E"))
        .collect();
    assert_eq!(wado.len(), 2);
    assert!(wado[0].header("accept").unwrap().contains("1.2.840.10008.1.2.4.90"));
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn a_series_begun_earlier_fetches_only_what_is_missing() {
    let server = serve(|r| match r.path.as_str() {
        "/studies/S/series" => json_reply(json!([series_json("E", 3)])),
        "/studies/S/series/E/instances" => {
            json_reply(json!([instance_json("I1"), instance_json("I2"), instance_json("I3")]))
        }
        p if p.starts_with("/studies/S/series/E/instances/") => multipart(1, 10, 2),
        _ => reply(500, "text/plain", "the whole series must not be asked for"),
    });
    let out = temp_dir("resume");
    let outcome = client(&server)
        .retrieve(
            "S",
            None,
            &out,
            &|uid| uid == "I1" || uid == "I3",
            &|_| true,
            &|_: RetrieveProgress| {},
            &never(),
        )
        .unwrap();
    assert_eq!(outcome.files.len(), 1);
    let asked: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.contains("/instances/"))
        .map(|r| r.path)
        .collect();
    assert_eq!(asked, ["/studies/S/series/E/instances/I2"]);
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn cancelling_keeps_what_arrived_and_leaves_no_half_file() {
    // Three whole parts, then a part that never ends.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let Ok(clone) = stream.try_clone() else { return };
                let Some(request) = read_request(clone) else { return };
                let mut stream = stream;
                if request.path.ends_with("/series") {
                    let _ = write_reply(&mut stream, &json_reply(json!([series_json("E", 5)])));
                    return;
                }
                let mut head = String::from(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\
                     Content-Type: multipart/related; type=\"application/dicom\"; boundary=XYZ\r\n\r\n",
                );
                let mut body = String::new();
                for _ in 0..3 {
                    body.push_str("--XYZ\r\nContent-Type: application/dicom\r\n\r\nimage\r\n");
                }
                body.push_str("--XYZ\r\nContent-Type: application/dicom\r\n\r\npartial");
                head.push_str(&format!("{:x}\r\n{body}\r\n", body.len()));
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.flush();
                std::thread::sleep(std::time::Duration::from_secs(30));
            });
        }
    });
    let client = WebClient::new(node_with(&format!("http://127.0.0.1:{port}"), Arc::new(NoSignIn))).unwrap();
    let out = temp_dir("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let stopper = cancel.clone();
    let done = AtomicUsize::new(0);
    let report = |p: RetrieveProgress| {
        done.store(p.completed as usize, Ordering::Relaxed);
        if p.completed >= 3 {
            stopper.store(true, Ordering::Relaxed);
        }
    };
    let result = client.retrieve("S", None, &out, &|_| false, &|_| false, &report, &cancel);
    assert_eq!(result, Err(NetError::Cancelled));
    let names: Vec<String> = files_in(&out)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names.iter().filter(|n| n.ends_with(".dcm")).count(), 3);
    assert_eq!(names.len(), 3, "no half-written part is left: {names:?}");
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn a_flood_of_parts_is_stopped_with_what_arrived_kept() {
    // The real limit is checked in the reader's tests; 100,000 files take a while.
    let server = serve(|r| match r.path.as_str() {
        "/studies/S/series" => json_reply(json!([series_json("E", 300)])),
        _ => multipart(300, 0, 0),
    });
    let mut node = node_with(&server.url(), Arc::new(NoSignIn));
    node.most_parts = 200;
    let out = temp_dir("flood");
    let result = retrieve(&WebClient::new(node).unwrap(), &out);
    assert!(matches!(result, Err(NetError::TooLarge(_))), "{result:?}");
    assert_eq!(files_in(&out).len(), 200);
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn large_series_do_not_sit_in_memory_and_arrive_whole() {
    let fill = 77;
    let server = one_series(50, fill);
    let out = temp_dir("whole");
    let outcome = retrieve(&client(&server), &out).unwrap();
    assert_eq!(outcome.files.len(), 50);
    assert_eq!(outcome.progress.completed, 50);
    assert_eq!(outcome.progress.remaining, Some(0));
    for file in &outcome.files {
        assert_eq!(std::fs::read(file).unwrap(), vec![fill; 300]);
    }
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn a_reply_cut_short_is_reported_and_the_broken_part_dropped() {
    let server = serve(|r| match r.path.as_str() {
        "/studies/S/series" => json_reply(json!([series_json("E", 2)])),
        _ => {
            let body = b"--XYZ\r\nContent-Type: application/dicom\r\n\r\ngood\r\n\
                --XYZ\r\nContent-Type: application/dicom\r\n\r\nbad"
                .to_vec();
            reply(200, "multipart/related; boundary=\"XYZ\"", body)
        }
    });
    let out = temp_dir("short");
    assert!(retrieve(&client(&server), &out).is_err());
    assert_eq!(files_in(&out).len(), 1);
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn sign_in_goes_out_as_each_method_says() {
    let check = |sign_in: Arc<dyn SignIn>, key: &'static str, value: &'static str| {
        let server = serve(|_| json_reply(json!([])));
        let client = WebClient::new(node_with(&server.url(), sign_in)).unwrap();
        client.find_series("S", &never()).unwrap();
        assert_eq!(server.requests()[0].header(key), Some(value), "{key}");
    };
    check(
        Arc::new(Basic {
            user: "a".into(),
            password: "b".into(),
        }),
        "authorization",
        "Basic YTpi",
    );
    check(Arc::new(AccessToken { token: "tok".into() }), "cf-access-token", "tok");
    check(
        Arc::new(ServiceToken {
            client_id: "id".into(),
            secret: "shh".into(),
        }),
        "cf-access-client-secret",
        "shh",
    );
}

fn tokens(token_endpoint: &str, expires_in: i64, refresh: Option<&str>) -> OidcTokens {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    OidcTokens {
        access: "old".into(),
        refresh: refresh.map(String::from),
        expires: (now + expires_in) as u64,
        token_endpoint: token_endpoint.to_string(),
    }
}

#[test]
fn an_expired_token_is_refreshed_once_and_quietly() {
    let provider = serve(|r| {
        assert!(String::from_utf8_lossy(&r.body).contains("grant_type=refresh_token"));
        reply(
            200,
            "application/json",
            r#"{"access_token":"new","expires_in":3600,"refresh_token":"r2"}"#,
        )
    });
    let gateway = serve(|r| {
        if r.header("authorization") == Some("Bearer new") {
            json_reply(json!([]))
        } else {
            reply(401, "text/plain", "no")
        }
    });
    let saved: Arc<Mutex<Vec<OidcTokens>>> = Arc::default();
    let log = saved.clone();
    let sign_in = Oidc::new(
        tokens(&format!("{}/token", provider.url()), 3600, Some("r1")),
        "ruthos",
        Arc::new(move |t: &OidcTokens| log.lock().unwrap().push(t.clone())),
    )
    .unwrap();
    let client = WebClient::new(node_with(&gateway.url(), Arc::new(sign_in))).unwrap();
    client.find_series("S", &never()).unwrap();
    assert_eq!(provider.requests().len(), 1, "one renewal");
    assert_eq!(gateway.requests().len(), 2, "one retry");
    let saved = saved.lock().unwrap();
    assert_eq!(saved[0].access, "new");
    assert_eq!(saved[0].refresh.as_deref(), Some("r2"), "kept for next time");
}

#[test]
fn a_token_about_to_expire_is_renewed_before_it_is_sent() {
    let provider = serve(|_| reply(200, "application/json", r#"{"access_token":"new","expires_in":3600}"#));
    let gateway = serve(|_| json_reply(json!([])));
    let sign_in = Oidc::new(
        tokens(&format!("{}/token", provider.url()), 10, Some("r1")),
        "ruthos",
        Arc::new(|_: &OidcTokens| {}),
    )
    .unwrap();
    let client = WebClient::new(node_with(&gateway.url(), Arc::new(sign_in))).unwrap();
    client.find_series("S", &never()).unwrap();
    assert_eq!(gateway.requests().len(), 1);
    assert_eq!(gateway.requests()[0].header("authorization"), Some("Bearer new"));
}

#[test]
fn a_revoked_refresh_token_asks_for_sign_in_and_403_is_not_permitted() {
    let provider = serve(|_| reply(400, "application/json", r#"{"error":"invalid_grant"}"#));
    let gateway = serve(|_| reply(401, "text/plain", "no"));
    let sign_in = Oidc::new(
        tokens(&format!("{}/token", provider.url()), 3600, Some("gone")),
        "ruthos",
        Arc::new(|_: &OidcTokens| {}),
    )
    .unwrap();
    let client = WebClient::new(node_with(&gateway.url(), Arc::new(sign_in))).unwrap();
    assert_eq!(client.find_series("S", &never()), Err(NetError::SignInNeeded));

    let forbidden = serve(|_| reply(403, "text/plain", "outside your scope"));
    assert_eq!(
        self::client(&forbidden).find_series("S", &never()),
        Err(NetError::NotPermitted)
    );
    let refused = serve(|_| reply(401, "text/plain", "no"));
    assert_eq!(
        self::client(&refused).find_series("S", &never()),
        Err(NetError::SignInNeeded)
    );
    assert_eq!(refused.requests().len(), 1, "no renewal, so no retry");
}

#[test]
fn access_login_pages_mean_signed_out_and_are_not_followed() {
    let redirected = serve(|_| Reply {
        status: 302,
        headers: vec![(
            "Location".into(),
            "https://team.cloudflareaccess.com/cdn-cgi/access/login/x".into(),
        )],
        body: Vec::new(),
    });
    let access = |server: &Server| {
        let sign_in = Arc::new(AccessToken { token: "t".into() });
        WebClient::new(node_with(&server.url(), sign_in)).unwrap()
    };
    assert_eq!(
        access(&redirected).find_series("S", &never()),
        Err(NetError::SignInNeeded)
    );

    let html = serve(|_| reply(200, "text/html; charset=utf-8", "<html>Sign in</html>"));
    assert_eq!(access(&html).find_series("S", &never()), Err(NetError::SignInNeeded));

    // Without Access in front, HTML is a wrong address, not a sign-in.
    assert!(matches!(
        client(&html).find_series("S", &never()),
        Err(NetError::NotDicomWeb(_))
    ));
}

#[test]
fn redirects_leave_neither_the_host_nor_https() {
    let elsewhere = serve(|_| Reply {
        status: 302,
        headers: vec![("Location".into(), "https://other.example.org/dicom-web/studies".into())],
        body: Vec::new(),
    });
    assert_eq!(
        client(&elsewhere).find_series("S", &never()),
        Err(NetError::RedirectRefused("other.example.org".into()))
    );
    assert_eq!(elsewhere.requests().len(), 1);
}

#[test]
fn addresses_are_refused_before_anything_is_sent() {
    assert!(check_address("http://gateway.example.org/dicom-web", true).is_err());
    assert!(check_address("https://gateway.example.org/dicom-web", false).is_err());
    assert!(check_address("http://localhost:8042/dicom-web", false).is_ok());
}

#[test]
fn sending_goes_in_batches_and_reports_what_was_refused() {
    let dir = temp_dir("stow");
    let files: Vec<PathBuf> = (0..120)
        .map(|i| {
            let path = dir.join(format!("{i}.dcm"));
            std::fs::write(&path, format!("image {i}")).unwrap();
            path
        })
        .collect();
    let batch = Arc::new(AtomicUsize::new(0));
    let counter = batch.clone();
    let server = serve(move |_| {
        if counter.fetch_add(1, Ordering::SeqCst) == 2 {
            let failed = json!({
                "00081198": {"vr": "SQ", "Value": [{
                    "00081155": {"vr": "UI", "Value": ["9.9"]},
                    "00081197": {"vr": "US", "Value": [290]},
                }]},
            });
            return reply(409, "application/dicom+json", serde_json::to_vec(&failed).unwrap());
        }
        json_reply(json!({}))
    });
    let outcome = client(&server).send_files(&files, &|_, _| {}, &never()).unwrap();
    assert_eq!(outcome.sent, 119);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].1, "the server does not take this kind of object");

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/studies");
    let content_type = requests[0].header("content-type").unwrap();
    assert!(content_type.starts_with("multipart/related; type=\"application/dicom\"; boundary="));
    let parts = |r: &Seen| {
        r.body
            .windows(b"Content-Type: application/dicom".len())
            .filter(|w| *w == b"Content-Type: application/dicom")
            .count()
    };
    assert_eq!(requests.iter().map(parts).collect::<Vec<_>>(), [50, 50, 20]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn the_connection_test_tells_problems_apart() {
    let good = serve(|r| {
        if r.parameter("includefield").is_empty() || r.parameter("includefield") == ["all"] {
            json_reply(json!([study("1", "A")]))
        } else {
            reply(400, "text/plain", "no")
        }
    });
    let (steps, include_all) = test_connection(&client(&good));
    assert!(steps.iter().all(|s| s.passed), "{steps:?}");
    assert_eq!(include_all, Some(true));

    let html = serve(|_| reply(200, "text/html", "<html>A web site</html>"));
    let (steps, _) = test_connection(&client(&html));
    assert_eq!(steps.len(), 3);
    assert!(steps[0].passed && steps[1].passed);
    assert!(!steps[2].passed, "reached and allowed in, but not DICOMweb");

    let refused = serve(|_| reply(401, "text/plain", "no"));
    let (steps, _) = test_connection(&client(&refused));
    assert_eq!(steps.len(), 2);
    assert!(steps[0].passed && !steps[1].passed, "a failed sign-in");

    let plain = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = plain.local_addr().unwrap().port();
    drop(plain);
    let gone = WebClient::new(node_with(&format!("http://127.0.0.1:{port}"), Arc::new(NoSignIn))).unwrap();
    let (steps, _) = test_connection(&gone);
    assert_eq!(steps.len(), 1);
    assert!(!steps[0].passed);
}

/// A TLS server with a throwaway certificate, answering any request with JSON.
fn tls_server() -> (u16, [u8; 32]) {
    let key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let der = key.cert.der().to_vec();
    let pin: [u8; 32] = Sha256::digest(public_key_info(&der).unwrap()).into();
    let chain = vec![rustls::pki_types::CertificateDer::from(der)];
    let private = rustls::pki_types::PrivateKeyDer::try_from(key.key_pair.serialize_der()).unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, private)
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let config = config.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let Ok(mut connection) = rustls::ServerConnection::new(config) else {
                    return;
                };
                let mut tls = rustls::Stream::new(&mut connection, &mut stream);
                let mut head = [0u8; 4096];
                if tls.read(&mut head).is_ok() {
                    let _ = write_reply(&mut tls, &json_reply(json!([])));
                }
            });
        }
    });
    (port, pin)
}

#[test]
fn a_self_signed_gateway_needs_its_pin() {
    let (port, pin) = tls_server();
    let url = format!("https://localhost:{port}");
    let sign_in = || -> Arc<dyn SignIn> {
        Arc::new(Basic {
            user: "a".into(),
            password: "b".into(),
        })
    };

    let unpinned = WebClient::new(node_with(&url, sign_in())).unwrap();
    let result = unpinned.find_series("S", &never());
    assert!(matches!(result, Err(NetError::Certificate(_))), "{result:?}");
    let (steps, _) = test_connection(&unpinned);
    assert_eq!(steps.len(), 1, "an untrusted certificate stops the test at once");

    let mut pinned = node_with(&url, sign_in());
    pinned.pin = Some(pin);
    assert_eq!(
        WebClient::new(pinned).unwrap().find_series("S", &never()),
        Ok(Vec::new())
    );

    let mut changed = node_with(&url, sign_in());
    let mut other = pin;
    other[0] ^= 0xff;
    changed.pin = Some(other);
    let result = WebClient::new(changed).unwrap().find_series("S", &never());
    assert!(matches!(result, Err(NetError::Certificate(_))), "{result:?}");
    assert!(parse_pin(&"ab".repeat(32)).is_some());
}

#[test]
fn no_secret_reaches_an_error() {
    let secret = "correct-horse-battery";
    let server = serve(|_| reply(401, "text/plain", "no"));
    let sign_in = Arc::new(ServiceToken {
        client_id: "id".into(),
        secret: secret.into(),
    });
    let client = WebClient::new(node_with(&server.url(), sign_in)).unwrap();
    let error = client.find_series("S", &never()).unwrap_err();
    assert!(!format!("{error} {error:?}").contains(secret));

    // Nor does one show in a failed connection.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let sign_in = Arc::new(AccessToken { token: secret.into() });
    let client = WebClient::new(node_with(&format!("http://127.0.0.1:{port}"), sign_in)).unwrap();
    let error = client.find_series("S", &never()).unwrap_err();
    assert!(!format!("{error} {error:?}").contains(secret));
}
