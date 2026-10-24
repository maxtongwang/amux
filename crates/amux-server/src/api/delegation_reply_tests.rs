// Real owner-input routes and reply-matching class sweep — refs maxtongwang/ai-config#160.
// Only process discovery and provider execution are fixtures; recorder/resolver/router are production.
use super::*;
use crate::api::{process_identity::{self, test_support, Origin}, settings::test_env, chat_worker};
use crate::db::{Store, WriteOutcome};
use axum::http::header;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use tower::ServiceExt;

const LEAD: &str = "review-reply-lead";
const TARGET: &str = "review-reply-worker";
const OUTSIDE: u16 = 42101;
const REMOTE: u16 = 42102;
const FORWARDED: u16 = 42103;
const PEER: u16 = 42104;
static SEQ: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    state: AppState,
    app: axum::Router,
    // Reverse dependency drop: resolver, environment, then scratch home.
    _resolver: test_support::ResolverGuard,
    _env: test_env::HomeGuard,
    home: tempfile::TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for session in [OWNER_AGENT,TARGET] { crate::api::pane_prompts::reset_test_deliveries(session); }
    }
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let env = test_env::set_home(home.path());
        let resolver = test_support::set_resolver(Arc::new(test_support::ByPort(std::collections::HashMap::from([
            (OUTSIDE, Origin::Outside), (REMOTE, Origin::Remote { forwarded: false }),
            (FORWARDED, Origin::Remote { forwarded: true }), (PEER, Origin::Worker(LEAD.into())),
        ]))));
        std::fs::create_dir_all(home.path().join("sessions")).unwrap();
        for (name,tags,orch) in [(OWNER_AGENT,"leads",false),(LEAD,"leads,dev",true),(TARGET,"dev",false)] {
            std::fs::write(home.path().join(format!("sessions/{name}.env")),format!(
                "CC_DIR=\"/tmp\"\nCC_INCARNATION=\"inc-{name}\"\nCC_TAGS=\"{tags}\"\nCC_ORCHESTRATOR=\"{}\"\nCC_PAUSED=\"1\"\nCC_TEST_FAKE_PANE=\"1\"\n", u8::from(orch))).unwrap();
        }
        let state = AppState { store: Arc::new(Store::open(&home.path().join("test.db")).unwrap()),
            started: std::time::Instant::now(), build_hash: "test".into(), auth_token: None,
            reconciled: Arc::new(std::sync::atomic::AtomicBool::new(true)) };
        for session in [OWNER_AGENT,TARGET] { crate::api::pane_prompts::reset_test_deliveries(session); }
        let app = crate::api::router(state.clone());
        Self { state, app, _resolver: resolver, _env: env, home }
    }

    async fn call(&self, port: Option<u16>, actor: Option<&str>, path: &str, body: Value) -> (StatusCode, Value) {
        let mut b = Request::builder().method("POST").uri(path).header(header::CONTENT_TYPE,"application/json");
        if let Some(a) = actor { b = b.header("x-amux-session",a); }
        let mut req = b.body(Body::from(body.to_string())).unwrap();
        if let Some(p) = port {
            req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127,0,0,1],p))));
        }
        let res = self.app.clone().oneshot(req).await.unwrap();
        let st = res.status();
        let bytes = axum::body::to_bytes(res.into_body(),usize::MAX).await.unwrap();
        (st,serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn ask(&self) -> String {
        let (st,v) = self.call(None,Some(LEAD),"/api/delegation/requests",json!({
            "method":"PATCH","path":format!("/api/sessions/{TARGET}/config"),"body":{"tags":""}})).await;
        assert_eq!(st,StatusCode::ACCEPTED,"{v}");
        assert_eq!(v["reply_session"],OWNER_AGENT);
        v["request_id"].as_str().unwrap().into()
    }

    async fn send(&self, session: &str, port: u16, text: &str, id: &str) -> (StatusCode,Value) {
        self.call(Some(port),None,&format!("/api/sessions/{session}/send"),json!({
            "text":text,"msg_id":id,"record_history":true,"no_board":true})).await
    }

    fn write(&self, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<()> + Send + 'static) {
        self.state.store.write(move |c| { f(c)?; Ok(WriteOutcome { applied:true, events:vec![] }) }).unwrap();
    }

    fn request_resolution(&self, id: &str) -> (Option<i64>,Option<String>) {
        self.state.store.read().unwrap().query_row(
            "SELECT resolution_row,resolution_code FROM _amux_delegation_grants WHERE code=?1",[id],
            |r|Ok((r.get(0)?,r.get(1)?))).unwrap()
    }

    // No API GET/list/apply: those can hide a severed recorder wire by reconciling it.
    fn resolution(&self, session: &str, code: &str, request: Option<&str>) -> i64 {
        let c = self.state.store.read().unwrap();
        let row: i64 = c.query_row("SELECT MAX(id) FROM cmd_history WHERE session=?1 AND COALESCE(delegation_message_type,type) IN ('user','chat')",[session],|r|r.get(0)).unwrap();
        let actual: (String,Option<String>) = c.query_row("SELECT code,request_id FROM _amux_delegation_resolutions WHERE reply_row=?1",[row],|r|Ok((r.get(0)?,r.get(1)?)))
            .expect("real route must commit resolution before returning");
        assert_eq!(actual,(code.into(),request.map(str::to_string)));
        if let Some(id) = request { assert_eq!(self.request_resolution(id),(Some(row),Some(code.into()))); }
        row
    }

    async fn apply(&self, id: &str) -> (StatusCode,Value) {
        let req=Request::builder().method("PATCH").uri(format!("/api/sessions/{TARGET}/config"))
            .header(header::CONTENT_TYPE,"application/json").header("x-amux-session",LEAD)
            .body(Body::from(json!({"tags":"","request_id":id}).to_string())).unwrap();
        let res=self.app.clone().oneshot(req).await.unwrap(); let st=res.status();
        let bytes=axum::body::to_bytes(res.into_body(),usize::MAX).await.unwrap();
        (st,serde_json::from_slice(&bytes).unwrap())
    }

    fn report(&self, text: &str) -> Value {
        let seq = SEQ.fetch_add(1,Ordering::SeqCst);
        json!({"native_status":true,"event":"UserPromptSubmit","state":"active","run_id":"reply-run",
            "provider":"claude","sequence":seq,"event_ts":crate::config::now_f64()-0.5,"turn_id":format!("reply-{seq}"),"prompt":text})
    }

    async fn hook(&self, session: &str, report: Value) -> (StatusCode,Value) {
        let d = self.home.path().join("status-events").join(session);
        std::fs::create_dir_all(&d).unwrap();
        if !d.join("current.json").exists() {
            std::fs::write(d.join("current.json"),json!({"run_id":"reply-run","provider":"claude","started":crate::config::now_f64()-100.0}).to_string()).unwrap();
        }
        let result=self.call(None,Some(session),&format!("/api/sessions/{session}/report"),report).await;
        assert!(result.0.is_success(),"hook HTTP failure: {result:?}");
        result
    }
}

