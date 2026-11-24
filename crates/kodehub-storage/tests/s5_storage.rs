//! S5 acceptance (storage): objects round-trip through the storage service; cloud mode writes
//! straight through; a hybrid backup produces a full then an incremental bundle per project; one
//! project restores alone.
use bytes::Bytes;
use kodehub_storage::{LocalBackend, Mode, Storage, StorageClient, Upstream, UpstreamAuth, BUCKET};

async fn serve(s: Storage) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(l, kodehub_storage::server::router(s))
            .await
            .unwrap()
    });
    url
}

#[tokio::test]
async fn objects_modes_backups_and_single_project_restore() {
    let cold_dir = tempfile::tempdir().unwrap();
    let hot_dir = tempfile::tempdir().unwrap();
    // the cold tier: another storage service in local mode (stands in for S3 Glacier)
    let cold = serve(
        Storage::new(
            Mode::Local,
            Some(LocalBackend {
                root: cold_dir.path().into(),
            }),
            None,
            Some("cold-secret".into()),
        )
        .unwrap(),
    )
    .await;
    let upstream = Upstream::new(
        &cold,
        "kodehub-cold",
        UpstreamAuth::Bearer("cold-secret".into()),
    )
    .unwrap();
    let hot = serve(
        Storage::new(
            Mode::Hybrid,
            Some(LocalBackend {
                root: hot_dir.path().into(),
            }),
            Some(upstream.clone()),
            Some("hot-secret".into()),
        )
        .unwrap(),
    )
    .await;

    // auth required (health is public)
    let anon = StorageClient::new(&hot, None);
    assert!(anon.put(BUCKET, "projects/a/x", "nope").await.is_err());
    assert!(reqwest::get(format!("{hot}/health"))
        .await
        .unwrap()
        .status()
        .is_success());

    let c = StorageClient::new(&hot, Some("hot-secret".into()));
    // round trip, etags, list, delete, path traversal refused
    let tag = c
        .put(
            BUCKET,
            "projects/alpha/artifacts/h1/rust-lib.tar",
            Bytes::from_static(b"binary-1"),
        )
        .await
        .unwrap();
    assert_eq!(tag.len(), 32);
    assert_eq!(
        &c.get(BUCKET, "projects/alpha/artifacts/h1/rust-lib.tar")
            .await
            .unwrap()
            .unwrap()[..],
        b"binary-1"
    );
    assert!(c
        .exists(BUCKET, "projects/alpha/artifacts/h1/rust-lib.tar")
        .await
        .unwrap());
    assert!(c
        .get(BUCKET, "projects/alpha/missing")
        .await
        .unwrap()
        .is_none());
    // clients normalise `..` away, so speak raw HTTP: the server refuses a traversal itself
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut sock = tokio::net::TcpStream::connect(hot.trim_start_matches("http://"))
            .await
            .unwrap();
        sock.write_all(b"PUT /kodehub/projects/../../../etc/passwd HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer hot-secret\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx").await.unwrap();
        let mut resp = String::new();
        sock.read_to_string(&mut resp).await.unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        assert!(!hot_dir.path().join("etc").exists());
    }
    c.put(BUCKET, "projects/alpha/tests/t1.sh", "#!/bin/sh\nexit 0\n")
        .await
        .unwrap();
    c.put(BUCKET, "projects/alpha/tests/t2.sh", "#!/bin/sh\nexit 0\n")
        .await
        .unwrap();
    c.put(BUCKET, "projects/beta/artifacts/h9/python-pkg.tar", "py")
        .await
        .unwrap();
    let alpha = c.list(BUCKET, "projects/alpha/").await.unwrap();
    assert_eq!(alpha.len(), 3);

    // first backup: a full bundle per project
    let r = c
        .admin(reqwest::Method::POST, "_backup", None)
        .await
        .unwrap();
    let reports = r["backups"].as_array().unwrap();
    assert_eq!(reports.len(), 2);
    let a1 = reports.iter().find(|x| x["project"] == "alpha").unwrap();
    assert_eq!(a1["kind"], "full");
    assert_eq!(a1["carried"], 3);
    // nothing changed → no bundle
    let r = c
        .admin(reqwest::Method::POST, "_backup", Some("alpha"))
        .await
        .unwrap();
    assert_eq!(r["backups"][0]["kind"], "none");
    // change one, delete one, add one → a small incremental bundle
    c.put(BUCKET, "projects/alpha/tests/t1.sh", "#!/bin/sh\nexit 1\n")
        .await
        .unwrap();
    c.delete(BUCKET, "projects/alpha/tests/t2.sh")
        .await
        .unwrap();
    c.put(BUCKET, "projects/alpha/tests/t3.sh", "#!/bin/sh\necho hi\n")
        .await
        .unwrap();
    let r = c
        .admin(reqwest::Method::POST, "_backup", Some("alpha"))
        .await
        .unwrap();
    let a2 = &r["backups"][0];
    assert_eq!(a2["kind"], "incr");
    assert_eq!(a2["carried"], 2);
    assert_eq!(a2["deleted"], 1);
    assert!(a2["bytes"].as_u64().unwrap() < a1["bytes"].as_u64().unwrap() + 2048);
    let b = c
        .admin(reqwest::Method::GET, "_backups", Some("alpha"))
        .await
        .unwrap();
    let names: Vec<&str> = b["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "backups/alpha/000001-full.tar",
            "backups/alpha/000002-incr.tar"
        ]
    );

    // lose the hot tier's alpha objects (and corrupt one); beta is untouched
    std::fs::remove_dir_all(hot_dir.path().join("kodehub/projects/alpha")).unwrap();
    c.put(BUCKET, "projects/alpha/stray", "not in the backup")
        .await
        .unwrap();
    let r = c
        .admin(reqwest::Method::POST, "_restore", Some("alpha"))
        .await
        .unwrap();
    assert_eq!(r["applied"].as_array().unwrap().len(), 2);
    let alpha = c.list(BUCKET, "projects/alpha/").await.unwrap();
    let keys: Vec<&str> = alpha.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "projects/alpha/artifacts/h1/rust-lib.tar",
            "projects/alpha/tests/t1.sh",
            "projects/alpha/tests/t3.sh"
        ]
    );
    assert_eq!(
        &c.get(BUCKET, "projects/alpha/tests/t1.sh")
            .await
            .unwrap()
            .unwrap()[..],
        b"#!/bin/sh\nexit 1\n"
    );
    assert_eq!(
        &c.get(BUCKET, "projects/beta/artifacts/h9/python-pkg.tar")
            .await
            .unwrap()
            .unwrap()[..],
        b"py"
    );

    // cloud mode: straight through to the upstream
    let cloud = serve(Storage::new(Mode::Cloud, None, Some(upstream), None).unwrap()).await;
    let cc = StorageClient::new(&cloud, None);
    cc.put(BUCKET, "projects/gamma/a", "through").await.unwrap();
    assert_eq!(
        &cc.get(BUCKET, "projects/gamma/a").await.unwrap().unwrap()[..],
        b"through"
    );
    assert_eq!(
        cc.list(BUCKET, "projects/gamma/").await.unwrap()[0].key,
        "projects/gamma/a"
    );
    assert!(
        cold_dir
            .path()
            .join("kodehub-cold/kodehub/projects/gamma/a")
            .exists(),
        "stored upstream, not locally"
    );
}
