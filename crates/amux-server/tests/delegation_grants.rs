//! Approval request identity/correlation regressions — Max 2026-10-23, refs maxtongwang/ai-config#160.
//! Real router, explicit requests and durable Messages rows. No text is parsed
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

// Read the recorder's result directly: GET request/list reconciles missed
// replies and would hide a disconnected post-recording resolver.
fn recorded_resolution(e: &Env, code: &str) -> i64 {
    let conn=e.store.read().unwrap();
    let row: i64=conn.query_row(
        "SELECT id FROM cmd_history WHERE session=?1 AND delivery='pane' ORDER BY id DESC LIMIT 1",
        [MAX_PANE],|r|r.get(0)).expect("applied owner hook must record a pane message");
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
    let id=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""}),MAX_PANE).await;
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
    assert!(text.contains(&id) && text.contains("SetTags") && text.contains(DEV_B));
}

#[tokio::test]
async fn approval_yes_is_bound_to_the_shown_session_and_requesting_actor() {
    let e=setup().await;
    let foreign=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""}),DEV_LEAD).await;
    let local=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true}),MAX_PANE).await;
    e.seed_pane_prompt(MAX_PANE,"yes",now_s());
    let (st,v)=apply(&e,DEV_LEAD,&foreign,DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"request_unresolved","yes elsewhere cannot fund the foreign request");
    let (st,v)=apply(&e,DEV_LEAD,&local,DEV_A,json!({"toggle_yolo":true})).await;
    assert_code(st,&v,"request_actor_identity_changed","another actor cannot cite the request");
    let (st,v)=apply(&e,MRPOPO,&local,DEV_A,json!({"toggle_yolo":true})).await;
    assert_allowed(st,&v,"yes in S funds only the request shown in S");
    assert_eq!(e.grant_status(&foreign),"pending");
    assert_eq!(e.tags(DEV_B),std::collections::BTreeSet::from(["dev".to_string()]));
}

#[tokio::test]
async fn newer_unanswered_request_gets_yes_and_older_window_never_does() {
    let e=setup().await;
    let old=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""}),MAX_PANE).await;
    let wait=e.seed_pane_prompt(MAX_PANE,"wait",now_s());
    e.store.write(move|c| {
        c.execute("UPDATE cmd_history SET type='steering',delegation_message_type='user' WHERE id=?1",[wait])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    let new=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true}),MAX_PANE).await;
    e.seed_pane_prompt(MAX_PANE,"yes",now_s());
    let (st,v)=apply(&e,DEV_LEAD,&old,DEV_B,json!({"tags":""})).await;
    assert_code(st,&v,"request_reply_not_affirmative","old unanswered question cannot catch a later yes");
    let (st,v)=apply(&e,MRPOPO,&new,DEV_A,json!({"toggle_yolo":true})).await;
    assert_allowed(st,&v,"newer question gets this window's yes");
    assert_eq!(e.grant_status(&old),"pending");
}

#[tokio::test]
async fn negated_free_text_never_creates_or_funds_a_request() {
    let e=setup().await;
    // Keep this specimen runnable against faba94781: seed only shipped
    // columns, with the original N2 binding, so the pre-fix failure is the
    // unauthorized config write rather than a missing 0129 fixture column.
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
async fn two_requests_since_last_reply_are_ambiguous_and_stay_pending_with_notes() {
    let e=setup().await;
    let a=e.request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""}),MAX_PANE).await;
    let b=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true}),MAX_PANE).await;
    e.seed_pane_prompt(MAX_PANE,"yes",now_s());
    let before=e.config_snapshot();
    for (actor,id,w,body) in [(DEV_LEAD,&a,DEV_B,json!({"tags":""})),(MRPOPO,&b,DEV_A,json!({"toggle_yolo":true}))] {
        let (st,v)=apply(&e,actor,id,w,body).await;
        assert_code(st,&v,"request_ambiguous","two equally eligible questions fund nothing");
        let g=view(&e,id).await;
        assert_eq!(g["status"],"pending");
        assert_eq!(g["resolution_code"],"request_ambiguous");
        assert!(g["resolution_note"].as_str().unwrap().contains("multiple"));
        assert!(g["cited_row"].is_null());
    }
    assert_eq!(before,e.config_snapshot());
}

