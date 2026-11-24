//! S6 acceptance: claim-and-lease, caps, dependencies, retries and escalation, schedules and
//! triggers on competing replicas, intents (absorb / sequence / negotiate), budgets.
use std::collections::BTreeMap;

use kodehub_store::queue::NewWorkItem;
use kodehub_store::testutil::temp_store;
use kodehub_store::Store;
use serde_json::json;

async fn admin(s: &Store) -> kodehub_store::Principal {
    s.register_user("alice", "password123", "Alice")
        .await
        .unwrap();
    let (t, _) = s.login("alice", "password123").await.unwrap();
    s.authenticate(&t).await.unwrap()
}

fn item(project: &str, agent: &str, target: &str) -> NewWorkItem {
    NewWorkItem {
        project: project.into(),
        agent_type: agent.into(),
        kind: "implement".into(),
        target: Some(target.into()),
        task: json!({"do": target}),
        ..Default::default()
    }
}

#[tokio::test]
async fn competing_claimers_caps_leases_retries() {
    let s = temp_store("s6q").await.unwrap();
    let a = admin(&s).await;
    let p = s.project_id().unwrap();
    s.set_setting(
        &a,
        &format!("project:{p}"),
        "agent_caps",
        Some(json!({"coding": 100, "test_author": 2, "architect": 1})),
    )
    .await
    .unwrap();
    for i in 0..12 {
        s.enqueue(&item(&p, "coding", &format!("symbols/s{i}")))
            .await
            .unwrap();
    }
    // 8 competing claimers drain the queue: every item claimed exactly once
    let mut handles = Vec::new();
    for r in 0..8 {
        let s2 = s.clone();
        handles.push(tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(w) = s2
                .claim_next(&format!("replica-{r}"), &["coding".into()], 60)
                .await
                .unwrap()
            {
                got.push(w["_key"].as_str().unwrap().to_string());
            }
            got
        }));
    }
    let mut all: Vec<String> = Vec::new();
    for h in handles {
        all.extend(h.await.unwrap());
    }
    let n = all.len();
    all.sort();
    all.dedup();
    assert_eq!(n, 12, "every item claimed");
    assert_eq!(all.len(), 12, "no item claimed twice");

    // caps: at most 2 test_author agents at once
    for i in 0..5 {
        s.enqueue(&item(&p, "test_author", &format!("components/c{i}")))
            .await
            .unwrap();
    }
    let t1 = s
        .claim_next("o", &["test_author".into()], 60)
        .await
        .unwrap()
        .unwrap();
    let _t2 = s
        .claim_next("o", &["test_author".into()], 60)
        .await
        .unwrap()
        .unwrap();
    assert!(
        s.claim_next("o", &["test_author".into()], 60)
            .await
            .unwrap()
            .is_none(),
        "cap of 2"
    );
    s.complete_work_item(
        t1["_key"].as_str().unwrap(),
        "o",
        json!({"changed": []}),
        json!({}),
    )
    .await
    .unwrap();
    assert!(
        s.claim_next("o", &["test_author".into()], 60)
            .await
            .unwrap()
            .is_some(),
        "a slot freed"
    );

    // a dead worker's lease expires and the item becomes claimable again
    let w = s
        .enqueue(&item(&p, "coding", "symbols/lease"))
        .await
        .unwrap();
    let k = w["_key"].as_str().unwrap().to_string();
    let c = s
        .claim_next("dead", &["coding".into()], 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c["_key"], json!(k));
    assert!(s
        .claim_next("other", &["coding".into()], 60)
        .await
        .unwrap()
        .is_none());
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    let c2 = s
        .claim_next("other", &["coding".into()], 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c2["_key"], json!(k));
    assert!(
        s.complete_work_item(&k, "dead", json!({}), json!({}))
            .await
            .is_err(),
        "the lease was lost"
    );
    assert!(s.heartbeat(&k, "other", 60).await.unwrap());

    // dependencies: B waits for A
    let ia = s
        .enqueue(&item(&p, "architect", "philosophy/philosophy"))
        .await
        .unwrap();
    let mut b = item(&p, "architect", "contexts/x");
    b.depends_on = vec![ia["_key"].as_str().unwrap().into()];
    let ib = s.enqueue(&b).await.unwrap();
    let first = s
        .claim_next("o", &["architect".into()], 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first["_key"], ia["_key"]);
    s.complete_work_item(first["_key"].as_str().unwrap(), "o", json!({}), json!({}))
        .await
        .unwrap();
    let second = s
        .claim_next("o", &["architect".into()], 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second["_key"], ib["_key"]);
    s.complete_work_item(second["_key"].as_str().unwrap(), "o", json!({}), json!({}))
        .await
        .unwrap();

    // retries carry history; the third failed rework cycle goes to an architect
    s.set_setting(&a, &format!("project:{p}"), "retry_limit", Some(json!(3)))
        .await
        .unwrap();
    let mut cur = s
        .enqueue(&item(&p, "coding", "symbols/hard"))
        .await
        .unwrap();
    for attempt in 1..=3 {
        let c = loop {
            let c = s
                .claim_next("o", &["coding".into(), "architect".into()], 60)
                .await
                .unwrap()
                .unwrap();
            if c["_key"] == cur["_key"] {
                break c;
            }
            s.complete_work_item(c["_key"].as_str().unwrap(), "o", json!({}), json!({}))
                .await
                .unwrap();
        };
        let next = s
            .fail_work_item(
                c["_key"].as_str().unwrap(),
                "o",
                &format!("attempt {attempt} failed"),
                json!({"tried": attempt}),
            )
            .await
            .unwrap();
        assert_eq!(
            next["history"].as_array().unwrap().len(),
            attempt,
            "full history carried"
        );
        assert_eq!(next["priority"], 100, "rework is high priority");
        if attempt < 3 {
            assert_eq!(next["agent_type"], "coding");
            assert_eq!(next["retry_count"], attempt);
        } else {
            assert_eq!(
                next["agent_type"], "architect",
                "escalated after the retry limit"
            );
            assert_eq!(next["kind"], "resolve_hard_failure");
        }
        cur = next;
    }

    // budget: a project at its budget claims nothing new
    s.set_setting(&a, &format!("project:{p}"), "budget_usd", Some(json!(0.01)))
        .await
        .unwrap();
    s.record_usage(&p, None, "coding", "claude-sonnet-5-5", 10_000, 10_000)
        .await
        .unwrap();
    assert!(s.project_at_budget(&p).await.unwrap());
    s.enqueue(&item(&p, "coding", "symbols/after-budget"))
        .await
        .unwrap();
    assert!(s
        .claim_next("o", &["coding".into()], 60)
        .await
        .unwrap()
        .is_none());
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn schedules_triggers_and_intents() {
    let s = temp_store("s6s").await.unwrap();
    let a = admin(&s).await;
    let p = s.project_id().unwrap();
    // two "replicas" fire due schedules concurrently: each fires once
    for i in 0..6 {
        let sc = s
            .upsert_schedule(&p, &format!("components/c{i}"), "component", 7200, "test")
            .await
            .unwrap();
        // make it due now
        s.db.aql_raw(
            "UPDATE @k WITH {next_due: '2000-01-01T00:00:00.000Z'} IN kodehub_schedules",
            json!({"k": sc["_key"]}),
        )
        .await
        .unwrap();
    }
    let (r1, r2) = tokio::join!(
        s.fire_due_schedules("sched-1", 60),
        s.fire_due_schedules("sched-2", 60)
    );
    let (r1, r2) = (r1.unwrap(), r2.unwrap());
    assert_eq!(
        r1.len() + r2.len(),
        6,
        "each due schedule fired exactly once"
    );
    let again = s.fire_due_schedules("sched-1", 60).await.unwrap();
    assert!(again.is_empty(), "next run is an interval later");
    let scheds = s.list_schedules(&p).await.unwrap();
    for sc in &scheds {
        let last = chrono::DateTime::parse_from_rfc3339(sc["last_run"].as_str().unwrap()).unwrap();
        let next = chrono::DateTime::parse_from_rfc3339(sc["next_due"].as_str().unwrap()).unwrap();
        assert_eq!((next - last).num_seconds(), 7200, "based on the last run");
    }
    let items = s
        .list_work_items(Some(&p), Some("queued"), 100)
        .await
        .unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|w| w["agent_type"] == "test_run")
            .count(),
        6
    );
    // the daily project run is always kept
    let daily = s
        .upsert_schedule(&p, "project", "project", 86400, "default")
        .await
        .unwrap();
    assert!(s
        .set_schedule_enabled(&p, daily["_key"].as_str().unwrap(), false)
        .await
        .is_err());
    // triggers ("Test now", a version landed) fire once across replicas
    s.trigger(&p, "test_now", "components/c1", "alice")
        .await
        .unwrap();
    let (t1, t2) = tokio::join!(s.fire_triggers("a"), s.fire_triggers("b"));
    assert_eq!(t1.unwrap().len() + t2.unwrap().len(), 1);

    // intents: large overlap absorbs, small overlap sequences, shared end state negotiates
    s.set_setting(
        &a,
        &format!("project:{p}"),
        "agent_caps",
        Some(json!({"coding": 10})),
    )
    .await
    .unwrap();
    let nodes =
        |r: std::ops::Range<i32>| -> Vec<String> { r.map(|i| format!("symbols/n{i}")).collect() };
    let w1 = s
        .enqueue(&item(&p, "coding", "components/a"))
        .await
        .unwrap();
    let w2 = s
        .enqueue(&item(&p, "coding", "components/a"))
        .await
        .unwrap();
    let w3 = s
        .enqueue(&item(&p, "coding", "components/b"))
        .await
        .unwrap();
    let w4 = s
        .enqueue(&item(&p, "coding", "components/c"))
        .await
        .unwrap();
    let mut claimed = BTreeMap::new();
    for _ in 0..4 {
        let c = s
            .claim_next("o", &["coding".into()], 60)
            .await
            .unwrap()
            .unwrap();
        claimed.insert(c["_key"].as_str().unwrap().to_string(), c);
    }
    let k = |w: &serde_json::Value| w["_key"].as_str().unwrap().to_string();
    let d1 = s
        .register_intent(&k(&w1), &nodes(0..20), "edit a", false)
        .await
        .unwrap();
    assert_eq!(d1["decision"], "proceed");
    // 20 nodes each, 18 shared → 18/22 = 0.82 ≥ 0.6 → absorbed
    let d2 = s
        .register_intent(&k(&w2), &nodes(2..22), "edit a too", false)
        .await
        .unwrap();
    assert_eq!(d2["decision"], "absorbed", "{d2}");
    assert_eq!(d2["into"], json!(k(&w1)));
    let surv = s.work_item(&k(&w1)).await.unwrap();
    assert_eq!(surv["combined_with"], json!([k(&w2)]));
    assert_eq!(s.work_item(&k(&w2)).await.unwrap()["state"], "absorbed");
    // 1 shared of 21 → sequenced behind w1
    let d3 = s
        .register_intent(
            &k(&w3),
            &["symbols/n0".to_string(), "symbols/x1".into()],
            "touch n0",
            false,
        )
        .await
        .unwrap();
    assert_eq!(d3["decision"], "sequenced", "{d3}");
    let w3d = s.work_item(&k(&w3)).await.unwrap();
    assert_eq!(w3d["state"], "queued", "re-queued, not idle");
    assert!(w3d["depends_on"]
        .as_array()
        .unwrap()
        .contains(&json!(k(&w1))));
    // shared end state → negotiation; deadlock after the round limit → architect
    let d4 = s
        .register_intent(
            &k(&w4),
            &["symbols/n1".to_string()],
            "change n1 signature",
            true,
        )
        .await
        .unwrap();
    assert_eq!(d4["decision"], "negotiate");
    let neg = d4["negotiation"].as_str().unwrap().to_string();
    let mut last = json!({});
    for round in 0..3 {
        s.negotiate(
            &p,
            &neg,
            &k(&w1),
            &json!({"sig": format!("a{round}")}),
            false,
        )
        .await
        .unwrap();
        last = s
            .negotiate(
                &p,
                &neg,
                &k(&w4),
                &json!({"sig": format!("b{round}")}),
                false,
            )
            .await
            .unwrap();
    }
    assert_eq!(last["state"], "deadlocked");
    let arch = s
        .work_item(last["architect"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(arch["agent_type"], "architect");
    // completing the survivor completes what it absorbed (counted once)
    s.complete_work_item(&k(&w1), "o", json!({"changed": nodes(0..22)}), json!({}))
        .await
        .unwrap();
    assert_eq!(s.work_item(&k(&w2)).await.unwrap()["state"], "done");
    // the sequenced item is now claimable
    let next = s
        .claim_next("o", &["coding".into()], 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next["_key"], json!(k(&w3)));
    s.drop_db().await.unwrap();
}
