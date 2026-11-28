//! S2 acceptance (store level): users, agent users, tokens, orgs, permissions, layered settings,
//! credentials, audit.
use kodehub_store::audit::AuditFilter;
use kodehub_store::identity::{Owner, Permission};
use kodehub_store::testutil::temp_store;
use serde_json::json;

async fn human(s: &kodehub_store::Store, name: &str) -> kodehub_store::Principal {
    s.register_user(name, "password123", name).await.unwrap();
    let (tok, _) = s.login(name, "password123").await.unwrap();
    s.authenticate(&tok).await.unwrap()
}

#[tokio::test]
async fn users_tokens_and_permissions() {
    let s = temp_store("s2id").await.unwrap();
    let alice = human(&s, "alice").await;
    assert!(alice.is_admin, "first human is the system admin");
    let bob = human(&s, "bob").await;
    assert!(!bob.is_admin);
    assert!(s.login("alice", "wrong-password").await.is_err());

    // the ownerless first project went to alice
    let first = s.project_id().unwrap();
    assert_eq!(
        s.project_permission(&alice, &first).await.unwrap(),
        Some(Permission::Admin)
    );
    assert_eq!(
        s.project_permission(&bob, &first).await.unwrap(),
        None,
        "private by default"
    );

    let p = s
        .create_owned_project(&alice, "Alpha", "", None)
        .await
        .unwrap();
    let alpha = p["id"].as_str().unwrap().to_string();
    assert!(s
        .visible_projects(&bob)
        .await
        .unwrap()
        .iter()
        .all(|x| x["id"] != alpha.as_str()));

    // share read with bob
    s.share_project(
        &alice,
        &alpha,
        &Owner::User("bob".into()),
        Some(Permission::Read),
    )
    .await
    .unwrap();
    assert_eq!(
        s.project_permission(&bob, &alpha).await.unwrap(),
        Some(Permission::Read)
    );
    assert!(
        s.share_project(
            &bob,
            &alpha,
            &Owner::User("bob".into()),
            Some(Permission::Admin)
        )
        .await
        .is_err(),
        "a reader cannot escalate"
    );

    // an agent user acting for bob never exceeds bob
    let agent = s
        .create_agent_user(&bob, "Claude Code", None)
        .await
        .unwrap();
    let ak = agent["key"].as_str().unwrap().to_string();
    let (pat, rec) = s
        .issue_token(&bob, &ak, "laptop", None, Permission::Admin, Some(7))
        .await
        .unwrap();
    assert!(pat.starts_with("khp_"));
    // only the hash is stored
    let all = s.aql_global("FOR t IN tokens RETURN t", json!({})).await;
    assert!(!serde_json::to_string(&all).unwrap().contains(&pat));
    assert_eq!(rec["prefix"].as_str().unwrap(), &pat[..12]);
    let ap = s.authenticate(&pat).await.unwrap();
    assert!(!ap.is_human());
    assert_eq!(ap.on_behalf_of().as_deref(), Some("users/bob"));
    assert_eq!(
        s.project_permission(&ap, &alpha).await.unwrap(),
        Some(Permission::Read)
    );
    // bob loses access → agent loses it too
    s.share_project(&alice, &alpha, &Owner::User("bob".into()), None)
        .await
        .unwrap();
    assert_eq!(s.project_permission(&ap, &alpha).await.unwrap(), None);

    // project-scoped token
    let ag = s.create_agent_user(&alice, "kh", None).await.unwrap();
    let (pat2, _) = s
        .issue_token(
            &alice,
            ag["key"].as_str().unwrap(),
            "ci",
            Some(vec![first.clone()]),
            Permission::Write,
            None,
        )
        .await
        .unwrap();
    let ap2 = s.authenticate(&pat2).await.unwrap();
    assert_eq!(
        s.project_permission(&ap2, &first).await.unwrap(),
        Some(Permission::Write)
    );
    assert_eq!(s.project_permission(&ap2, &alpha).await.unwrap(), None);

    // revocation is immediate
    let tid = s.list_tokens(&bob).await.unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.revoke_token(&bob, &tid).await.unwrap();
    assert!(s.authenticate(&pat).await.is_err());
    // agents cannot create tokens
    assert!(s.create_agent_user(&ap2, "nested", None).await.is_err());
    s.drop_db().await.unwrap();
}

trait AqlGlobal {
    async fn aql_global(&self, q: &str, b: serde_json::Value) -> Vec<serde_json::Value>;
}
impl AqlGlobal for kodehub_store::Store {
    async fn aql_global(&self, q: &str, b: serde_json::Value) -> Vec<serde_json::Value> {
        self.db.aql_raw(q, b).await.unwrap()
    }
}