#[tokio::test]
async fn yes_with_no_shown_candidate_funds_nothing_and_is_observable() {
    let e=setup().await;
    // Actual hook path invokes the resolver even with no existing requests.
    let (st,v)=e.type_in_pane(MAX_PANE,"yes").await;
    assert_allowed(st,&v,"record human yes");
    recorded_resolution(&e,"request_no_candidate");
    let (st,v)=e.call(Who::Worker(MRPOPO),"GET","/api/delegation/requests",None).await;
    assert_allowed(st,&v,"list no-candidate outcome");
    assert!(v["resolutions"].as_array().unwrap().iter().any(|r|r["code"]=="request_no_candidate"),"{v}");
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"toggle_yolo":true}),MAX_PANE).await;
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
    let next=e.request(MRPOPO,"PATCH",&cfg(DEV_B),json!({"worktree_verify":"two"}),MAX_PANE).await;
    let (st,v)=apply(&e,MRPOPO,&next,DEV_B,json!({"worktree_verify":"two"})).await;
    assert_code(st,&v,"request_unresolved","same reply cannot fund a subsequent request");
    let conn=e.store.read().unwrap();
    let n:i64=conn.query_row("SELECT COUNT(*) FROM _amux_delegation_grants WHERE evidence_row=?1",[row],|r|r.get(0)).unwrap();
    assert_eq!(n,1);
}

#[tokio::test]
async fn request_actor_target_and_reply_incarnations_never_follow_reused_names() {
    for worker in [MRPOPO,DEV_A,MAX_PANE] {
        let e=setup().await;
        let id=e.approved_request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"})).await;
        let id=id.as_str().unwrap();
        let path=e.env_path(worker);
        let original=std::fs::read_to_string(&path).unwrap();
        std::fs::rename(&path,e.env_path(&format!("renamed-{worker}"))).unwrap();
        std::fs::write(&path,original.replace(&format!("inc-{worker}"),"replacement-incarnation")).unwrap();
        let (st,v)=apply(&e,MRPOPO,id,DEV_A,json!({"worktree_verify":"one"})).await;
        let expected=if worker==MRPOPO {"request_actor_identity_changed"} else if worker==DEV_A {"request_target_identity_changed"} else {"request_reply_identity_changed"};
        assert_code(st,&v,expected,"a replacement name cannot inherit a request");
        if worker==DEV_A {
            let (st,v)=apply(&e,MRPOPO,id,&format!("renamed-{worker}"),json!({"worktree_verify":"one"})).await;
            assert_code(st,&v,"request_target_identity_changed","renamed target cannot redirect an existing request path");
        }
        assert_eq!(e.grant_status(id),"pending");
    }
}

#[tokio::test]
async fn lead_requests_retain_team_only_own_groups_and_leads_exclusion() {
    let e=setup().await;
    for (w,tags,code) in [(OPS_A,"dev,ops","peer_config_owner_only"),(DEV_A,"dev,ops","peer_config_owner_only"),(DEV_A,"dev,leads","worker_group_membership_refused")] {
        let (st,v)=e.call(Who::Worker(DEV_LEAD),"POST","/api/delegation/requests",Some(json!({"reply_session":MAX_PANE,"method":"PATCH","path":cfg(w),"body":{"tags":tags}}))).await;
        assert_code(st,&v,code,"stored change must pass the lead membership table");
    }
    let id=e.approved_request(DEV_LEAD,"PATCH",&cfg(DEV_B),json!({"tags":""})).await;
    let (st,v)=apply(&e,DEV_LEAD,id.as_str().unwrap(),DEV_B,json!({"tags":""})).await;
    assert_allowed(st,&v,"own-team, own-group removal");
}

