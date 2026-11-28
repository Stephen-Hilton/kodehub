//! Ontology step 6 (D-061, D-062): approvals are the state of what awaits a human; agent types
//! are project data with permissions; schedules target nodes; negotiations are agent chats.
use kodehub_store::queue::NewWorkItem;
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use serde_json::json;

#[tokio::test]
async fn approvals_agents_schedules_and_chats() {
    let s = temp_store("o6gov").await.unwrap();
    import_dir(&s, &seed_dir()).await.unwrap();
    let p = s.project_id().unwrap();

    // a requirement change waits as a proposed version; the requirement is unchanged
    let a = s
        .request_approval(
            "requirement_change",
            json!({"requirement": "r2-hashed-storage", "patch": {"statement": "Passwords are stored only as Argon2id hashes with a pepper."}}),
            "peppers resist offline cracking",
            "agent:architect:wi-1",
        )
        .await
        .unwrap();
    assert_eq!(a["state"], "pending");
    assert!(a["_key"]
        .as_str()
        .unwrap()
        .starts_with("requirement_versions:"));
    let r2 =
        s.db.get("requirements", "r2-hashed-storage", None)
            .await
            .unwrap();
    assert_eq!(r2["revision"], 1);
    // a new context waits unplaced: it is in no project version
    let c = s
        .request_approval(
            "new_context",
            json!({"level": "context", "name": "Billing", "description": "Money"}),
            "the product charges users",
            "agent:planning:wi-2",
        )
        .await
        .unwrap();
    let ctx = c["node"].as_str().unwrap().to_string();
    assert!(s.tree(None).await.unwrap().version(&ctx).is_none());
    let pending = s.list_approvals(Some("pending")).await.unwrap();
    assert_eq!(pending.len(), 2);
    // a human approves both
    s.decide_approval(a["_key"].as_str().unwrap(), true, "users/alice", "ok")
        .await
        .unwrap();
    let r2 =
        s.db.get("requirements", "r2-hashed-storage", None)
            .await
            .unwrap();
    assert_eq!(r2["revision"], 2);
    assert!(r2["statement"].as_str().unwrap().contains("pepper"));
    assert_eq!(r2["at_risk"], true, "an approved change puts it at risk");
    s.decide_approval(c["_key"].as_str().unwrap(), true, "users/alice", "")
        .await
        .unwrap();
    assert!(
        s.tree(None).await.unwrap().version(&ctx).is_some(),
        "an approved context is placed in the current project version"
    );
    assert!(s.list_approvals(Some("pending")).await.unwrap().is_empty());
    // rejecting a proposed new requirement leaves it rejected, never accepted
    let n = s
        .request_approval(
            "requirement_change",
            json!({"requirement_doc": {"title": "Audit", "statement": "Every login is audited."}}),
            "",
            "agent:architect:wi-3",
        )
        .await
        .unwrap();
    s.decide_approval(n["_key"].as_str().unwrap(), false, "users/alice", "no")
        .await
        .unwrap();
    let cov = s.coverage(None).await.unwrap();
    assert!(cov.requirements.iter().all(|r| r.title != "Audit"));

    // agent types are project data with permissions; defaults are wide but separate
    let agents = s.list_agents().await.unwrap();
    assert_eq!(agents.len(), 8);
    assert!(s.agent_may("coding", "author_new_code").await.unwrap());
    assert!(s.agent_may("coding", "deploy").await.unwrap());
    assert!(!s.agent_may("coding", "review_code").await.unwrap());
    assert!(!s.agent_may("coding", "write_tests").await.unwrap());
    assert!(s.agent_may("review", "review_code").await.unwrap());
    // a project narrows or widens a type, and can change its start-up prompt
    s.put_agent(
        "coding",
        &json!({"permissions": ["plan_code", "modify_code"], "prompt": "Be brief."}),
        "users/alice",
    )
    .await
    .unwrap();
    assert!(!s.agent_may("coding", "author_new_code").await.unwrap());
    assert!(s
        .require_agent_permission("coding", "author_new_code")
        .await
        .is_err());
    assert!(s
        .put_agent("coding", &json!({"permissions": ["fly"]}), "users/alice")
        .await
        .is_err());
    let custom = s
        .put_agent(
            "custom:policy",
            &json!({"prompt": "Check licences.", "permissions": ["create_work_items"]}),
            "users/alice",
        )
        .await
        .unwrap();
    assert_eq!(custom["builtin"], false);
    let owns = s
        .aql(
            "RETURN COUNT(FOR e IN @@owns FILTER STARTS_WITH(e._to, 'agents/') RETURN 1)",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(owns[0], 9, "the project owns every agent type");

    // schedules are project data and point at what they act on
    let comp = s.db.qid("components/password-hashing").unwrap();
    let sch = s
        .upsert_schedule(&p, &comp, "component", 7200, "test")
        .await
        .unwrap();
    let targets = s
        .aql(
            "FOR e IN @@targets FILTER e._from == @s RETURN e._to",
            json!({"s": sch["_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(targets, vec![json!(comp)]);
    let daily = s
        .upsert_schedule(&p, "project", "project", 86400, "default")
        .await
        .unwrap();
    let t = s
        .aql(
            "FOR e IN @@targets FILTER e._from == @s RETURN e._to",
            json!({"s": daily["_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(t, vec![json!(s.project_node().unwrap())]);

    // intents are edges from the work item; a shared end state opens a negotiation chat
    let mk = |kind: &str| NewWorkItem {
        project: p.clone(),
        agent_type: "coding".into(),
        kind: kind.into(),
        ..Default::default()
    };
    let w1 = s.enqueue(&mk("a")).await.unwrap()["_key"]
        .as_str()
        .unwrap()
        .to_string();
    let w2 = s.enqueue(&mk("b")).await.unwrap()["_key"]
        .as_str()
        .unwrap()
        .to_string();
    let hp = s.symbol_id("hash_password").await.unwrap();
    let vp = s.symbol_id("verify_password").await.unwrap();
    s.register_intent(&w1, &[hp.clone(), vp.clone()], "edit", true)
        .await
        .unwrap();
    let ie = s
        .aql(
            "FOR e IN @@intends_changes FILTER e._from == CONCAT('work_items/', @w) SORT e._to RETURN e._to",
            json!({"w": w1}),
        )
        .await
        .unwrap();
    assert_eq!(ie.len(), 2);
    let d = s
        .register_intent(&w2, std::slice::from_ref(&hp), "edit too", true)
        .await
        .unwrap();
    assert_eq!(d["decision"], "negotiate");
    let chat = s
        .get_chat(d["negotiation"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(chat["kind"], "negotiation");
    let refs = chat["messages"][0]["refers_to"].as_array().unwrap();
    assert!(refs.contains(&json!(format!("work_items/{w1}"))));
    assert!(refs.contains(&json!(format!("work_items/{w2}"))));
    assert!(refs.contains(&json!(hp)));
    s.drop_db().await.unwrap();
}
