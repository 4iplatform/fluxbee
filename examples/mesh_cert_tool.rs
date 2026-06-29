//! Mesh TLS cert tool (dev/ops + lab validation). Uses the real `mesh_tls` code.
//!
//!   mesh_cert_tool gen-ca <ca_dir>
//!       Generate a dedicated mesh CA into <ca_dir>/{ca.crt,ca.key}.
//!   mesh_cert_tool issue <ca_dir> <hive_id> <out_dir>
//!       Load the CA from <ca_dir> and issue a per-hive leaf into
//!       <out_dir>/{ca.crt,cert.crt,cert.key}.

use std::fs;
use std::path::Path;

use json_router::mesh_tls::MeshCa;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen-ca") => {
            let dir = Path::new(args.get(2).expect("usage: gen-ca <ca_dir>"));
            fs::create_dir_all(dir).unwrap();
            let ca = MeshCa::generate().expect("generate CA");
            fs::write(dir.join("ca.crt"), ca.ca_cert_pem()).unwrap();
            fs::write(dir.join("ca.key"), ca.ca_key_pem()).unwrap();
            eprintln!("wrote CA to {}", dir.display());
        }
        Some("issue") => {
            let ca_dir = Path::new(args.get(2).expect("usage: issue <ca_dir> <hive_id> <out_dir>"));
            let hive_id = args.get(3).expect("hive_id");
            let out = Path::new(args.get(4).expect("out_dir"));
            let ca = MeshCa::from_pem(
                &fs::read_to_string(ca_dir.join("ca.crt")).unwrap(),
                &fs::read_to_string(ca_dir.join("ca.key")).unwrap(),
            )
            .expect("load CA");
            let leaf = ca.issue_leaf(hive_id).expect("issue leaf");
            fs::create_dir_all(out).unwrap();
            fs::write(out.join("ca.crt"), ca.ca_cert_pem()).unwrap();
            fs::write(out.join("cert.crt"), &leaf.cert_pem).unwrap();
            fs::write(out.join("cert.key"), &leaf.key_pem).unwrap();
            eprintln!("issued leaf for '{hive_id}' to {}", out.display());
        }
        _ => {
            eprintln!("usage: mesh_cert_tool gen-ca <ca_dir> | issue <ca_dir> <hive_id> <out_dir>");
            std::process::exit(2);
        }
    }
}
