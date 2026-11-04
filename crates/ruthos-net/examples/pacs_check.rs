//! Check Ruthos's PACS client against a real PACS.
//!
//!   cargo run -p ruthos-net --example pacs_check -- HOST PORT AE FOLDER
//!
//! Echoes the PACS, sends every .dcm in FOLDER, searches, retrieves the
//! first study by C-GET and by C-MOVE (starting a listener as AE RUTHOS on
//! port 11121, which the PACS must know), and shows the explanation for a
//! wrong AE title. Used against pynetdicom's qrscp; see docs/pacs.md.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ruthos_net::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [host, port, ae, folder] = &args[..] else {
        eprintln!("usage: pacs_check HOST PORT AE FOLDER");
        std::process::exit(2);
    };
    let node = Node {
        host: host.clone(),
        port: port.parse().unwrap(),
        ae_title: ae.clone(),
    };
    let me = Me {
        ae_title: "RUTHOS".into(),
        port: 11121,
        address: "127.0.0.1".into(),
        listener_on: true,
    };
    let never = AtomicBool::new(false);

    println!("echo: {:?}", echo(&node, "RUTHOS"));

    let files: Vec<PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "dcm"))
        .collect();
    match send_files(&node, "RUTHOS", &files, &|_, _| {}, &never) {
        Ok(o) => {
            println!("send: {} sent, {} failed", o.sent, o.failed.len());
            for (f, why) in &o.failed {
                println!("  not sent: {}: {why}", f.file_name().unwrap().to_string_lossy());
            }
        }
        Err(e) => println!("send failed: {}", explain(&e, &node, &me).summary),
    }

    let studies = find_studies(&node, "RUTHOS", &StudyQuery::default(), &never).unwrap();
    println!("find: {} studies", studies.len());
    for s in &studies {
        println!(
            "  {} | {} | {} | {} | {:?} images",
            s.patient_name, s.patient_id, s.study_date, s.modalities, s.instances
        );
    }
    let by_name = find_studies(
        &node,
        "RUTHOS",
        &StudyQuery {
            patient: studies[0].patient_name.split('^').next().unwrap().to_string(),
            ..Default::default()
        },
        &never,
    )
    .unwrap();
    println!("find by name: {} studies", by_name.len());

    let study = &studies[0].study_uid;
    let out = std::env::temp_dir().join("ruthos-pacs-check-get");
    let _ = std::fs::remove_dir_all(&out);
    let r = retrieve_study(&node, "RUTHOS", study, RetrieveMethod::Get, &out, &|_| {}, &never);
    match r {
        Ok(o) => println!("c-get: {} files, progress {:?}", o.files.len(), o.progress),
        Err(e) => println!("c-get failed: {e}"),
    }

    let inbox = std::env::temp_dir().join("ruthos-pacs-check-move");
    let _ = std::fs::remove_dir_all(&inbox);
    let mut scp = StoreScp::start(ListenerConfig {
        port: 11121,
        ae_title: "RUTHOS".into(),
        out_dir: inbox.clone(),
        allow: std::sync::Arc::new(std::sync::Mutex::new(vec![ruthos_net::scp::AllowEntry {
            name: "Test PACS".into(),
            ae_title: "QRSCP".into(),
            address: String::new(),
        }])),
        promiscuous: false,
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    match retrieve_study(&node, "RUTHOS", study, RetrieveMethod::Move, &inbox, &|_| {}, &never) {
        Ok(o) => {
            std::thread::sleep(Duration::from_millis(500));
            let n = std::fs::read_dir(&inbox).map(|d| d.count()).unwrap_or(0);
            println!("c-move: progress {:?}, {n} files arrived at the listener", o.progress);
        }
        Err(e) => println!("c-move failed: {}", explain(&e, &node, &me).summary),
    }
    scp.stop();

    let wrong = Node {
        ae_title: "WRONG".into(),
        ..node.clone()
    };
    match echo(&wrong, "RUTHOS") {
        Ok(_) => println!("wrong AE: accepted (this PACS does not check called AE titles)"),
        Err(e) => {
            let p = explain(&e, &wrong, &me);
            println!("wrong AE: {} / {}", p.summary, p.fix);
        }
    }
    let unknown = Node {
        host: "no-such-pacs.invalid".into(),
        ..node.clone()
    };
    let p = explain(&echo(&unknown, "RUTHOS").unwrap_err(), &unknown, &me);
    println!("unknown host: {} / {}", p.summary, p.fix);
    let closed = Node { port: 11199, ..node };
    let p = explain(&echo(&closed, "RUTHOS").unwrap_err(), &closed, &me);
    println!("closed port: {} / {}", p.summary, p.fix);
}
