//! Approval request identity/correlation regressions — Max 2026-10-23, refs maxtongwang/ai-config#160.
//! Real router, explicit requests and native Stop/UserPromptSubmit reports. No text is parsed
//! into a request. Historical free-text matcher tests were retired with it.
mod delegation_support;
use delegation_support::*;
use serde_json::{json, Value};

fn cfg(w: &str) -> String { format!("/api/sessions/{w}/config") }

async fn view(e: &Env, id: &str) -> Value {
    let (st,v)=e.call(Who::Worker(MRPOPO),"GET",&format!("/api/delegation/requests/{id}"),None).await;
    assert!(st.is_success(),"request read: {v}");
    v["request"].clone()
}

async fn apply(e: &Env, actor: &str, id: &str, worker: &str, body: Value) -> (axum::http::StatusCode,Value) {
    e.call(Who::Worker(actor),"PATCH",&cfg(worker),Some(with_request(body,json!(id)))).await
}

// Only the live native owner session funds approval. HTTP sends are close-only.
async fn owner_reply(e: &Env, text: &str) -> (axum::http::StatusCode,Value) {
    e.type_in_pane(MAX_PANE, text).await
}

// Read the recorder's result directly: GET request/list reconciles missed
// replies and would hide a disconnected post-recording resolver.
fn recorded_resolution(e: &Env, code: &str) -> i64 {
    let conn=e.store.read().unwrap();
    let row: i64=conn.query_row(
        "SELECT id FROM cmd_history WHERE session=?1 AND COALESCE(delegation_message_type,type)='user' ORDER BY id DESC LIMIT 1",
        [MAX_PANE],|r|r.get(0)).expect("verified owner route must record a message");
    let (count,actual):(i64,Option<String>)=conn.query_row(
        "SELECT COUNT(*),MIN(code) FROM _amux_delegation_resolutions WHERE reply_row=?1",
        [row],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((count,actual),(1,Some(code.into())),"recorder must resolve once before a recovery read");
    row
}

#[tokio::test]
async fn explicit_request_is_shown_with_stable_identities_and_structured_change() {
    let e=setup().await;
    let before=e.config_snapshot();
    let id=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    assert_eq!(before,e.config_snapshot(),"creation must not execute the mutation");
    let g=view(&e,&id).await;
    assert_eq!(g["request_id"],id);
    assert_eq!(g["actor_incarnation"],format!("inc-{DEV_LEAD}"));
    assert_eq!(g["target_incarnation"],format!("inc-{DEV_B}"));
    assert_eq!(g["reply_session"],MAX_PANE);
    assert_eq!(g["reply_incarnation"],format!("inc-{MAX_PANE}"));
    assert_eq!(g["change"]["action"],json!({"SetTags":{"from":["dev"],"to":[]}}));
    assert_eq!(g["change"]["body"],json!({"tags":""}));
    let conn=e.store.read().unwrap();
    let (session,text):(String,String)=conn.query_row("SELECT session,text FROM cmd_history WHERE id=?1",[g["shown_row"].as_i64().unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(session,MAX_PANE);
    assert_eq!(text, e.approval_block(&id));
    assert!(text.starts_with(&format!("Approval needed — {}\n", e.short_code(&id))));
    assert!(!text.contains(&id), "the display uses a short code, never the full request ID");
}

#[tokio::test]
async fn approval_yes_is_bound_to_the_shown_session_and_requesting_actor() {
    let e=setup().await;
    let id=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    e.type_in_pane(DEV_A,"yes").await;
    let (st,v)=apply(&e,DEV_LEAD,&id,DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"request_unresolved","another session cannot fund an owner-session question");
    owner_reply(&e,"approve").await;
    recorded_resolution(&e,"request_resolved");
    let (st,v)=apply(&e,MRPOPO,&id,DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"request_actor_identity_changed","another actor cannot cite the request");
    let (st,v)=apply(&e,DEV_LEAD,&id,DEV_B,json!({"tags":""})).await;
    assert_allowed(st,&v,"Max's owner-session reply funds its asking actor only");
}

#[tokio::test]
async fn native_decline_closes_old_request_before_coded_new_approval() {
    let e=setup().await;
    let old=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    owner_reply(&e,"wait").await;
    recorded_resolution(&e,"request_reply_not_affirmative");
    let new=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true})).await;
    owner_reply(&e,&format!("yes {}",e.short_code(&new))).await;
    recorded_resolution(&e,"request_resolved");
    let (st,v)=apply(&e,DEV_LEAD,&old,DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"request_reply_not_affirmative","a native decline permanently closes the old question");
    let (st,v)=apply(&e,MRPOPO,&new,DEV_A,json!({"toggle_yolo":true})).await;
    assert_allowed(st,&v,"the code approves only the new question");
    assert_eq!(e.grant_status(&old),"pending");
}

