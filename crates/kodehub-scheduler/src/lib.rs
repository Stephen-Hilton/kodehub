//! The scheduler (spec §3.3): very light, so it is never bogged down when the orchestrator is
//! busy. Each tick it claims due schedules and queued triggers (claim-and-lease, so one or two
//! replicas can run) and hands them off as work items. It never runs work itself.

use std::time::{Duration, Instant};

use kodehub_store::Store;

pub struct Scheduler {
    pub store: Store,
    pub owner: String,
    pub lease_secs: i64,
    /// How often the default cadence is rebuilt from settings and node lifecycles.
    pub replan_every: Duration,
    last_replan: Option<Instant>,
}

impl Scheduler {
    pub fn new(store: Store, owner: String) -> Self {
        Scheduler {
            store,
            owner,
            lease_secs: 60,
            replan_every: Duration::from_secs(600),
            last_replan: None,
        }
    }

    /// One tick: (re)plan default schedules when due, fire due schedules and triggers.
    /// Returns the number of work items handed off.
    pub async fn tick(&mut self) -> anyhow::Result<usize> {
        if self
            .last_replan
            .is_none_or(|t| t.elapsed() >= self.replan_every)
        {
            for p in self.store.list_projects().await? {
                if let Some(id) = p["id"].as_str() {
                    if let Err(e) = self.store.ensure_default_schedules(id).await {
                        tracing::warn!("schedules for {id}: {e}");
                    }
                }
            }
            self.last_replan = Some(Instant::now());
        }
        let mut n = self
            .store
            .fire_due_schedules(&self.owner, self.lease_secs)
            .await?
            .len();
        n += self.store.fire_triggers(&self.owner).await?.len();
        Ok(n)
    }
}
