//! The listener's allow list (Decoder Hardening Specification, acceptance
//! checks 10 and 11): with an empty list every device is refused and
//! reported; once listed, the same device gets through; a listed AE title
//! from another address is refused.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ruthos_net::scp::AllowEntry;
use ruthos_net::{echo, ListenerConfig, NetEvent, Node, StoreScp};

fn refused_from(scp: &StoreScp) -> Option<(String, String)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        while let Ok(ev) = scp.events.try_recv() {
            if let NetEvent::Refused { peer_ae, peer_addr } = ev {
                return Some((peer_ae, peer_addr));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn only_listed_devices_get_in() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let allow = Arc::new(Mutex::new(Vec::new()));
    let mut scp = StoreScp::start(ListenerConfig {
        port,
        ae_title: "RUTHOS".into(),
        out_dir: std::env::temp_dir().join(format!("ruthos-allow-{}", std::process::id())),
        allow: allow.clone(),
        promiscuous: false,
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let node = Node {
        host: "127.0.0.1".into(),
        port,
        ae_title: "RUTHOS".into(),
    };

    // Empty list: refused, and reported with the AE title and address.
    assert!(echo(&node, "ECHO_ROOM2").is_err());
    let (ae, addr) = refused_from(&scp).expect("the refusal is reported");
    assert_eq!(ae, "ECHO_ROOM2");
    assert_eq!(addr, "127.0.0.1");

    // Add to list: the same device gets through.
    allow.lock().unwrap().push(AllowEntry {
        name: "Ultrasound, room 2".into(),
        ae_title: "ECHO_ROOM2".into(),
        address: String::new(),
    });
    echo(&node, "ECHO_ROOM2").expect("a listed device is accepted");

    // A listed title from another address is refused.
    allow.lock().unwrap()[0].address = "10.20.30.40".into();
    assert!(echo(&node, "ECHO_ROOM2").is_err());
    assert!(refused_from(&scp).is_some());
    // From the right subnet it is accepted again.
    allow.lock().unwrap()[0].address = "127.0.0.0/8".into();
    echo(&node, "ECHO_ROOM2").expect("from the listed subnet");
    scp.stop();
}