#[tokio::test]
async fn peer_echo_no_never_reopens_refused_request_p1() {
    let e=setup().await;
    let before=e.config_snapshot();
    let id=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    owner_reply(&e,"no").await;
    let no=recorded_resolution(&e,"request_reply_not_affirmative");
    // P1: the exact row shape send_post records for a peer's direct send.
    e.seed_row(MAX_PANE,"no","session",DEV_LEAD,Some("direct"),now_s());
    owner_reply(&e,"yes").await;
    // Try the actual mutation before checking the no-candidate audit record:
    // on 960da3c7 this returns 200 and removes the worker's dev tag.
    let (st,v)=apply(&e,DEV_LEAD,&id,DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"request_reply_not_affirmative","P1: unrelated yes cannot fund a refused request");
    recorded_resolution(&e,"request_no_candidate");
    assert_eq!(before,e.config_snapshot());
    let conn=e.store.read().unwrap();
    let refusal:(Option<i64>,String)=conn.query_row(
        "SELECT resolution_row,resolution_code FROM _amux_delegation_grants WHERE code=?1",
        [&id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(refusal,(None,"request_reply_not_affirmative".into()));
    assert_eq!(conn.query_row("SELECT code FROM _amux_delegation_resolutions WHERE reply_row=?1",
        [no],|r|r.get::<_,String>(0)).unwrap(),"request_reply_not_affirmative");
}

#[tokio::test]
async fn peer_rows_preserve_requests_but_uncertain_input_closes_them() {
    // Peer session rows preserve provenance. Uncertain user/chat shapes are
    // close-only boundaries; neither is a substitute for the native hook.
    for (ty,projected) in [("session",false),("chat",false),("user",false),("session",true)] {
        let e=setup().await;
        let id=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
        let peer=e.seed_row(MAX_PANE,"no",ty,DEV_LEAD,Some("direct"),now_s());
        if projected {
            e.store.write(move|c| {
                c.execute("UPDATE cmd_history SET type='steering',delegation_message_type='session',process_origin='remote' WHERE id=?1",[peer])?;
                Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
            }).unwrap();
        }
        // Reconcile the legacy boundary before later live input. Native
        // production reports already adjudicate their input atomically.
        view(&e,&id).await;
        owner_reply(&e,&format!("yes {}",e.short_code(&id))).await;
        recorded_resolution(&e,if ty == "session" { "request_resolved" } else { "request_no_open_code" });
        let (st,v)=apply(&e,DEV_LEAD,&id,DEV_B,json!({"tags":""})).await;
        if ty == "session" {
            assert_allowed(st,&v,"peer input cannot close the pending native question");
        } else {
            assert_code(st,&v,"request_reply_not_owner_input","uncertain input closes without funding");
        }
        let conn=e.store.read().unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM _amux_delegation_resolutions WHERE reply_row=?1",
            [peer],|r|r.get::<_,i64>(0)).unwrap(),i64::from(ty != "session"));
    }
}

#[tokio::test]
async fn declined_request_never_reopens_for_later_plain_or_coded_yes() {
    // v3.1 confirmation is open; a decline is durably closed. The old
    // resolution_code-only exclusion is superseded by approval_closed_at.
    let e=setup().await;
    let before=e.config_snapshot();
    let id=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    owner_reply(&e,"no").await;
    recorded_resolution(&e,"request_reply_not_affirmative");
    for text in ["yes".to_string(),format!("yes {}",e.short_code(&id))] {
        owner_reply(&e,&text).await;
        let (st,v)=apply(&e,DEV_LEAD,&id,DEV_B,json!({"tags":""})).await;
        assert_code(st,&v,"request_reply_not_affirmative","closed request cannot become a candidate again");
        assert_eq!(before,e.config_snapshot());
    }
}

