//! The storage service against a real S3-compatible store (SeaweedFS in the Kind cluster, see
//! `scripts/k8s.sh seaweed-check`): SigV4 bucket creation, objects in cloud mode, and hybrid
//! backups with a single-project restore. Skipped unless `KODEHUB_TEST_S3_URL` is set.
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

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[tokio::test]
async fn hybrid_and_cloud_modes_over_a_real_s3_store() {
    let Some(url) = env("KODEHUB_TEST_S3_URL") else {
        eprintln!("skipped: KODEHUB_TEST_S3_URL is not set (run `scripts/k8s.sh seaweed-check`)");
        return;
    };
    let auth = UpstreamAuth::SigV4 {
        access_key: env("KODEHUB_TEST_S3_ACCESS_KEY").expect("KODEHUB_TEST_S3_ACCESS_KEY"),
        secret_key: env("KODEHUB_TEST_S3_SECRET_KEY").expect("KODEHUB_TEST_S3_SECRET_KEY"),
        region: env("KODEHUB_TEST_S3_REGION").unwrap_or_else(|| "us-east-1".into()),
    };
    // a fresh bucket per run, so runs never see each other's bundles
    let bucket = format!(
        "kodehub-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let up = Upstream::new(&url, &bucket, auth.clone()).unwrap();
    up.ensure_bucket().await.unwrap();
    up.ensure_bucket().await.expect("creating twice is fine");

    // wrong secret: refused by the store
    let bad = Upstream::new(
        &url,
        &bucket,
        UpstreamAuth::SigV4 {
            access_key: env("KODEHUB_TEST_S3_ACCESS_KEY").unwrap(),
            secret_key: "not-the-secret".into(),
            region: "us-east-1".into(),
        },
    )
    .unwrap();
    assert!(bad.put("x", Bytes::from_static(b"x")).await.is_err());

    // cloud mode: straight through to the store
    let cloud =
        serve(Storage::new(Mode::Cloud, None, Some(up.clone()), Some("t".into())).unwrap()).await;
    let cc = StorageClient::new(&cloud, Some("t".into()));
    cc.put(BUCKET, "projects/gamma/artifacts/h1/lib.tar", "through")
        .await
        .unwrap();
    assert_eq!(
        &cc.get(BUCKET, "projects/gamma/artifacts/h1/lib.tar")
            .await
            .unwrap()
            .unwrap()[..],
        b"through"
    );
    assert!(cc
        .get(BUCKET, "projects/gamma/missing")
        .await
        .unwrap()
        .is_none());
    let listed = cc.list(BUCKET, "projects/gamma/").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, "projects/gamma/artifacts/h1/lib.tar");
    assert_eq!(listed[0].size, 7);
    cc.delete(BUCKET, "projects/gamma/artifacts/h1/lib.tar")
        .await
        .unwrap();
    assert!(cc.list(BUCKET, "projects/gamma/").await.unwrap().is_empty());

    // hybrid mode: local hot tier, per-project backups to the store, single-project restore
    let hot_dir = tempfile::tempdir().unwrap();
    let hot = serve(
        Storage::new(
            Mode::Hybrid,
            Some(LocalBackend {
                root: hot_dir.path().into(),
            }),
            Some(up.clone()),
            Some("h".into()),
        )
        .unwrap(),
    )
    .await;
    let c = StorageClient::new(&hot, Some("h".into()));
    c.put(BUCKET, "projects/alpha/tests/t1.sh", "#!/bin/sh\nexit 0\n")
        .await
        .unwrap();
    c.put(BUCKET, "projects/alpha/tests/t2.sh", "#!/bin/sh\nexit 0\n")
        .await
        .unwrap();
    c.put(BUCKET, "projects/beta/a", "beta").await.unwrap();
    let r = c
        .admin(reqwest::Method::POST, "_backup", None)
        .await
        .unwrap();
    let a1 = r["backups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["project"] == "alpha")
        .unwrap()
        .clone();
    assert_eq!(a1["kind"], "full");
    c.put(BUCKET, "projects/alpha/tests/t1.sh", "#!/bin/sh\nexit 1\n")
        .await
        .unwrap();
    c.delete(BUCKET, "projects/alpha/tests/t2.sh")
        .await
        .unwrap();
    let r = c
        .admin(reqwest::Method::POST, "_backup", Some("alpha"))
        .await
        .unwrap();
    assert_eq!(r["backups"][0]["kind"], "incr");
    let b = c
        .admin(reqwest::Method::GET, "_backups", Some("alpha"))
        .await
        .unwrap();
    assert_eq!(
        b["bundles"],
        serde_json::json!([
            "backups/alpha/000001-full.tar",
            "backups/alpha/000002-incr.tar"
        ])
    );

    std::fs::remove_dir_all(hot_dir.path().join("kodehub/projects/alpha")).unwrap();
    let r = c
        .admin(reqwest::Method::POST, "_restore", Some("alpha"))
        .await
        .unwrap();
    assert_eq!(r["applied"].as_array().unwrap().len(), 2);
    let keys: Vec<String> = c
        .list(BUCKET, "projects/alpha/")
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.key)
        .collect();
    assert_eq!(keys, vec!["projects/alpha/tests/t1.sh"]);
    assert_eq!(
        &c.get(BUCKET, "projects/alpha/tests/t1.sh")
            .await
            .unwrap()
            .unwrap()[..],
        b"#!/bin/sh\nexit 1\n"
    );
    assert_eq!(
        &c.get(BUCKET, "projects/beta/a").await.unwrap().unwrap()[..],
        b"beta"
    );
}