#[tokio::test]
async fn orgs_settings_credentials_audit() {
    let s = temp_store("s2org").await.unwrap();
    std::env::set_var(
        "KODEHUB_HOME",
        std::env::temp_dir().join(format!("kh-s2-{}", uuid::Uuid::new_v4().simple())),
    );
    let alice = human(&s, "alice").await;
    let carol = human(&s, "carol").await;
    let org = s.create_org(&alice, "Acme").await.unwrap();
    let o = org["_key"].as_str().unwrap().to_string();
    s.set_org_member(&alice, &o, "carol", "member")
        .await
        .unwrap();
    assert!(s
        .set_org_member(&carol, &o, "carol", "admin")
        .await
        .is_err());

    let p = s
        .create_owned_project(&alice, "Regulated", "", Some(&o))
        .await
        .unwrap();
    let rp = p["id"].as_str().unwrap().to_string();
    assert_eq!(
        s.project_permission(&carol, &rp).await.unwrap(),
        Some(Permission::Write)
    );
    let mine = s
        .create_owned_project(&carol, "Home", "", None)
        .await
        .unwrap();
    let hp = mine["id"].as_str().unwrap().to_string();

    // settings: app default 3 → org 3 → project 1
    assert_eq!(s.setting(&rp, "retry_limit").await.unwrap(), json!(3));
    s.set_setting(&alice, &format!("org:{o}"), "retry_limit", Some(json!(5)))
        .await
        .unwrap();
    assert_eq!(s.setting(&rp, "retry_limit").await.unwrap(), json!(5));
    s.set_setting(
        &alice,
        &format!("project:{rp}"),
        "retry_limit",
        Some(json!(1)),
    )
    .await
    .unwrap();
    let r = s.resolve_settings(&rp, None).await.unwrap();
    assert_eq!(r["retry_limit"]["value"], json!(1));
    assert_eq!(r["retry_limit"]["source"], json!(format!("project:{rp}")));
    // org-owned project does not fall through to a contributing user's value
    s.set_setting(&carol, "user:carol", "blind_review", Some(json!(false)))
        .await
        .unwrap();
    assert_eq!(s.setting(&rp, "blind_review").await.unwrap(), json!(true));
    // ...but a user-owned project inherits its owner's
    assert_eq!(s.setting(&hp, "blind_review").await.unwrap(), json!(false));
    assert!(s
        .set_setting(&carol, &format!("org:{o}"), "retry_limit", Some(json!(9)))
        .await
        .is_err());
    assert!(s
        .set_setting(
            &alice,
            &format!("project:{rp}"),
            "retry_limit",
            Some(json!(-2))
        )
        .await
        .is_err());

    // credentials: subscription tokens are personal only
    assert!(s
        .put_credential(
            &alice,
            &format!("org:{o}"),
            "anthropic",
            "subscription_token",
            "sk-ant-oat01-x",
            ""
        )
        .await
        .is_err());
    assert!(s
        .put_credential(
            &alice,
            &format!("project:{rp}"),
            "anthropic",
            "subscription_token",
            "sk-ant-oat01-x",
            ""
        )
        .await
        .is_err());
    s.put_credential(
        &carol,
        "user:carol",
        "anthropic",
        "subscription_token",
        "sk-ant-oat01-carol",
        "home",
    )
    .await
    .unwrap();
    s.put_credential(
        &alice,
        &format!("org:{o}"),
        "anthropic",
        "api_key",
        "sk-ant-api03-acme",
        "",
    )
    .await
    .unwrap();
    // org project: API key only; personal project: subscription token for claude_code
    assert!(s
        .resolve_model_credential(&rp, None, "claude_code")
        .await
        .unwrap()
        .is_none());
    let k = s
        .resolve_model_credential(&rp, None, "api")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(k.expose(), "sk-ant-api03-acme");
    let t = s
        .resolve_model_credential(&hp, None, "claude_code")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.kind, "subscription_token");
    // the subscription token is never offered to the API backend
    assert!(s
        .resolve_model_credential(&hp, None, "api")
        .await
        .unwrap()
        .is_none());
    // no plaintext anywhere in the database
    let dump = serde_json::to_string(
        &s.db
            .aql_raw("FOR c IN secrets RETURN c", json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!dump.contains("sk-ant"));
    // each secret points at what it secures (D-061)
    let secures =
        s.db.aql_raw("FOR e IN secures SORT e._to RETURN e._to", json!({}))
            .await
            .unwrap();
    assert_eq!(
        secures,
        vec![json!(format!("orgs/{o}")), json!("users/carol")]
    );

    // audit
    s.audit(
        Some(&alice),
        Some(&rp),
        "POST /api/x",
        200,
        json!({"password": "nope"}),
    )
    .await
    .unwrap();
    let rows = s
        .list_audit(&AuditFilter {
            project: Some(rp.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows[0]["actor"], json!("users/alice"));
    assert_eq!(rows[0]["actor_class"], json!("human"));
    assert!(!rows[0].to_string().contains("nope"));

    // the last admin leaving archives the org
    s.remove_org_member(&alice, &o, "alice").await.unwrap();
    assert_eq!(s.get_org(&o).await.unwrap()["archived"], json!(true));
    assert!(s
        .set_org_member(&alice, &o, "carol", "admin")
        .await
        .is_err());
    s.drop_db().await.unwrap();
}
