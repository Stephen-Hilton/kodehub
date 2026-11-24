//! Container jobs (spec §3.1, §5.3–5.4): build, test, git-import and git-export containers.
//!
//! The orchestrator never runs that work in its own process: it writes the job input to the
//! storage service, starts `kodehub-worker <kind> --input <key> --output <key>` in a container,
//! waits, and reads the output back. Containers never write the graph.
//!
//! Runtimes:
//! - `kubernetes`: a `batch/v1` Job created through the Kubernetes API with the pod's service
//!   account (no kubectl in the image), image per job kind;
//! - `local`: the same entrypoint as a subprocess (laptop dev loop and `cargo test`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Result, RunnerError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    /// `build` | `test` | `git-import` | `git-export`
    pub kind: String,
    /// Test container type (`rust`, `python`, `browser`) for test jobs.
    #[serde(default)]
    pub container_type: Option<String>,
    pub input_key: String,
    pub output_key: String,
    pub timeout: Duration,
    /// Extra environment (e.g. a Git credential, passed for this job only).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Name hint (work item key).
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutcome {
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub logs: String,
    pub duration_ms: u64,
    pub runtime: String,
    pub job: String,
}

#[derive(Debug, Clone)]
pub enum JobRuntime {
    Local {
        worker: PathBuf,
        /// Environment for the subprocess (storage location, Runner home overrides).
        env: Vec<(String, String)>,
    },
    Kubernetes(Kubernetes),
}

impl JobRuntime {
    /// `KODEHUB_RUNTIME=kubernetes` inside the cluster, else local.
    pub fn from_env() -> Self {
        if std::env::var("KODEHUB_RUNTIME").as_deref() == Ok("kubernetes") {
            if let Ok(k) = Kubernetes::in_cluster() {
                return JobRuntime::Kubernetes(k);
            }
            tracing::warn!("KODEHUB_RUNTIME=kubernetes but no in-cluster config; using local jobs");
        }
        JobRuntime::Local {
            worker: local_worker_bin(),
            env: vec![],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            JobRuntime::Local { .. } => "local",
            JobRuntime::Kubernetes(_) => "kubernetes",
        }
    }

    pub async fn run(&self, spec: &JobSpec) -> Result<JobOutcome> {
        match self {
            JobRuntime::Local { worker, env } => run_local(worker, env, spec).await,
            JobRuntime::Kubernetes(k) => k.run(spec).await,
        }
    }
}

/// The `kodehub-worker` binary next to the current executable (or on PATH).
pub fn local_worker_bin() -> PathBuf {
    if let Ok(p) = std::env::var("KODEHUB_WORKER_BIN") {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for _ in 0..2 {
            let c = dir.join("kodehub-worker");
            if c.exists() {
                return c;
            }
            dir = dir.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        }
    }
    "kodehub-worker".into()
}

async fn run_local(
    worker: &PathBuf,
    env: &[(String, String)],
    spec: &JobSpec,
) -> Result<JobOutcome> {
    let start = Instant::now();
    let mut c = tokio::process::Command::new(worker);
    c.arg(&spec.kind)
        .arg("--input")
        .arg(&spec.input_key)
        .arg("--output")
        .arg(&spec.output_key)
        .kill_on_drop(true);
    for (k, v) in env.iter().chain(spec.env.iter()) {
        c.env(k, v);
    }
    let out = tokio::time::timeout(spec.timeout, c.output())
        .await
        .map_err(|_| RunnerError::Build(format!("{} job timed out", spec.kind)))?
        .map_err(|e| RunnerError::Invalid(format!("cannot start {}: {e}", worker.display())))?;
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(JobOutcome {
        ok: out.status.success(),
        exit_code: out.status.code(),
        logs: tail(&logs, 200),
        duration_ms: start.elapsed().as_millis() as u64,
        runtime: "local".into(),
        job: format!("local-{}", spec.name),
    })
}

fn tail(s: &str, n: usize) -> String {
    let l: Vec<&str> = s.lines().collect();
    l[l.len().saturating_sub(n)..].join("\n")
}

/// Kubernetes API access with the pod's service account.
#[derive(Debug, Clone)]
pub struct Kubernetes {
    pub api: String,
    pub namespace: String,
    token: String,
    http: reqwest::Client,
    /// Image of build / git jobs (has the toolchains).
    pub build_image: String,
    /// Test image per container type.
    pub test_image_prefix: String,
}

const SA: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

