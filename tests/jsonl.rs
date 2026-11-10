use std::{io::{BufRead,BufReader,Write},process::{Command,Child,Stdio},sync::{mpsc,atomic::{AtomicUsize,Ordering}},time::Duration,path::{Path,PathBuf}};
use serde_json::{Value,json};
static NEXT:AtomicUsize=AtomicUsize::new(0);
fn dir()->PathBuf { let p=std::env::temp_dir().join(format!("mvdb-jsonl-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));std::fs::create_dir_all(&p).unwrap();p }
struct Server { child:Child, rx:mpsc::Receiver<String> }
impl Server {
 fn open(p:&Path)->Self {
  let mut child=Command::new(env!("CARGO_BIN_EXE_minivectordb")).args(["--data-dir"]).arg(p).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
  let out=child.stdout.take().unwrap();let(tx,rx)=mpsc::channel();
  std::thread::spawn(move||{for line in BufReader::new(out).lines(){if tx.send(line.unwrap()).is_err(){break}}});
  let s=Self{child,rx};assert_eq!(s.read(),json!({"type":"ready"}));s
 }
 fn read(&self)->Value {serde_json::from_str(&self.rx.recv_timeout(Duration::from_secs(5)).expect("response blocked while stdin was open")).unwrap()}
 fn request(&mut self,r:Value)->Value {writeln!(self.child.stdin.as_mut().unwrap(),"{r}").unwrap();self.child.stdin.as_mut().unwrap().flush().unwrap();let v=self.read();assert_eq!(v["id"],r["id"]);assert!(v["ok"].is_boolean());v}
 fn sql(&mut self,sql:&str)->Value {self.request(json!({"id":1,"op":"execute","sql":sql,"search_mode":"exact"}))}
 fn stop(&mut self) {assert_eq!(self.request(json!({"id":"stop","op":"shutdown"})),json!({"id":"stop","ok":true}));assert!(self.child.wait().unwrap().success());assert!(self.rx.recv_timeout(Duration::from_secs(1)).is_err());}
}
impl Drop for Server {fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();}}
#[test]
fn default_ready_and_each_response_arrive_before_stdin_closes() {let p=dir();let mut s=Server::open(&p);assert_eq!(s.sql("SELECT 1;")["rows"],json!([[1]]));s.stop();}
#[test]
fn typed_bigints_unicode_controls_and_vector_projection_roundtrip() {
 let p=dir();let mut s=Server::open(&p);
 assert_eq!(s.sql("CREATE TABLE t(id BIGINT PRIMARY KEY,txt VARCHAR(64),v VECTOR(2));")["ok"],true);
 let text=" \";|\n你好😀\t\\ ' ";let escaped=text.replace('\\',"\\\\").replace('\'',"''");
 let insert=format!("INSERT INTO t VALUES(-9223372036854775808,'{escaped}','[0.1,3]'),(9223372036854775807,'max','[0,0]');");
 assert_eq!(s.sql(&insert)["affected_rows"],2);
 let r=s.sql("SELECT id,txt,v FROM t ORDER BY id;");assert_eq!(r["columns"],json!(["id","txt","v"]));
 assert_eq!(r["rows"][0][0],json!(i64::MIN));assert_eq!(r["rows"][1][0],json!(i64::MAX));assert_eq!(r["rows"][0][1],text);
 assert!(r["rows"][0][2].is_array());assert_eq!(r["rows"][0][2][0].as_f64().unwrap(),0.1f32 as f64);s.stop();
}
#[test]
fn request_ids_keep_json_type_and_arbitrary_integer_precision() {
 let p=dir();let mut s=Server::open(&p);
 for raw in ["9007199254740993","1844674407370955161600000","-9223372036854775809","1.25","\"中文-id\"","null","true"] {
  let id:Value=serde_json::from_str(raw).unwrap();let r=s.request(json!({"id":id,"op":"execute","sql":"SELECT 1;"}));assert_eq!(r["ok"],true);
 }s.stop();
}
#[test]
fn sql_and_request_errors_do_not_close_session_or_execute_a_batch() {
 let p=dir();let mut s=Server::open(&p);assert_eq!(s.sql("CREATE TABLE t(id BIGINT PRIMARY KEY);")["ok"],true);
 assert_eq!(s.sql("INSERT INTO t VALUES(1); INSERT INTO t VALUES(2);")["ok"],false);
 assert_eq!(s.sql("SELECT id FROM t;")["rows"],json!([]));
 s.sql("INSERT INTO t VALUES(1);");let r=s.sql("INSERT INTO t VALUES(1);");assert_eq!(r["ok"],false);assert!(r["error"]["code"].as_str().unwrap().len()>0);
 assert_eq!(s.request(json!({"id":2,"op":"execute","sql":"DELETE FROM t;","search_mode":"bad"}))["ok"],false);
 assert_eq!(s.sql("SELECT id FROM t;")["rows"],json!([[1]]));s.stop();
}
#[test]
fn shutdown_rolls_back_before_ack_and_restart_restores_commits() {
 let p=dir();let mut s=Server::open(&p);s.sql("CREATE TABLE t(id BIGINT PRIMARY KEY);");s.sql("INSERT INTO t VALUES(1);");s.sql("BEGIN;");s.sql("DELETE FROM t;");s.stop();
 let mut s=Server::open(&p);assert_eq!(s.sql("SELECT id FROM t;")["rows"],json!([[1]]));s.stop();
}
#[test]
fn ann_request_uses_correct_filtered_topk_and_empty_columns() {
 let p=dir();let mut s=Server::open(&p);s.sql("CREATE TABLE t(id BIGINT PRIMARY KEY,g BIGINT,v VECTOR(2));");s.sql("INSERT INTO t VALUES(2,1,'[1,0]'),(1,1,'[0,1]'),(3,2,'[0,0]');");
 let r=s.request(json!({"id":7,"op":"execute","sql":"SELECT id FROM t WHERE g=1 ORDER BY L2_DISTANCE(v,'[0,0]') LIMIT 2;","search_mode":"ann"}));assert_eq!(r["rows"],json!([[1],[2]]));
 let r=s.sql("SELECT id FROM t WHERE g=9;");assert_eq!(r["columns"],json!(["id"]));assert_eq!(r["rows"],json!([]));s.stop();
}