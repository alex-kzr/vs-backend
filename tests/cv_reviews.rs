use axum::{Router, body::{Body,to_bytes}, http::{Request,StatusCode}};
use serde_json::{Value,json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use vacancy_search_backend::{AppState,open_database,router};
struct App { app:Router, pool:SqlitePool, _dir:TempDir }
impl App {
 async fn new()->Self {
  let dir=tempfile::tempdir().unwrap();
  let pool=open_database(&dir.path().join("synthetic.sqlite3")).await.unwrap();
  let app=router(AppState{pool:pool.clone(),hosts:vec!["localhost".into()],origins:vec!["http://localhost".into()],hh:Default::default()},None);
  Self{app,pool,_dir:dir}
 }
 async fn call(&self,method:&str,path:&str,input:Value,key:Option<&str>)->(StatusCode,Value){
  let mut r=Request::builder().method(method).uri(format!("/api/v1/{path}")).header("host","localhost").header("content-type","application/json");
  if let Some(k)=key {r=r.header("Idempotency-Key",k);}
  let response=self.app.clone().oneshot(r.body(if method=="GET"{Body::empty()}else{Body::from(input.to_string())}).unwrap()).await.unwrap();
  let status=response.status();let bytes=to_bytes(response.into_body(),4*1024*1024).await.unwrap();
  (status,serde_json::from_slice(&bytes).unwrap())
 }
 async fn post(&self,path:&str,input:Value)->Value {
  let(s,v)=self.call("POST",path,input,Some(&uuid::Uuid::new_v4().to_string())).await;
  assert!(s.is_success(),"{s}: {v}");v["data"].clone()
 }
 async fn get(&self,path:&str)->Value {let(s,v)=self.call("GET",path,Value::Null,None).await;assert_eq!(s,StatusCode::OK,"{v}");v}
 async fn cv(&self,name:&str)->Value {self.post("cvs",draft(name)).await}
 async fn run(&self,cv:&Value,reference:Option<&Value>)->Value {
  self.post("cv-review-runs",json!({"cv_id":cv["id"],"expected_revision_id":cv["current_revision_id"],"reference_cv_id":reference.map(|r|r["id"].clone()),"expected_reference_revision_id":reference.map(|r|r["current_revision_id"].clone()),"confirmed":true})).await
 }
 async fn claim(&self,run:&Value)->Value {self.post(&format!("cv-review-runs/{}/claim",run["id"].as_str().unwrap()),json!({"expected_version":run["version"],"agent":"synthetic-test"})).await}
 async fn complete(&self,run:&Value,findings:Value,resolutions:Value)->Value {self.post(&format!("cv-review-runs/{}/complete",run["id"].as_str().unwrap()),json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":findings,"resolutions":resolutions})).await}
}
fn draft(name:&str)->Value {json!({"name":name,"content":"Synthetic CV. Rust APIs. SQL.","structured":{},"source":{"kind":"manual_text","observed_at":"2026-10-14T12:00:00Z"},"confirmed":true})}
fn finding(cv:&Value)->Value {json!({"issue_key":"summary-specificity","field":"summary","mismatch":"Описание задач недостаточно конкретно.","suggested_edit":"Уточнить, какие API разрабатывались, если это соответствует опыту.","rationale":"В тексте указаны API, но нет описания задач.","evidence":[{"revision_id":cv["current_revision_id"],"quote":"Rust APIs"}]})}
fn resolution(cv:&Value)->Value {json!({"issue_key":"summary-specificity","rationale":"Повторно проверено описание задач.","evidence":[{"revision_id":cv["current_revision_id"],"quote":"Rust APIs"}]})}
#[tokio::test]
async fn explicit_run_pins_inputs_and_preserves_delivery_and_decisions() {
 let t=App::new().await;let cv=t.cv("Synthetic target").await;
 let created=t.run(&cv,None).await;
 assert_eq!(created["state"],"queued");
 assert_eq!(t.get(&format!("cvs/{}/recommendations",cv["id"].as_str().unwrap())).await["items"],json!([]));
 let packet=t.get(&format!("cv-review-runs/{}/inputs",created["id"].as_str().unwrap())).await;
 assert_eq!(packet["data"]["target_revision"]["content"],draft("x")["content"]);
 let run=t.claim(&created).await;let key=uuid::Uuid::new_v4().to_string();
 let body=json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[finding(&cv)],"resolutions":[]});
 let path=format!("cv-review-runs/{}/complete",run["id"].as_str().unwrap());
 let(s,first)=t.call("POST",&path,body.clone(),Some(&key)).await;assert_eq!(s,StatusCode::OK,"{first}");
 let(s,replay)=t.call("POST",&path,body.clone(),Some(&key)).await;assert_eq!(s,StatusCode::OK);assert_eq!(replay["deduplicated"],true);assert_eq!(first["data"],replay["data"]);
 let mut changed=body;changed["findings"]=json!([]);assert_eq!(t.call("POST",&path,changed,Some(&key)).await.0,StatusCode::CONFLICT);
 let mut rec=t.get(&format!("cvs/{}/recommendations",cv["id"].as_str().unwrap())).await["items"][0].clone();
 rec=t.post(&format!("cv-recommendations/{}/decision",rec["id"].as_str().unwrap()),json!({"expected_version":rec["version"],"state":"rejected","reason":"Не подходит."})).await;
 let second=t.run(&cv,None).await;let second=t.claim(&second).await;t.complete(&second,json!([finding(&cv)]),json!([])).await;
 let current=t.get(&format!("cvs/{}/recommendations",cv["id"].as_str().unwrap())).await["items"][0].clone();
 assert_eq!(current["id"],rec["id"]);assert_eq!(current["state"],"rejected");assert_eq!(current["decision_reason"],"Не подходит.");
 assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM cv_recommendation").fetch_one(&t.pool).await.unwrap(),1);
 let history=t.get(&format!("cv-recommendations/{}/history",rec["id"].as_str().unwrap())).await;assert_eq!(history["items"].as_array().unwrap().len(),3);
}