impl Kubernetes {
    pub fn in_cluster() -> Result<Self> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|_| RunnerError::Invalid("not in a cluster".into()))?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let token = std::fs::read_to_string(format!("{SA}/token"))?;
        let ca = std::fs::read(format!("{SA}/ca.crt"))?;
        let namespace = std::env::var("KODEHUB_NAMESPACE")
            .or_else(|_| std::fs::read_to_string(format!("{SA}/namespace")))
            .unwrap_or_else(|_| "kodehub".into());
        let cert = reqwest::Certificate::from_pem(&ca)
            .map_err(|e| RunnerError::Invalid(format!("cluster CA: {e}")))?;
        let http = reqwest::Client::builder()
            .add_root_certificate(cert)
            .build()
            .map_err(|e| RunnerError::Invalid(e.to_string()))?;
        Ok(Kubernetes {
            api: format!("https://{host}:{port}"),
            namespace: namespace.trim().to_string(),
            token: token.trim().to_string(),
            http,
            build_image: std::env::var("KODEHUB_BUILD_IMAGE")
                .unwrap_or_else(|_| "kodehub/kodehub:dev".into()),
            test_image_prefix: std::env::var("KODEHUB_TEST_IMAGE_PREFIX")
                .unwrap_or_else(|_| "kodehub/test-".into()),
        })
    }

    fn image_for(&self, spec: &JobSpec) -> String {
        match (spec.kind.as_str(), spec.container_type.as_deref()) {
            ("test", Some(t)) => format!("{}{t}:dev", self.test_image_prefix),
            _ => self.build_image.clone(),
        }
    }

    async fn call(&self, m: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut r = self
            .http
            .request(m, format!("{}{path}", self.api))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(b);
        }
        let resp = r
            .send()
            .await
            .map_err(|e| RunnerError::Invalid(format!("kubernetes API: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(RunnerError::Invalid(format!(
                "kubernetes API {path}: {status} {text}"
            )));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub fn job_manifest(&self, name: &str, spec: &JobSpec) -> Value {
        let mut env = vec![
            json!({"name": "KODEHUB_STORAGE_URL", "value": std::env::var("KODEHUB_STORAGE_URL").unwrap_or_else(|_| "http://storage:9000".into())}),
            json!({"name": "KODEHUB_STORAGE_TOKEN", "valueFrom": {"secretKeyRef": {"name": "kodehub-secrets", "key": "storage-token"}}}),
            json!({"name": "KODEHUB_HOME", "value": "/work/kode"}),
            // the non-root job user needs a writable cargo registry cache
            json!({"name": "CARGO_HOME", "value": "/work/cargo-home"}),
        ];
        for (k, v) in &spec.env {
            env.push(json!({"name": k, "value": v}));
        }
        json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": {"name": name, "labels": {"app": "kodehub-job", "kodehub/kind": spec.kind}},
            "spec": {
                "backoffLimit": 0,
                "ttlSecondsAfterFinished": 600,
                "activeDeadlineSeconds": spec.timeout.as_secs().max(60),
                "template": {
                    "metadata": {"labels": {"app": "kodehub-job", "kodehub/kind": spec.kind}},
                    "spec": {
                        "restartPolicy": "Never",
                        "automountServiceAccountToken": false,
                        "containers": [{
                            "name": spec.kind,
                            "image": self.image_for(spec),
                            "imagePullPolicy": "Never",
                            "command": ["kodehub-worker", spec.kind, "--input", spec.input_key, "--output", spec.output_key],
                            "env": env,
                            "workingDir": "/work",
                            "volumeMounts": [{"name": "work", "mountPath": "/work"}],
                            "resources": {"requests": {"cpu": "100m", "memory": "128Mi"}},
                        }],
                        "volumes": [{"name": "work", "emptyDir": {}}],
                    }
                }
            }
        })
    }

    pub async fn run(&self, spec: &JobSpec) -> Result<JobOutcome> {
        let start = Instant::now();
        let slug: String = spec
            .name
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let name = format!(
            "kh-{}-{}-{}",
            spec.kind,
            slug[..slug.len().min(20)].trim_matches('-'),
            &uuid_hex()[..6]
        );
        let ns = &self.namespace;
        self.call(
            reqwest::Method::POST,
            &format!("/apis/batch/v1/namespaces/{ns}/jobs"),
            Some(&self.job_manifest(&name, spec)),
        )
        .await?;
        let deadline = Instant::now() + spec.timeout + Duration::from_secs(60);
        let ok = loop {
            let j = self
                .call(
                    reqwest::Method::GET,
                    &format!("/apis/batch/v1/namespaces/{ns}/jobs/{name}"),
                    None,
                )
                .await?;
            if j["status"]["succeeded"].as_u64().unwrap_or(0) > 0 {
                break true;
            }
            if j["status"]["failed"].as_u64().unwrap_or(0) > 0 {
                break false;
            }
            if Instant::now() > deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        };
        let pods = self
            .call(
                reqwest::Method::GET,
                &format!("/api/v1/namespaces/{ns}/pods?labelSelector=job-name%3D{name}"),
                None,
            )
            .await?;
        let mut logs = String::new();
        if let Some(p) = pods["items"].as_array().and_then(|a| a.first()) {
            let pod = p["metadata"]["name"].as_str().unwrap_or_default();
            if let Ok(Value::String(l)) = self
                .call(
                    reqwest::Method::GET,
                    &format!("/api/v1/namespaces/{ns}/pods/{pod}/log"),
                    None,
                )
                .await
            {
                logs = l;
            }
        }
        let _ = self
            .call(
                reqwest::Method::DELETE,
                &format!("/apis/batch/v1/namespaces/{ns}/jobs/{name}?propagationPolicy=Background"),
                None,
            )
            .await;
        Ok(JobOutcome {
            ok,
            exit_code: if ok { Some(0) } else { None },
            logs: tail(&logs, 200),
            duration_ms: start.elapsed().as_millis() as u64,
            runtime: "kubernetes".into(),
            job: name,
        })
    }
}

fn uuid_hex() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    blake3::hash(format!("{t}{}", std::process::id()).as_bytes())
        .to_hex()
        .to_string()
}
