//! Golden-file test: content hashes must be identical across runs and machines.
use kodehub_core::{Kind, Language, VersionContent};
use serde_json::json;

fn sample() -> Vec<(&'static str, VersionContent)> {
    vec![
        (
            "rust_fn",
            VersionContent {
                kind: Kind::Function,
                language: Language::Rust,
                signature: "(password: &str) -> String".into(),
                body: "let salt = SaltString::generate(&mut OsRng);\r\nArgon2::default()  \n    .hash_password(password.as_bytes(), &salt)\n    .unwrap()\n    .to_string()\n\n".into(),
                attributes: json!({"crates": {"argon2": "0.5.3"}, "uses": ["argon2::Argon2"]}),
            },
        ),
        (
            "python_fn",
            VersionContent {
                kind: Kind::Function,
                language: Language::Python,
                signature: "(username: str, password: str) -> dict".into(),
                body: "return {\"ok\": False, \"error\": \"invalid_credentials\"}".into(),
                attributes: json!({}),
            },
        ),
        (
            "unicode",
            VersionContent {
                kind: Kind::Test,
                language: Language::Rust,
                signature: "".into(),
                body: "assert_eq!(\"héllo ✓\".len(), 10);".into(),
                attributes: json!({"z": 1, "a": [true, null, 1.5]}),
            },
        ),
    ]
}

#[test]
fn golden_hashes_are_stable() {
    let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/hashes.txt");
    let actual: String = sample()
        .iter()
        .map(|(n, c)| format!("{n} {}\n", c.hash()))
        .collect();
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(golden_path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(golden_path).expect("golden file present");
    assert_eq!(actual, expected, "content hashes drifted from golden file");
}

#[test]
fn whitespace_and_line_endings_do_not_change_hash() {
    let a = &sample()[0].1;
    let mut b = a.clone();
    b.body = b.body.replace("\r\n", "\n").replace("  \n", "\n");
    b.body.push_str("\n\n   \n");
    assert_eq!(a.hash(), b.hash());
    let mut c = a.clone();
    c.body.push_str("// changed");
    assert_ne!(a.hash(), c.hash());
}
