//! Agent containers (spec §4.1–4.2): every agent task runs in a fresh container from the one
//! tiny agent image, instantiated with a role, instructions, tools and context, and torn down
//! afterwards. The model credential is passed to the container only for the duration of the
//! task (a Kubernetes Secret deleted with the job); the agent's graph tools use a short-lived
//! Kodehub token scoped to one project and work item.

use std::time::Duration;

use kodehub_runner::jobs::{JobRuntime, JobSpec};
use kodehub_storage::{StorageClient, BUCKET};
use serde_json::json;

use crate::{AgentBackend, AgentError, AgentOutput, AgentTask, RecordedBackend, Result};

pub struct ContainerBackend {
    /// What runs inside the container: `claude_code`, `api` or `recorded`.
    pub inner: String,
    pub jobs: JobRuntime,
    pub storage: StorageClient,
    /// `(env name, value)`: CLAUDE_CODE_OAUTH_TOKEN for claude_code, KODEHUB_ANTHROPIC_API_KEY for
    /// api. Never ANTHROPIC_API_KEY.
    pub credential: Option<(String, String)>,
    /// Kodehub API as seen from inside the cluster.
    pub server_url: String,
    pub timeout: Duration,
    /// For `recorded`: the server's recording, which keeps the position across tasks (each
    /// container is fresh, so it replays the answer it is handed).
    pub recorded: Option<std::sync::Arc<RecordedBackend>>,
}

#[async_trait::async_trait]
impl AgentBackend for ContainerBackend {
    fn name(&self) -> &str {
        "container"
    }

    async fn run(&self, task: &AgentTask) -> Result<AgentOutput> {
        let project = task.project.clone().unwrap_or_else(|| "_".into());
        let id = format!(
            "{}-{}",
            task.role.as_str(),
            &blake3::hash(format!("{}{:?}", task.label, std::time::SystemTime::now()).as_bytes())
                .to_hex()[..10]
        );
        let in_key = format!("projects/{project}/jobs/agent-{id}/input.json");
        let out_key = format!("projects/{project}/jobs/agent-{id}/output.json");
        let mut input = json!({"task": task, "backend": self.inner, "server_url": self.server_url});
        if let Some(r) = &self.recorded {
            input["recorded_output"] = r.next_output(task)?;
            input["recording"] = json!(r.scenario);
        }
        self.storage
            .put(
                BUCKET,
                &in_key,
                serde_json::to_vec(&input).unwrap_or_default(),
            )
            .await
            .map_err(|e| AgentError::Backend(format!("storage: {e}")))?;
        let outcome = self
            .jobs
            .run(&JobSpec {
                kind: "agent".into(),
                container_type: None,
                input_key: in_key.clone(),
                output_key: out_key.clone(),
                timeout: self.timeout,
                env: vec![],
                secret_env: self.credential.iter().cloned().collect(),
                name: id,
            })
            .await
            .map_err(|e| AgentError::Backend(format!("agent container: {e}")))?;
        let out = self
            .storage
            .get(BUCKET, &out_key)
            .await
            .map_err(|e| AgentError::Backend(format!("storage: {e}")))?
            .ok_or_else(|| {
                AgentError::Backend(format!("agent container wrote no output: {}", outcome.logs))
            })?;
        // the input carried a token: remove it now that the task is over
        let _ = self.storage.delete(BUCKET, &in_key).await;
        let v: serde_json::Value =
            serde_json::from_slice(&out).map_err(|e| AgentError::Backend(e.to_string()))?;
        if v["ok"] != true {
            return Err(AgentError::Backend(format!(
                "agent container failed: {}",
                v["error"].as_str().unwrap_or("unknown error")
            )));
        }
        let mut o: AgentOutput = serde_json::from_value(v["output"].clone())
            .map_err(|e| AgentError::Output(e.to_string()))?;
        o.backend = format!("{}-in-container", o.backend);
        Ok(o)
    }
}