fn accepted(result: (StatusCode,Value)) {
    assert!(result.0.is_success() && result.1["ok"] != false,"{result:?}");
}

#[tokio::test]
async fn dashboard_send_resolves_in_owner_session() {
    let f=Fixture::new(); let id=f.ask().await;
    let (st,v)=f.call(Some(OUTSIDE),None,&format!("/api/sessions/{OWNER_AGENT}/send"),json!({
        "text":"yes","record_history":true,"no_board":true,"msg_id":"dashboard-reply","client_meta":{"surface":"dashboard"}})).await;
    accepted((st,v)); f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn owner_cli_send_resolves_in_owner_session() {
    let f=Fixture::new(); let id=f.ask().await;
    // Exact Bash CLI endpoint and payload: no record_history flag.
    accepted(f.call(Some(OUTSIDE),None,&format!("/api/workers/{OWNER_AGENT}/send"),json!({
        "text":"yes","msg_id":"cli-reply","no_board":true})).await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn remote_phone_send_resolves_in_owner_session_only() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.send(TARGET,REMOTE,"yes","phone-wrong-session").await);
    f.resolution(TARGET,"request_reply_not_owner_session",None);
    assert_eq!(f.request_resolution(&id),(None,None));
    accepted(f.send(OWNER_AGENT,REMOTE,"approve","phone-reply").await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn chat_companion_send_resolves_in_owner_session() {
    let f=Fixture::new(); let id=f.ask().await;
    let _turn=chat_worker::hold_test_turn(&format!("{OWNER_AGENT}@chat"));
    accepted(f.send(&format!("{OWNER_AGENT}@chat"),OUTSIDE,"yes","companion-reply").await);
    let row=f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
    assert_eq!(f.state.store.read().unwrap().query_row("SELECT type FROM cmd_history WHERE id=?1",[row],|r|r.get::<_,String>(0)).unwrap(),"chat");
}

#[tokio::test]
async fn chat_type_worker_send_resolves_in_owner_session() {
    let f=Fixture::new(); let id=f.ask().await;
    let p=f.home.path().join(format!("sessions/{OWNER_AGENT}.env"));
    let env=std::fs::read_to_string(&p).unwrap().replace("CC_TEST_FAKE_PANE=\"1\"","CC_WORKER_TYPE=\"chat\"").replace("CC_PAUSED=\"1\"","CC_PAUSED=\"0\"");
    std::fs::write(p,env).unwrap();
    let _turn=chat_worker::hold_test_turn(OWNER_AGENT);
    accepted(f.send(OWNER_AGENT,OUTSIDE,"yes","chat-worker-reply").await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn recorded_steer_resolves_in_owner_session() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.call(Some(OUTSIDE),None,&format!("/api/sessions/{OWNER_AGENT}/steer"),json!({
        "text":"yes","record_history":true,"no_board":true,"msg_id":"steer-reply"})).await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn project_send_resolves_in_owner_session() {
    let f=Fixture::new(); let id=f.ask().await;
    let p=f.home.path().join(format!("sessions/{OWNER_AGENT}.env"));
    let env=std::fs::read_to_string(&p).unwrap()+"CC_PROJECT=\"reply-project\"\nCC_BOARD_CARD=\"REPLY-1\"\n";
    std::fs::write(p,env).unwrap();
    f.write(|c| {
        let execution=crate::project_execution::planner::Execution { worker:OWNER_AGENT.into(),stage:"working".into(),..Default::default() };
        c.execute("INSERT INTO issues(id,title,status,type,project_group,session,created,updated,execution_state) VALUES('REPLY-1','Reply fixture','inprogress','code','reply-project',?1,1,1,?2)",rusqlite::params![OWNER_AGENT,serde_json::to_string(&execution).unwrap()])?;
        Ok(())
    });
    accepted(f.send(OWNER_AGENT,OUTSIDE,"yes","project-reply").await);
    let row=f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
    assert_eq!(f.state.store.read().unwrap().query_row("SELECT delivery FROM cmd_history WHERE id=?1",[row],|r|r.get::<_,String>(0)).unwrap(),"queued");
}

#[tokio::test]
async fn pane_and_remote_control_in_another_session_fund_nothing() {
    let f=Fixture::new(); let id=f.ask().await;
    let result=f.hook(TARGET,f.report("yes")).await;
    accepted(result.clone()); assert_eq!(result.1["applied"],true);
    f.resolution(TARGET,"request_reply_not_owner_session",None);
    assert_eq!(f.request_resolution(&id),(None,None));
    accepted(f.hook(OWNER_AGENT,f.report("approve")).await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn asker_cannot_choose_reply_session() {
    let f=Fixture::new();
    let (st,_)=f.call(None,Some(LEAD),"/api/delegation/requests",json!({"reply_session":TARGET,
        "method":"PATCH","path":format!("/api/sessions/{TARGET}/config"),"body":{"tags":""}})).await;
    assert_eq!(st,StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_grants",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[tokio::test]
async fn peer_echo_and_same_text_rows_never_become_owner_replies() {
    for text in ["yes","no"] {
        let f=Fixture::new(); let id=f.ask().await;
        accepted(f.send(OWNER_AGENT,PEER,text,&format!("peer-{text}")).await);
        assert_eq!(f.request_resolution(&id),(None,None));
        assert_eq!(f.state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_resolutions",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        // A re-recorded hook copy of the peer's words is rejected by the production classifier.
        accepted(f.hook(OWNER_AGENT,f.report(text)).await);
        assert_eq!(f.request_resolution(&id),(None,None));
        accepted(f.hook(OWNER_AGENT,f.report("go ahead")).await);
        f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
    }
}

#[tokio::test]
async fn replayed_send_and_hook_copy_cannot_fund_a_second_request() {
    let f=Fixture::new(); let first=f.ask().await;
    accepted(f.send(OWNER_AGENT,OUTSIDE,"yes","same-reply").await);
    let row=f.resolution(OWNER_AGENT,"request_resolved",Some(&first));
    // Re-observing this already recorded row is idempotent too. The initial
    // observation above came through /send, not a handcrafted history row.
    crate::api::delegation_grants::on_row(&f.state,row).await;
    let second=f.ask().await;
    accepted(f.send(OWNER_AGENT,OUTSIDE,"yes","same-reply").await);
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    assert_eq!(f.request_resolution(&second),(None,None));
    assert_eq!(f.state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_resolutions WHERE reply_row=?1",[row],|r|r.get::<_,i64>(0)).unwrap(),1);
}

#[tokio::test]
async fn replayed_hook_event_cannot_fund_a_second_request() {
    let f=Fixture::new(); let first=f.ask().await; let report=f.report("approve");
    accepted(f.hook(OWNER_AGENT,report.clone()).await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&first));
    let second=f.ask().await;
    accepted(f.hook(OWNER_AGENT,report).await);
    assert_eq!(f.request_resolution(&second),(None,None));
}

#[tokio::test]
async fn reply_before_question_and_id_ties_fund_nothing() {
    let f=Fixture::new(); accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    f.resolution(OWNER_AGENT,"request_no_candidate",None);
    let id=f.ask().await; assert_eq!(f.request_resolution(&id),(None,None));
    drop(f);
    // Test a question shown at the reply row itself, then strictly after it.
    for offset in [1i64,2] {
        let f=Fixture::new(); let id=f.ask().await;
        let code=id.clone(); f.write(move|c| {
            c.execute("UPDATE _amux_delegation_grants SET shown_row=(SELECT MAX(id)+?2 FROM cmd_history) WHERE code=?1",rusqlite::params![code,offset])?; Ok(())
        });
        accepted(f.hook(OWNER_AGENT,f.report("approve")).await);
        f.resolution(OWNER_AGENT,"request_no_candidate",None);
        assert_eq!(f.request_resolution(&id),(None,None));
    }
}

#[tokio::test]
async fn clock_reversal_and_expiry_refuse_even_with_increasing_row_ids() {
    for delta_ms in [60_000i64,-7_201_000] {
        let f=Fixture::new(); let id=f.ask().await; let code=id.clone();
        f.write(move|c| { c.execute("UPDATE cmd_history SET ts=ts+?2 WHERE id=(SELECT shown_row FROM _amux_delegation_grants WHERE code=?1)",rusqlite::params![code,delta_ms])?; Ok(()) });
        accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
        f.resolution(OWNER_AGENT,"request_reply_outside_window",None);
        assert_eq!(f.request_resolution(&id),(None,Some("request_reply_outside_window".into())));
    }
}

#[tokio::test]
async fn intervening_question_is_ambiguous_but_peer_notice_is_not() {
    let f=Fixture::new(); let a=f.ask().await; let b=f.ask().await;
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    f.resolution(OWNER_AGENT,"request_ambiguous",None);
    for id in [a,b] { assert_eq!(f.request_resolution(&id),(None,Some("request_ambiguous".into()))); }
    let id=f.ask().await;
    accepted(f.send(OWNER_AGENT,PEER,"status notice","peer-notice").await);
    accepted(f.hook(OWNER_AGENT,f.report("approve")).await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn negations_waits_and_questions_never_fund_through_recorder() {
    for text in ["no","don't","not yet","wait","yes?","yes but not yet","yes, don't proceed"] {
        let f=Fixture::new(); let id=f.ask().await;
        accepted(f.hook(OWNER_AGENT,f.report(text)).await);
        f.resolution(OWNER_AGENT,"request_reply_not_affirmative",None);
        assert_eq!(f.request_resolution(&id),(None,Some("request_reply_not_affirmative".into())));
    }
}

#[tokio::test]
async fn replaced_owner_incarnation_cannot_resolve_old_question() {
    let f=Fixture::new(); let id=f.ask().await;
    let p=f.home.path().join(format!("sessions/{OWNER_AGENT}.env"));
    std::fs::write(&p,std::fs::read_to_string(&p).unwrap().replace(&format!("inc-{OWNER_AGENT}"),"replacement-owner")).unwrap();
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    f.resolution(OWNER_AGENT,"request_reply_identity_changed",None);
    assert_eq!(f.request_resolution(&id),(None,Some("request_reply_identity_changed".into())));
}

#[tokio::test]
async fn forwarded_remote_owner_send_keeps_owner_session_boundary() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.send(TARGET,FORWARDED,"yes","forward-wrong").await);
    f.resolution(TARGET,"request_reply_not_owner_session",None);
    accepted(f.send(OWNER_AGENT,FORWARDED,"approve","forward-owner").await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
}

#[tokio::test]
async fn equal_clock_uses_strict_row_order() {
    let f=Fixture::new(); let id=f.ask().await;
    // Simulate the ordinary millisecond tie without replacing the recorder.
    f.write(|c| { c.execute_batch("CREATE TRIGGER tie_question_clock BEFORE INSERT ON cmd_history WHEN NEW.origin='pane' BEGIN UPDATE cmd_history SET ts=NEW.ts WHERE delivery='approval_request'; END;")?; Ok(()) });
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    let reply=f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
    let c=f.state.store.read().unwrap();
    let (shown,ask_ts,reply_ts):(i64,i64,i64)=c.query_row("SELECT g.shown_row,a.ts,r.ts FROM _amux_delegation_grants g JOIN cmd_history a ON a.id=g.shown_row JOIN cmd_history r ON r.id=g.resolution_row WHERE g.code=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert!(shown<reply); assert_eq!(ask_ts,reply_ts);
}

#[tokio::test]
async fn replaced_asker_cannot_spend_owner_reply() {
    let f=Fixture::new(); let id=f.ask().await;
    let p=f.home.path().join(format!("sessions/{LEAD}.env"));
    std::fs::rename(&p,f.home.path().join("sessions/retired-asker.env")).unwrap();
    std::fs::write(&p,format!("CC_TAGS=leads,dev\nCC_ORCHESTRATOR=1\nCC_INCARNATION=replacement-asker\n")).unwrap();
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    let (st,v)=f.apply(&id).await;
    assert_eq!(st,StatusCode::FORBIDDEN); assert_eq!(v["code"],"request_actor_identity_changed");
}

#[tokio::test]
async fn peer_echo_after_owner_no_never_reopens_the_window_through_routes() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.hook(OWNER_AGENT,f.report("no")).await);
    f.resolution(OWNER_AGENT,"request_reply_not_affirmative",None);
    accepted(f.send(OWNER_AGENT,PEER,"no","peer-echo-no").await);
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    f.resolution(OWNER_AGENT,"request_no_candidate",None);
    assert_eq!(f.request_resolution(&id),(None,Some("request_reply_not_affirmative".into())));
}

#[tokio::test]
async fn future_or_stale_recorded_reply_never_funds() {
    for delta_ms in [60_000i64,-86_401_000] {
        let f=Fixture::new(); let id=f.ask().await;
        f.write(move|c| { c.execute_batch(&format!("CREATE TRIGGER skew_reply_clock AFTER INSERT ON cmd_history WHEN NEW.origin='pane' BEGIN UPDATE cmd_history SET ts=NEW.ts+{delta_ms}-1000 WHERE delivery='approval_request'; UPDATE cmd_history SET ts=ts+{delta_ms} WHERE id=NEW.id; END;"))?; Ok(()) });
        accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
        f.resolution(OWNER_AGENT,"request_reply_outside_window",None);
        assert_eq!(f.request_resolution(&id),(None,Some("request_reply_outside_window".into())));
    }
}

#[tokio::test]
async fn legacy_asker_selected_session_is_refused_at_application() {
    let f=Fixture::new(); let id=f.ask().await; let code=id.clone();
    f.write(move|c| { c.execute("UPDATE _amux_delegation_grants SET reply_session=?2,reply_incarnation=?3 WHERE code=?1",rusqlite::params![code,TARGET,format!("inc-{TARGET}")])?; Ok(()) });
    accepted(f.hook(TARGET,f.report("yes")).await);
    f.resolution(TARGET,"request_reply_not_owner_session",None);
    let (st,v)=f.apply(&id).await;
    assert_eq!(st,StatusCode::FORBIDDEN); assert_eq!(v["code"],"request_reply_not_owner_session");
}

#[tokio::test]
async fn missing_owner_incarnation_refuses_request_creation() {
    let f=Fixture::new();
    std::fs::remove_file(f.home.path().join(format!("sessions/{OWNER_AGENT}.env"))).unwrap();
    let (st,v)=f.call(None,Some(LEAD),"/api/delegation/requests",json!({
        "method":"PATCH","path":format!("/api/sessions/{TARGET}/config"),"body":{"tags":""}})).await;
    assert_eq!(st,StatusCode::FORBIDDEN); assert_eq!(v["code"],"request_identity_missing");
}

#[tokio::test]
async fn no_history_send_cannot_supply_a_typed_reply() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.call(Some(OUTSIDE),None,&format!("/api/sessions/{OWNER_AGENT}/send"),json!({
        "text":"yes","record_history":false,"msg_id":"unrecorded-owner"})).await);
    assert_eq!(f.request_resolution(&id),(None,None));
    assert_eq!(f.state.store.read().unwrap().query_row("SELECT COUNT(*) FROM _amux_delegation_resolutions",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[tokio::test]
async fn intervening_owner_question_closes_the_old_request_window() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.hook(OWNER_AGENT,f.report("what is the status?")).await);
    f.resolution(OWNER_AGENT,"request_reply_not_affirmative",None);
    accepted(f.hook(OWNER_AGENT,f.report("approve")).await);
    f.resolution(OWNER_AGENT,"request_no_candidate",None);
    assert_eq!(f.request_resolution(&id),(None,Some("request_reply_not_affirmative".into())));
}

#[tokio::test]
async fn replaced_target_cannot_receive_the_approved_change() {
    let f=Fixture::new(); let id=f.ask().await;
    accepted(f.hook(OWNER_AGENT,f.report("yes")).await);
    f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
    let p=f.home.path().join(format!("sessions/{TARGET}.env"));
    std::fs::write(&p,std::fs::read_to_string(&p).unwrap().replace(&format!("inc-{TARGET}"),"replacement-target")).unwrap();
    let (st,v)=f.apply(&id).await;
    assert_eq!(st,StatusCode::FORBIDDEN); assert_eq!(v["code"],"request_target_identity_changed");
}

#[tokio::test]
async fn owner_cli_default_does_not_promote_worker_or_relay_input() {
    for relay in [false,true] {
        let f=Fixture::new(); let id=f.ask().await;
        let mut body=json!({"text":"yes","msg_id":"peer-cli","no_board":true});
        if relay { body["source_session"]=json!(LEAD); }
        accepted(f.call(Some(if relay { OUTSIDE } else { PEER }),None,
            &format!("/api/workers/{OWNER_AGENT}/send"),body).await);
        assert_eq!(f.request_resolution(&id),(None,None));
        let c=f.state.store.read().unwrap();
        let ty:String=c.query_row("SELECT COALESCE(delegation_message_type,type) FROM cmd_history WHERE session=?1 ORDER BY id DESC LIMIT 1",[OWNER_AGENT],|r|r.get(0)).unwrap();
        assert_eq!(ty,"session","CLI default must preserve peer provenance");
        drop(c);
        accepted(f.hook(OWNER_AGENT,f.report("approve")).await);
        f.resolution(OWNER_AGENT,"request_resolved",Some(&id));
    }
}
