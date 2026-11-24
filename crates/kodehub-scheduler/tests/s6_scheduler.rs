//! S6 acceptance (scheduler binary logic): two scheduler replicas tick against the same
//! database; the default cadence is planned only for nodes ready for test, and every due run is
//! handed off once.
use kodehub_scheduler::Scheduler;
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use serde_json::json;

#[tokio::test]
async fn two_replicas_plan_and_fire_once() {
    let s = temp_store("s6sch").await.unwrap();
    import_dir(&s, &seed_dir()).await.unwrap();
    let p = s.project_id().unwrap();
    let mut a = Scheduler::new(s.clone(), "sched-a".into());
    let mut b = Scheduler::new(s.clone(), "sched-b".into());
    a.tick().await.unwrap();
    let scheds = s.list_schedules(&p).await.unwrap();
    // nothing has been tested yet: code with tests is ready for test, so components holding it
    // are scheduled; the project run always is
    let targets: Vec<&str> = scheds
        .iter()
        .map(|x| x["target"].as_str().unwrap())
        .collect();
    assert!(targets.contains(&"project"));
    assert!(
        targets.contains(&"components/credential-verification"),
        "{targets:?}"
    );
    let enabled = |t: &str| {
        scheds
            .iter()
            .find(|x| x["target"] == t)
            .map(|x| x["enabled"] == true)
    };
    // password-hashing holds the untested duplicate helper → not ready → not scheduled
    assert_ne!(enabled("components/password-hashing"), Some(true));
    // component cadence is 2 h by default
    let cv = scheds
        .iter()
        .find(|x| x["target"] == "components/credential-verification")
        .unwrap();
    assert_eq!(cv["interval_secs"], 7200);
    // make everything due; both replicas tick at once
    s.db.aql_raw(
        "FOR x IN kodehub_schedules UPDATE x WITH {next_due: '2000-01-01T00:00:00.000Z'} IN kodehub_schedules",
        json!({}),
    )
    .await
    .unwrap();
    let due = s
        .list_schedules(&p)
        .await
        .unwrap()
        .iter()
        .filter(|x| x["enabled"] == true)
        .count();
    let (na, nb) = tokio::join!(a.tick(), b.tick());
    assert_eq!(
        na.unwrap() + nb.unwrap(),
        due,
        "each due schedule fired exactly once"
    );
    let runs = s.list_work_items(Some(&p), None, 500).await.unwrap();
    assert_eq!(
        runs.iter()
            .filter(|w| w["agent_type"] == "test_run")
            .count(),
        due
    );
    s.drop_db().await.unwrap();
}