#[tokio::test]
async fn negated_free_text_never_creates_or_funds_a_request() {
    let e=setup().await;
    // Keep this specimen runnable against faba94781: seed only shipped
    // columns, with the original N2 binding, so the pre-fix failure is the
    // unauthorized config write rather than a missing 0132 fixture column.
    let pair=std::sync::Arc::new(std::sync::Mutex::new((0i64,0i64)));
    let out=pair.clone();
    let ask=format!("Max, rather than enable yolo for {DEV_A}, may I enable yolo for {DEV_B}?");
    let bindings=serde_json::to_string(&std::collections::BTreeMap::from([(DEV_A,format!("inc-{DEV_A}")),(DEV_B,format!("inc-{DEV_B}"))])).unwrap();
    let at=(now_s()*1000.0) as i64;
    e.store.write(move|c| {
        c.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts,cite_worker_ids) VALUES(?1,?2,'session',?3,'direct',?4,?5)",
            rusqlite::params![MAX_PANE,ask,MRPOPO,at,bindings])?;
        let ask=c.last_insert_rowid();
        c.execute("INSERT INTO cmd_history(session,text,type,origin,delivery,ts) VALUES(?1,'yes','user','pane','pane',?2)",rusqlite::params![MAX_PANE,at+1])?;
        *out.lock().unwrap()=(ask,c.last_insert_rowid());
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    // Deliberate legacy wire specimen: a removed citation pair must never authorize.
    let (a,r)=*pair.lock().unwrap();
    let before=e.config_snapshot();
    let (st,v)=e.call(Who::Worker(MRPOPO),"PATCH",&cfg(DEV_A),Some(json!({"toggle_yolo":true,"cite":{"ask_row_id":a,"reply_row_id":r}}))).await;
    let attempted=(st,v);
    assert_eq!(before,e.config_snapshot(),"contrast prose must not authorize the excluded worker change");
    let (st,v)=e.call(Who::Worker(MRPOPO),"GET","/api/delegation/requests",None).await;
    assert_allowed(st,&v,"request list");
    assert_eq!(v["requests"],json!([]));
    assert_code(attempted.0,&attempted.1,"request_id_required","plain text is never a request");
    assert_eq!(before,e.config_snapshot());
}

#[tokio::test]
async fn two_displayed_requests_require_confirmation_and_code_selects_one() {
    // v3.1: multiple open requests require confirmation; plain yes cannot
    // silently choose the most recent display. A short code selects one.
    let e=setup().await;
    let a=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    let b=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true})).await;
    let (_,reply)=owner_reply(&e,"yes").await;
    assert!(reply["approval_context"].as_str().unwrap().contains("reason: several-open"),"{reply}");
    recorded_resolution(&e,"request_confirm_needed");
    for (actor,id,worker,body) in [(DEV_LEAD,&a,DEV_B,json!({"tags":""})),(MRPOPO,&b,DEV_A,json!({"toggle_yolo":true}))] {
        let (st,v)=apply(&e,actor,id,worker,body).await;
        assert_code(st,&v,"request_unresolved","plain yes must fund neither open request");
        assert_eq!(view(&e,id).await["resolution_code"],"request_confirm_needed");
    }
    owner_reply(&e,&format!("yes {}",e.short_code(&b))).await;
    recorded_resolution(&e,"request_resolved");
    let (st,v)=apply(&e,MRPOPO,&b,DEV_A,json!({"toggle_yolo":true})).await;
    assert_allowed(st,&v,"the explicit code selects only its request");
    assert_eq!(view(&e,&a).await["open"],true);
    assert_eq!(e.grant_status(&a),"pending");
}

#[tokio::test]
async fn yes_with_no_shown_candidate_funds_nothing_and_is_observable() {
    let e=setup().await;
    // Actual attributed owner route invokes the resolver even with no requests.
    let (st,v)=owner_reply(&e,"yes").await;
    assert_allowed(st,&v,"record human yes");
    recorded_resolution(&e,"request_no_candidate");
    let (st,v)=e.call(Who::Worker(MRPOPO),"GET","/api/delegation/requests",None).await;
    assert_allowed(st,&v,"list no-candidate outcome");
    assert!(v["resolutions"].as_array().unwrap().iter().any(|r|r["code"]=="request_no_candidate"),"{v}");
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true})).await;
    let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"toggle_yolo":true})).await;
    assert_code(st,&v,"request_unresolved","an earlier yes cannot approve a question shown later");
}

