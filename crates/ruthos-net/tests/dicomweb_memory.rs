//! A large series must never sit in memory. Every allocation in this test
//! program is counted, while a server streams more than the limit allows.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use ruthos_net::dicomweb::{NoSignIn, Transfer, WebClient, WebNode};
use ruthos_net::RetrieveProgress;

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: passes every call straight to the system allocator and only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = NOW.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        NOW.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

const INSTANCES: usize = 500;
const INSTANCE_BYTES: usize = 1024 * 1024;
const MOST_MEMORY: usize = 300 * 1024 * 1024;

fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                }
                let mut stream = stream;
                if first.contains("/series ") || first.contains("/series?") {
                    let body = format!(
                        "[{{\"0020000E\":{{\"vr\":\"UI\",\"Value\":[\"E\"]}},\"00201209\":{{\"vr\":\"IS\",\"Value\":[{INSTANCES}]}}}}]"
                    );
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/dicom+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).unwrap();
                    stream.write_all(body.as_bytes()).unwrap();
                    return;
                }
                let head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\
                    Content-Type: multipart/related; type=\"application/dicom\"; boundary=XYZ\r\n\r\n";
                stream.write_all(head.as_bytes()).unwrap();
                let piece = vec![9u8; 64 * 1024];
                for _ in 0..INSTANCES {
                    stream
                        .write_all(b"--XYZ\r\nContent-Type: application/dicom\r\n\r\n")
                        .unwrap();
                    for _ in 0..INSTANCE_BYTES / piece.len() {
                        stream.write_all(&piece).unwrap();
                    }
                    stream.write_all(b"\r\n").unwrap();
                }
                stream.write_all(b"--XYZ--\r\n").unwrap();
            });
        }
    });
    port
}

#[test]
fn a_500_instance_series_stays_out_of_memory() {
    let port = serve();
    let url = format!("http://127.0.0.1:{port}");
    let client = WebClient::new(WebNode {
        name: "Test".into(),
        qido: url.clone(),
        wado: url.clone(),
        stow: url,
        sign_in: Arc::new(NoSignIn),
        pin: None,
        client_certificate: None,
        proxy: None,
        transfer: Transfer::Original,
        page_size: 100,
        include_all: None,
        parallel: 4,
        most_parts: ruthos_net::dicomweb::multipart::MOST_PARTS,
    })
    .unwrap();
    let out = std::env::temp_dir().join(format!("ruthos-web-memory-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();

    let before = NOW.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let cancel = AtomicBool::new(false);
    let outcome = client
        .retrieve(
            "S",
            None,
            &out,
            &|_| false,
            &|_| false,
            &|_: RetrieveProgress| {},
            &cancel,
        )
        .unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - before;

    assert_eq!(outcome.files.len(), INSTANCES);
    assert_eq!(
        std::fs::metadata(&outcome.files[0]).unwrap().len(),
        INSTANCE_BYTES as u64
    );
    std::fs::remove_dir_all(&out).ok();
    println!("peak memory: {} KB for {} MB received", peak / 1024, INSTANCES);
    assert!(peak > 16 * 1024, "the counter counted something: {peak}");
    assert!(peak < MOST_MEMORY, "peak {peak} bytes");
}