#[tokio::test]
async fn nonaffirmative_replies_and_unshown_requests_never_fund() {
    for text in ["no","wait","yes?","yes, don't proceed","yes but not yet"] {
        let e=setup().await;
        let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"}),MAX_PANE).await;
        e.seed_pane_prompt(MAX_PANE,text,now_s());
        let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
        assert_code(st,&v,"request_reply_not_affirmative",text);
    }
    let e=setup().await;
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"}),MAX_PANE).await;
    let i=id.clone();
    e.store.write(move|c| {c.execute("UPDATE _amux_delegation_grants SET shown_row=NULL WHERE code=?1",[i])?;Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})}).unwrap();
    e.seed_pane_prompt(MAX_PANE,"yes",now_s());
    let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_code(st,&v,"request_not_shown","no shown message means no authority");
}

#[tokio::test]
async fn request_creation_never_dispatches_send_or_an_ordinary_allowed_mutation() {
    let e=setup().await;
    let before=e.config_snapshot();
    for (method,path,body,code) in [("POST",format!("/api/sessions/{DEV_A}/send"),json!({"text":"hello"}),"request_route_invalid"),
        ("PATCH",cfg(DEV_A),json!({"model":"sonnet"}),"request_not_required")] {
        let (st,v)=e.call(Who::Worker(DEV_LEAD),"POST","/api/delegation/requests",Some(json!({"reply_session":MAX_PANE,"method":method,"path":path,"body":body}))).await;
        assert!(st.is_client_error(),"{v}");
        assert_eq!(v["code"],code);
    }
    assert_eq!(before,e.config_snapshot());
}


#[tokio::test]
async fn shown_message_must_still_exist_in_the_bound_reply_session() {
    let e=setup().await;
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"}),MAX_PANE).await;
    e.seed_row(MAX_PANE,"non-request receipt","session",MRPOPO,Some("direct"),now_s());
    let i=id.clone();
    e.store.write(move|c| {
        c.execute("DELETE FROM cmd_history WHERE id=(SELECT shown_row FROM _amux_delegation_grants WHERE code=?1)",[i])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    e.seed_pane_prompt(MAX_PANE,"yes",now_s());
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
    let id=e.request(MRPOPO,"PATCH",&cfg(DEV_A),json!({"worktree_verify":"one"}),MAX_PANE).await;
    let (st,v)=e.type_in_pane(MAX_PANE,"approve").await;
    assert_allowed(st,&v,"record actual owner reply through the hook");
    let row=recorded_resolution(&e,"request_resolved");
    assert_eq!(view(&e,&id).await["resolution_row"],row);
    {
        let conn=e.store.read().unwrap();
        assert_eq!(conn.query_row("SELECT delegation_message_type FROM cmd_history WHERE id=?1",[row],|r|r.get::<_,String>(0)).unwrap(),"user");
    }
    // Project execution's existing type projection must not rewrite the
    // recorder's original authorship fact or erase an eligible prior reply.
    e.store.write(move|c| {
        c.execute("UPDATE cmd_history SET type='steering' WHERE id=?1",[row])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    let (st,v)=apply(&e,MRPOPO,&id,DEV_A,json!({"worktree_verify":"one"})).await;
    assert_allowed(st,&v,"owner authorship survives the project projection");

    let next=e.request(MRPOPO,"PATCH",&cfg(DEV_B),json!({"worktree_verify":"two"}),MAX_PANE).await;
    let peer=e.seed_row(MAX_PANE,"yes","session",MRPOPO,Some("direct"),now_s());
    e.store.write(move|c| {
        c.execute("UPDATE cmd_history SET type='steering',delegation_message_type='session',process_origin='remote' WHERE id=?1",[peer])?;
        Ok(amux_server::db::WriteOutcome{applied:true,events:vec![]})
    }).unwrap();
    let (st,v)=apply(&e,MRPOPO,&next,DEV_B,json!({"worktree_verify":"two"})).await;
    assert_code(st,&v,"request_unresolved","peer steering is still not owner input");
    let (st,v)=e.type_in_pane(MAX_PANE,"go ahead").await;
    assert_allowed(st,&v,"record the actual owner control");
    recorded_resolution(&e,"request_resolved");
    let (st,v)=apply(&e,MRPOPO,&next,DEV_B,json!({"worktree_verify":"two"})).await;
    assert_allowed(st,&v,"only the actual owner reply funds the second request");
}
