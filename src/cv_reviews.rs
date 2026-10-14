//! Explicit CV↔CV / single-CV reviews. No import side effects or model execution.
use crate::{cvs, error::{ApiError,Result}, storage as s, validation as v};
use axum::http::StatusCode;
use serde_json::{Value,json};
use sqlx::{Row,SqliteConnection};
use std::collections::HashSet;

pub fn handles(parts:&[&str])->bool {
 matches!(parts.first(),Some(&"cv-review-runs"|&"cv-recommendations"))
 || matches!(parts,["cvs",_,"reviews"|"recommendations"])
}
fn version(input:&Value)->Result<i64> {
 input["expected_version"].as_i64().filter(|v|*v>=0&&*v<1_000_000)
 .ok_or_else(||ApiError::field("expected_version","Укажите текущую версию состояния."))
}
fn items<'a>(input:&'a Value,key:&str)->Result<&'a Vec<Value>> {
 input[key].as_array().filter(|a|a.len()<=100)
 .ok_or_else(||ApiError::field(key,"Допускается до 100 записей."))
}
fn issue_key(input:&Value)->Result<String> {
 let key=v::required(input,"issue_key",100)?;
 if !key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_'||b==b'.') {
  return Err(ApiError::field("issue_key","Используйте устойчивый ключ из латиницы, цифр, точки, дефиса и подчёркивания."));
 }
 Ok(key)
}
fn evidence(input:&Value)->Result<Value> {
 let a=input["evidence"].as_array().filter(|a|!a.is_empty()&&a.len()<=4)
 .ok_or_else(||ApiError::field("evidence","Приведите от 1 до 4 точных цитат из входных CV."))?;
 let mut out=Vec::new();
 for e in a {v::object(e,&["revision_id","quote"])?;let quote=v::required(e,"quote",2500)?;
  if quote.chars().count()<5 {return Err(ApiError::field("evidence","Цитата должна содержать не менее 5 символов."));}
  out.push(json!({"revision_id":v::uuid(e,"revision_id")?,"quote":quote}));
 }
 Ok(json!(out))
}
pub fn normalize(parts:&[&str],input:&Value)->Result<Value> {
 match parts {
 ["cv-review-runs"]=>{
  v::object(input,&["cv_id","expected_revision_id","reference_cv_id","expected_reference_revision_id","confirmed"])?;
  if !v::boolean(input,"confirmed",false)? {return Err(ApiError::field("confirmed","Подтвердите явную проверку CV."));}
  let reference=v::optional_uuid(input,"reference_cv_id")?;
  let revision=v::optional_uuid(input,"expected_reference_revision_id")?;
  if reference.is_null()!=revision.is_null(){return Err(ApiError::field("reference_cv_id","Укажите и CV сравнения, и его ревизию."));}
  let cv=v::uuid(input,"cv_id")?;
  if reference==json!(cv) {return Err(ApiError::field("reference_cv_id","Выберите другое CV для сравнения."));}
  Ok(json!({"cv_id":cv,"cv_revision_id":v::uuid(input,"expected_revision_id")?,"reference_cv_id":reference,"reference_revision_id":revision}))
 }
 ["cv-review-runs",_,"complete"]=>{
  v::object(input,&["expected_version","attempt_id","provenance","findings","resolutions"])?;
  let p=&input["provenance"];v::object(p,&["agent","method","model"])?;
  let provenance=json!({"agent":v::required(p,"agent",200)?,"method":v::required(p,"method",200)?,"model":v::optional(p,"model",200)?});
  let mut keys=HashSet::new();let mut findings=Vec::new();let mut resolutions=Vec::new();
  for f in items(input,"findings")? {
   v::object(f,&["issue_key","field","mismatch","suggested_edit","rationale","evidence"])?;
   let key=issue_key(f)?;if !keys.insert(key.clone()){return Err(ApiError::field("issue_key","Ключи замечаний не должны повторяться."));}
   let field=v::required(f,"field",100)?;
   if !["content","summary","skills","experience","education","source","consistency"].contains(&field.as_str()) {
    return Err(ApiError::field("field","Выберите поле CV: content, summary, skills, experience, education, source, consistency."));
   }
   findings.push(json!({"issue_key":key,"field":field,"mismatch":v::required(f,"mismatch",3000)?,"suggested_edit":v::required(f,"suggested_edit",5000)?,"rationale":v::required(f,"rationale",3000)?,"evidence":evidence(f)?}));
  }
  for r in items(input,"resolutions")? {
   v::object(r,&["issue_key","rationale","evidence"])?;let key=issue_key(r)?;
   if !keys.insert(key.clone()){return Err(ApiError::field("issue_key","Нельзя одновременно подтвердить и закрыть замечание."));}
   resolutions.push(json!({"issue_key":key,"rationale":v::required(r,"rationale",3000)?,"evidence":evidence(r)?}));
  }
  Ok(json!({"expected_version":version(input)?,"attempt_id":v::uuid(input,"attempt_id")?,"provenance":provenance,"findings":findings,"resolutions":resolutions}))
 }
 ["cv-review-runs",_,"claim"]=>{
  v::object(input,&["expected_version","agent"])?;
  Ok(json!({"expected_version":version(input)?,"agent":v::required(input,"agent",200)?}))
 }
 ["cv-review-runs",_,"fail"]=>{
  v::object(input,&["expected_version","attempt_id","error"])?;
  Ok(json!({"expected_version":version(input)?,"attempt_id":v::uuid(input,"attempt_id")?,"error":v::required(input,"error",2000)?}))
 }
 ["cv-review-runs",_,"retry"|"cancel"]=>{
  v::object(input,&["expected_version"])?;Ok(json!({"expected_version":version(input)?}))
 }
 ["cv-recommendations",_,"decision"]=>{
  v::object(input,&["expected_version","state","reason"])?;
  let state=v::enum_optional(input,"state",&["active","rejected"])?;
  if state.is_null(){return Err(ApiError::field("state","Выберите активное или отклонённое состояние."));}
  Ok(json!({"expected_version":version(input)?,"state":state,"reason":v::optional(input,"reason",2000)?}))
 }
 _=>Err(ApiError::missing())
 }
}
async fn scope_recommendations(c:&mut SqliteConnection,scope:&str)->Result<Vec<Value>> {
 let rows=sqlx::query_scalar::<_,String>("SELECT data FROM cv_recommendation WHERE scope=? ORDER BY ingest_seq")
 .bind(scope).fetch_all(c).await?;
 rows.into_iter().map(|r|serde_json::from_str(&r).map_err(|_|ApiError::internal())).collect()
}
async fn event(c:&mut SqliteConnection,table:&str,foreign:&str,id:&Value,run_id:&Value,kind:&str,data:&Value)->Result<()> {
 let e=s::record(json!({"kind":kind,"snapshot":data}));
 let columns=if table=="cv_review_event"{vec![(foreign,id.clone())]}else{vec![(foreign,id.clone()),("run_id",run_id.clone())]};
 s::insert(c,table,&e,&columns).await
}
async fn staleness(c:&mut SqliteConnection,run:&Value)->Result<Vec<String>> {
 let mut reasons=Vec::new();
 for (id,revision,label) in [("cv_id","cv_revision_id","target_changed"),("reference_cv_id","reference_revision_id","reference_changed")] {
  if let Some(id)=run[id].as_str() {
   let cv=cvs::metadata(c,s::get(c,"cv",id).await?).await?;
   if cv["current_revision_id"]!=run[revision] {reasons.push(label.into());}
   if !cv["deleted_at"].is_null(){reasons.push(if label=="target_changed"{"target_deleted"}else{"reference_deleted"}.into());}
  }
 }
 let newer:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cv_review_run WHERE scope=? AND ingest_seq>(SELECT ingest_seq FROM cv_review_run WHERE id=?) AND json_extract(data,'$.state')='completed' AND json_extract(data,'$.applied')=1)")
 .bind(run["scope"].as_str()).bind(run["id"].as_str()).fetch_one(c).await?;
 if newer{reasons.push("newer_review".into());}
 Ok(reasons)
}
async fn view(c:&mut SqliteConnection,mut run:Value)->Result<Value>{
 let reasons=staleness(c,&run).await?;
 run["stale"]=json!(!reasons.is_empty());run["stale_reasons"]=json!(reasons);Ok(run)
}
async fn validate_evidence(c:&mut SqliteConnection,run:&Value,input:&Value)->Result<()> {
 let target=s::get(c,"cv_revision",run["cv_revision_id"].as_str().ok_or_else(ApiError::internal)?).await?;
 let reference=if let Some(id)=run["reference_revision_id"].as_str(){Some(s::get(c,"cv_revision",id).await?)}else{None};
 let mut seen_target=false;let mut seen_reference=false;
 for e in items(input,"evidence")? {
  let revision=if e["revision_id"]==target["id"]{seen_target=true;&target}
   else if let Some(r)=&reference {if e["revision_id"]!=r["id"]{return Err(ApiError::field("evidence","Цитата относится к посторонней ревизии."));}seen_reference=true;r}
   else{return Err(ApiError::field("evidence","Цитата относится к посторонней ревизии."));};
  let quote=e["quote"].as_str().ok_or_else(ApiError::internal)?;
  if !revision["content"].as_str().ok_or_else(ApiError::internal)?.contains(quote){
   return Err(ApiError::field("evidence","Цитата не найдена в тексте зафиксированной ревизии."));
  }
 }
 if !seen_target||(reference.is_some()&&!seen_reference){return Err(ApiError::field("evidence","Для сравнения нужны цитаты из обоих CV; для одиночной проверки — из целевого CV."));}
 Ok(())
}
async fn save_recommendation(c:&mut SqliteConnection,run:&Value,mut rec:Value,kind:&str,new:bool)->Result<()> {
 rec["version"]=json!(rec["version"].as_i64().unwrap_or(0)+1);rec["updated_at"]=json!(v::now());
 let columns=[("cv_id",run["cv_id"].clone()),("reference_cv_id",run["reference_cv_id"].clone()),("scope",run["scope"].clone()),("issue_key",rec["issue_key"].clone())];
 if new{s::insert(c,"cv_recommendation",&rec,&columns).await?;}else{s::update(c,"cv_recommendation",&rec,&[]).await?;}
 event(c,"cv_recommendation_event","recommendation_id",&rec["id"],&run["id"],kind,&rec).await
}
async fn apply(c:&mut SqliteConnection,run:&Value,input:&Value)->Result<()> {
 let mut existing=scope_recommendations(c,run["scope"].as_str().ok_or_else(ApiError::internal)?).await?;
 for f in items(input,"findings")? {
  let previous=existing.iter().find(|r|r["issue_key"]==f["issue_key"]).cloned();let new=previous.is_none();
  let mut rec=previous.unwrap_or_else(||s::record(json!({"cv_id":run["cv_id"],"reference_cv_id":run["reference_cv_id"],"scope":run["scope"],"origin_run_id":run["id"],"state":"active","version":0,"decision_reason":null})));
  for (key,value) in f.as_object().ok_or_else(ApiError::internal)? {rec[key]=value.clone();}
  rec["last_run_id"]=run["id"].clone();rec["cv_revision_id"]=run["cv_revision_id"].clone();rec["reference_revision_id"]=run["reference_revision_id"].clone();
  if rec["state"]!="rejected"{rec["state"]=json!("active");}
  save_recommendation(c,run,rec,"finding",new).await?;
 }
 for resolution in items(input,"resolutions")? {
  let rec=existing.iter_mut().find(|r|r["issue_key"]==resolution["issue_key"]).ok_or_else(||ApiError::field("resolutions","Закрыть можно только существующее замечание."))?;
  // A user's rejection is never silently replaced by an agent decision.
  if rec["state"]!="rejected" {
   rec["state"]=json!("resolved");rec["resolution"]=resolution.clone();rec["last_run_id"]=run["id"].clone();
   rec["cv_revision_id"]=run["cv_revision_id"].clone();rec["reference_revision_id"]=run["reference_revision_id"].clone();
   save_recommendation(c,run,rec.clone(),"resolved",false).await?;
  }
 }
 Ok(())
}
pub async fn write(c:&mut SqliteConnection,parts:&[&str],input:&Value)->Result<Value>{
 if parts==["cv-review-runs"] {
  for (id,revision) in [("cv_id","cv_revision_id"),("reference_cv_id","reference_revision_id")] {
   if let Some(id)=input[id].as_str(){cvs::require_active(c,id).await?;let cv=s::get(c,"cv",id).await?;
    if cv["current_revision_id"]!=input[revision]{return Err(ApiError::conflict("cv_revision_conflict"));}
   }
  }
  let scope=format!("{}:{}",input["cv_id"].as_str().ok_or_else(ApiError::internal)?,input["reference_cv_id"].as_str().unwrap_or("single"));
  let previous=scope_recommendations(c,&scope).await?;
  if previous.len()>500{return Err(ApiError::field("cv_id","В этой области больше 500 замечаний; требуется отдельная стратегия проверки."));}
  let mut run=s::record(input.clone());run["scope"]=json!(scope);run["state"]=json!("queued");run["version"]=json!(0);run["attempt_id"]=Value::Null;run["attempt_number"]=json!(0);run["previous_recommendations"]=json!(previous);
  s::insert(c,"cv_review_run",&run,&[("cv_id",run["cv_id"].clone()),("cv_revision_id",run["cv_revision_id"].clone()),("reference_cv_id",run["reference_cv_id"].clone()),("reference_revision_id",run["reference_revision_id"].clone()),("scope",run["scope"].clone())]).await?;
  event(c,"cv_review_event","run_id",&run["id"],&run["id"],"requested",&run).await?;
  return view(c,run).await;
 }
 if let ["cv-recommendations",id,"decision"]=parts {
  let mut rec=s::get(c,"cv_recommendation",id).await?;
  if rec["version"]!=input["expected_version"]{return Err(ApiError::conflict("recommendation_version_conflict"));}
  if rec["state"]=="resolved"{return Err(ApiError::conflict("recommendation_resolved"));}
  rec["state"]=input["state"].clone();rec["decision_reason"]=input["reason"].clone();
  let run=s::get(c,"cv_review_run",rec["last_run_id"].as_str().ok_or_else(ApiError::internal)?).await?;
  save_recommendation(c,&run,rec,"user_decision",false).await?;
  return s::get(c,"cv_recommendation",id).await;
 }
 let ["cv-review-runs",id,action]=parts else{return Err(ApiError::missing());};
 let mut run=s::get(c,"cv_review_run",id).await?;
 if run["version"]!=input["expected_version"]{return Err(ApiError::conflict("review_version_conflict"));}
 let state=run["state"].as_str().ok_or_else(ApiError::internal)?;
 match *action {
  "claim" if state=="queued"=>{
   run["state"]=json!("running");run["attempt_id"]=json!(v::id());run["attempt_number"]=json!(run["attempt_number"].as_i64().unwrap_or(0)+1);run["agent"]=input["agent"].clone();
  }
  "complete"|"fail" if state=="running"=>{
   if run["attempt_id"]!=input["attempt_id"]{return Err(ApiError::conflict("review_attempt_conflict"));}
   if *action=="fail"{run["state"]=json!("failed");run["error"]=input["error"].clone();}
   else {
    if run["agent"]!=input["provenance"]["agent"]{return Err(ApiError::field("provenance","Результат должен принадлежать агенту, принявшему проверку."));}
    for f in items(input,"findings")?.iter().chain(items(input,"resolutions")?){validate_evidence(c,&run,f).await?;}
    for resolution in items(input,"resolutions")? {
     if !run["previous_recommendations"].as_array().ok_or_else(ApiError::internal)?.iter().any(|r|r["issue_key"]==resolution["issue_key"]){
      return Err(ApiError::field("resolutions","Замечание отсутствует во входном пакете этой проверки."));
     }
    }
    let stale=staleness(c,&run).await?;
    run["applied"]=json!(stale.is_empty());run["result"]=input.clone();run["state"]=json!("completed");
    if stale.is_empty(){apply(c,&run,input).await?;}
   }
  }
  "retry" if state=="failed"=>{
   if !staleness(c,&run).await?.is_empty(){return Err(ApiError::conflict("review_stale"));}
   run["state"]=json!("queued");run["attempt_id"]=Value::Null;run["error"]=Value::Null;
  }
  "cancel" if ["queued","running","failed"].contains(&state)=>{run["state"]=json!("cancelled");}
  _=>return Err(ApiError::conflict("review_state_conflict"))
 }
 run["version"]=json!(run["version"].as_i64().ok_or_else(ApiError::internal)?+1);run["updated_at"]=json!(v::now());
 s::update(c,"cv_review_run",&run,&[]).await?;
 event(c,"cv_review_event","run_id",&run["id"],&run["id"],action,&run).await?;
 view(c,run).await
}
async fn page(c:&mut SqliteConnection,table:&str,column:&str,id:&str,query:&str)->Result<Value>{
 let mut cursor=i64::MAX;let mut limit=100i64;let mut seen=HashSet::new();
 for (key,value) in url::form_urlencoded::parse(query.as_bytes()){
  if !seen.insert(key.to_string()){return Err(ApiError::field("query","Повторяющийся параметр."));}
  match key.as_ref(){
   "cursor"=>cursor=value.parse().ok().filter(|n|*n>0).ok_or_else(||ApiError::field("cursor","Некорректный курсор."))?,
   "limit"=>limit=value.parse().ok().filter(|n|*n>0&&*n<=100).ok_or_else(||ApiError::field("limit","Лимит от 1 до 100."))?,
   _=>return Err(ApiError::field("query","Неизвестный параметр."))
  }
 }
 let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT ingest_seq,data FROM {table} WHERE {column}=? AND ingest_seq<? ORDER BY ingest_seq DESC LIMIT ?")))
 .bind(id).bind(cursor).bind(limit+1).fetch_all(&mut *c).await?;
 let more=rows.len()>limit as usize;let mut out=Vec::new();let mut last=0;
 for row in rows.into_iter().take(limit as usize){
  last=row.get::<i64,_>("ingest_seq");let raw:Value=serde_json::from_str(row.get("data")).map_err(|_|ApiError::internal())?;
  out.push(if table=="cv_review_run"{view(c,raw).await?}else if table=="cv_recommendation"{
   let run=s::get(c,"cv_review_run",raw["last_run_id"].as_str().ok_or_else(ApiError::internal)?).await?;
   let mut rec=raw;rec["stale"]=json!(!staleness(c,&run).await?.is_empty());rec
  }else{raw});
 }
 Ok(json!({"items":out,"next_cursor":if more{Some(last.to_string())}else{None}}))
}
pub async fn read(c:&mut SqliteConnection,parts:&[&str],query:&str)->Result<Value>{
 match parts {
  ["cvs",id,"reviews"]|["cvs",id,"recommendations"]=>{
   s::get(c,"cv",id).await?;
   page(c,if parts[2]=="reviews"{"cv_review_run"}else{"cv_recommendation"},"cv_id",id,query).await
  }
  ["cv-review-runs",id,"history"]=>{s::get(c,"cv_review_run",id).await?;page(c,"cv_review_event","run_id",id,query).await}
  ["cv-recommendations",id,"history"]=>{s::get(c,"cv_recommendation",id).await?;page(c,"cv_recommendation_event","recommendation_id",id,query).await}
  ["cv-review-runs",id]|["cv-review-runs",id,"inputs"]=>{
   if !query.is_empty(){return Err(ApiError::field("query","Параметры здесь не поддерживаются."));}
   let run=s::get(c,"cv_review_run",id).await?;
   if parts.len()==2{return Ok(json!({"data":view(c,run).await?}));}
   let target=s::get(c,"cv_revision",run["cv_revision_id"].as_str().ok_or_else(ApiError::internal)?).await?;
   let reference=if let Some(id)=run["reference_revision_id"].as_str(){s::get(c,"cv_revision",id).await?}else{Value::Null};
   Ok(json!({"data":{"run":view(c,run.clone()).await?,"target_revision":target,"reference_revision":reference,"previous_recommendations":run["previous_recommendations"],
    "instructions":"Проверяйте только явно запрошенные входы. Целевое CV проверяется относительно reference_revision. Не выдумывайте опыт, навыки, образование и факты. Предлагайте уточнения с условием подтверждения пользователем. Цитируйте точный текст content обеих ревизий для сравнения. Сохраняйте issue_key известных замечаний. Устранение подтверждайте явно в resolutions после повторной проверки; пропуск замечания не закрывает его. Не редактируйте внешние сайты и не отправляйте отклики."}}))
  }
  _=>Err(ApiError::missing())
 }
}