#[tokio::test]
async fn changed_action_target_value_direction_or_route_is_refused() {
    let e=setup().await;
    let id=e.approved_request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    let id=id.as_str().unwrap();
    for (w,body) in [(DEV_A,json!({"worktree_verify":"two"})),(DEV_A,json!({"isolated":true})),(DEV_B,json!({"worktree_verify":"one"}))] {
        let before=e.config_snapshot();
        let (st,v)=apply(&e,MRPOPO,id,w,body).await;
        assert_code(st,&v,"request_change_mismatch","stored operands must match exactly");
        assert_eq!(before,e.config_snapshot());
        assert_eq!(e.grant_status(id),"pending");
    }
    let (st,v)=e.call(Who::Worker(MRPOPO),"PUT","/api/scope",Some(with_request(scope_body("worker",DEV_A,"env",json!({"CC_WORKTREE_VERIFY":"one"})),json!(id)))).await;
    assert_code(st,&v,"request_change_mismatch","equivalent routes cannot substitute for the stored route");
    let (st,v)=e.call(Who::Worker(MRPOPO),"PATCH",&format!("/api/workers/{DEV_A}/config"),Some(with_request(json!({"worktree_verify":"one"}),json!(id)))).await;
    assert_code(st,&v,"request_change_mismatch","the worker alias route cannot replace the captured session route");
    let (st,v)=apply(&e,MRPOPO,id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_allowed(st,&v,"matching control still applies after mismatches");
}

#[tokio::test]
async fn replay_and_undo_never_release_the_reply_or_request() {
    let e=setup().await;
    let id=e.approved_request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    let id=id.as_str().unwrap();
    let (st,v)=apply(&e,MRPOPO,id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_allowed(st,&v,"first application");
    let row=view(&e,id).await["cited_row"].as_i64().unwrap();
    let (st,v)=e.call(Who::Owner,"POST",&format!("/api/delegation/grants/{id}/undo"),None).await;
    assert_allowed(st,&v,"undo");
    let (st,v)=apply(&e,MRPOPO,id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_code(st,&v,"cite_pair_spent","undo does not release approval");
    let next=e.request(MRPOPO,"PATCH",&cfg(DEV_B),json!({"worktree_verify":"two"})).await;
    let (st,v)=apply(&e,MRPOPO,&next,DEV_B,json!({"worktree_verify":"two"})).await;
    assert_code(st,&v,"request_unresolved","same reply cannot fund a subsequent request");
    let conn=e.store.read().unwrap();
    let n:i64=conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants WHERE evidence_row=?1",[row],|r|r.get(0)).unwrap();
    assert_eq!(n,1);
}

#[tokio::test]
async fn request_actor_target_and_reply_incarnations_never_follow_reused_names() {
    for worker in [DEV_LEAD,DEV_B,MAX_PANE] {
        let e=setup().await;
        let id=e.approved_request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
        let id=id.as_str().unwrap();
        let path=e.env_path(worker);
        let original=std::fs::read_to_string(&path).unwrap();
        std::fs::rename(&path,e.env_path(&format!("renamed-{worker}"))).unwrap();
        std::fs::write(&path,original.replace(&format!("inc-{worker}"),"replacement-incarnation")).unwrap();
        let (st,v)=apply(&e,DEV_LEAD,id,DEV_B,json!({"tags":""})).await;
        let expected=if worker==DEV_LEAD {"request_actor_identity_changed"} else if worker==DEV_B {"request_target_identity_changed"} else {"request_reply_identity_changed"};
        assert_code(st,&v,expected,"a replacement name cannot inherit a request");
        if worker==DEV_B {
            let (st,v)=apply(&e,DEV_LEAD,id,&format!("renamed-{worker}"),json!({"tags":""})).await;
            assert_code(st,&v,"request_target_identity_changed","renamed target cannot redirect an existing request path");
        }
        assert_eq!(e.grant_status(id),"pending");
    }
}

#[tokio::test]
async fn lead_requests_retain_team_only_own_groups_and_leads_exclusion() {
    let e=setup().await;
    for (w,tags,code) in [(OPS_A,"dev,ops","peer_config_owner_only"),(DEV_A,"dev,ops","peer_config_owner_only"),(DEV_A,"dev,leads","worker_group_membership_refused")] {
        let (st,v)=e.call(Who::Worker(DEV_LEAD),"POST","/api/delegation/requests",Some(json!({"method":"PATCH","path":cfg(w),"body":{"tags":tags}}))).await;
        assert_code(st,&v,code,"stored change must pass the lead membership table");
    }
    let id=e.approved_request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    let (st,v)=apply(&e,DEV_LEAD,id.as_str().unwrap(),DEV_B,json!({"tags":""})).await;
    assert_allowed(st,&v,"own-team, own-group removal");
}

#[tokio::test]
async fn nonaffirmative_replies_and_unshown_requests_never_fund() {
    // dev-lead ruling (2026-10-24, v3.1 rule 4 refinement): only a prompt with
    // no approve word at the start declines; an approve word followed by other
    // text is doubt (confirm step), which neither funds nor closes.
    for text in ["no","wait"] {
        let e=setup().await;
        let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
        owner_reply(&e,text).await;
        let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
        assert_code(st,&v,"request_reply_not_affirmative",text);
    }
    for text in ["yes?","yes, don't proceed","yes but not yet"] {
        let e=setup().await;
        let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
        let (reply_st,reply)=owner_reply(&e,text).await;
        assert!(reply_st.is_success(),"{text}: {reply}");
        assert_eq!(reply["approval_context"],format!("[amux approval] confirm needed {} (reason: extra-text)",e.short_code(&id)),"{text}");
        let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
        assert_code(st,&v,"request_unresolved",text);
        let request=view(&e,&id).await;
        assert_eq!(request["resolution_note"],"extra-text","{text}");
        assert_eq!(request["confirm_required"],true,"{text}");
        assert_eq!(request["open"],true,"{text}");
        assert!(request["resolution_row"].is_null(),"{text}");
    }
    let e=setup().await;
    let (st,v)=e.request_result(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    assert_eq!(st, axum::http::StatusCode::ACCEPTED, "{v}");
    let id=v["request_id"].as_str().unwrap().to_string();
    owner_reply(&e,"yes").await;
    let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_code(st,&v,"request_not_shown","no shown message means no authority");
}

#[tokio::test]
async fn request_creation_never_dispatches_send_or_an_ordinary_allowed_mutation() {
    let e=setup().await;
    let before=e.config_snapshot();
    for (method,path,body,code) in [("POST",format!("/api/sessions/{DEV_A}/send"),json!({"text":"hello"}),"request_route_invalid"),
        ("PATCH",cfg(DEV_A),json!({"model":"sonnet"}),"request_not_required")] {
        let (st,v)=e.call(Who::Worker(DEV_LEAD),"POST","/api/delegation/requests",Some(json!({"method":method,"path":path,"body":body}))).await;
        assert!(st.is_client_error(),"{v}");
        assert_eq!(v["code"],code);
    }
    assert_eq!(before,e.config_snapshot());
}


#[tokio::test]
async fn shown_message_must_still_exist_in_the_bound_reply_session() {
    let e=setup().await;
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    e.seed_row(MAX_PANE,"non-request receipt","session",MRPOPO,Some("direct"),now_s());
    let i=id.clone();
    e.store.write(move|c| {
        c.execute("DELETE FROM cmd_history WHERE id=(SELECT shown_row FROM _amux_delegation_grants WHERE code=?1)",[i])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    owner_reply(&e,"yes").await;
    let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_code(st,&v,"request_not_shown","a dangling shown-row reference is not a visible question");
    assert_eq!(e.grant_status(&id),"pending");
}


#[tokio::test]
async fn lead_team_authority_is_rechecked_when_applying_the_stored_request() {
    let e=setup().await;
    let id=e.approved_request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    let path=e.env_path(DEV_LEAD);
    let original=std::fs::read_to_string(&path).unwrap();
    std::fs::write(path,original.replace("leads,dev","leads,ops")).unwrap();
    let (st,v)=apply(&e,DEV_LEAD,id.as_str().unwrap(),DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"peer_config_owner_only","approval cannot preserve lost team authority");
    assert_eq!(e.grant_status(id.as_str().unwrap()),"pending");
}


#[tokio::test]
async fn undo_keeps_the_original_request_identity_and_never_targets_a_replacement() {
    let e=setup().await;
    let id=e.approved_request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    let id=id.as_str().unwrap();
    let (st,v)=apply(&e,MRPOPO,id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_allowed(st,&v,"apply original request");
    let path=e.env_path(DEV_A);
    let original=std::fs::read_to_string(&path).unwrap();
    let replacement=original.replace(&format!("inc-{DEV_A}"),"replacement-incarnation");
    std::fs::write(&path,&replacement).unwrap();
    let (st,v)=e.call(Who::Owner,"POST",&format!("/api/delegation/grants/{id}/undo"),None).await;
    assert_code(st,&v,"grant_target_identity_changed","undo is bound to the original request");
    assert_eq!(std::fs::read_to_string(path).unwrap(),replacement);
    assert_eq!(e.grant_status(id),"applied");
}


#[tokio::test]
async fn stored_toggle_direction_cannot_change_when_current_state_changes() {
    let e=setup().await;
    let id=e.approved_request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true})).await;
    let (st,v)=e.call(Who::Owner,"PATCH",&cfg(DEV_A),Some(json!({"toggle_yolo":true}))).await;
    assert_allowed(st,&v,"owner enables the setting first");
    let before=e.config_snapshot();
    let (st,v)=apply(&e,MRPOPO,id.as_str().unwrap(),DEV_A,json!({"toggle_yolo":true})).await;
    assert_code(st,&v,"request_change_mismatch","an approved enable must never become a disable");
    assert_eq!(before,e.config_snapshot());
    assert_eq!(e.grant_status(id.as_str().unwrap()),"pending");
}


#[tokio::test]
async fn owner_undo_is_not_stranded_when_the_asking_worker_retires() {
    let e=setup().await;
    let id=e.approved_request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    let id=id.as_str().unwrap();
    let (st,v)=apply(&e,MRPOPO,id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_allowed(st,&v,"apply original request");
    std::fs::remove_file(e.env_path(MRPOPO)).unwrap();
    let (st,v)=e.call(Who::Owner,"POST",&format!("/api/delegation/grants/{id}/undo"),None).await;
    assert_allowed(st,&v,"owner can undo the stored target after actor retirement");
    assert_eq!(e.grant_status(id),"undone");
}


#[tokio::test]
async fn project_steering_projection_preserves_owner_but_never_peer_authorship() {
    let e=setup().await;
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
    let (st,v)=owner_reply(&e,"approve").await;
    assert_allowed(st,&v,"record actual owner reply through the attributed route");
    let row=recorded_resolution(&e,"request_resolved");
    assert_eq!(view(&e,&id).await["resolution_row"],row);
    {
        let conn=e.store.read().unwrap();
        assert_eq!(conn.query_row("SELECT delivery FROM cmd_history WHERE id=?1",[row],|r|r.get::<_,String>(0)).unwrap(),"approval_native_prompt");
    }
    // Project execution's existing type projection must not rewrite the
    // recorder's original authorship fact or erase an eligible prior reply.
    e.store.write(move|c| {
        c.execute("UPDATE cmd_history SET type='steering',delegation_message_type='user' WHERE id=?1",[row])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_allowed(st,&v,"owner authorship survives the project projection");

    let next=e.request(MRPOPO,"PATCH",&cfg(DEV_B),json!({"worktree_verify":"two"})).await;
    let peer=e.seed_row(MAX_PANE,"yes","session",MRPOPO,Some("direct"),now_s());
    e.store.write(move|c| {
        c.execute("UPDATE cmd_history SET type='steering',delegation_message_type='session',process_origin='remote' WHERE id=?1",[peer])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    let (st,v)=apply(&e,MRPOPO,&next,DEV_B,json!({"worktree_verify":"two"})).await;
    assert_code(st,&v,"request_unresolved","peer steering is still not owner input");
    let (st,v)=owner_reply(&e,&format!("ok {}",e.short_code(&next))).await;
    assert_allowed(st,&v,"record the actual owner control");
    recorded_resolution(&e,"request_resolved");
    let (st,v)=apply(&e,MRPOPO,&next,DEV_B,json!({"worktree_verify":"two"})).await;
    assert_allowed(st,&v,"only the actual owner reply funds the second request");
}
