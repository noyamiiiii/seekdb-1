use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize,Ordering};
use minivectordb::{executor::Session,response::{Response,Cell}};
static NEXT:AtomicUsize=AtomicUsize::new(0);
struct Db(PathBuf);
impl Db {fn new()->Self {let p=std::env::temp_dir().join(format!("mvdb-harden-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));std::fs::create_dir_all(&p).unwrap();Self(p)}}
impl Drop for Db {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
fn ok(s:&mut Session,sql:&str){assert!(!matches!(s.execute(sql),Response::Err(_)),"{sql}");}
fn rows(s:&mut Session,sql:&str)->Vec<Vec<Cell>>{match s.execute(sql){Response::Rows(r)=>r.rows,x=>panic!("{sql}: {x:?}")}}
#[test]
fn failed_statement_keeps_prior_writes_but_no_partial_rows(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();
 ok(&mut s,"CREATE TABLE t (id BIGINT PRIMARY KEY, s VARCHAR(2))");ok(&mut s,"BEGIN");ok(&mut s,"INSERT INTO t VALUES (1,'ok')");
 assert!(matches!(s.execute("INSERT INTO t VALUES (2,'ok'),(3,'long')"),Response::Err(_)));
 assert_eq!(rows(&mut s,"SELECT id FROM t ORDER BY id"),vec![vec![Cell::Int(1)]]);
 ok(&mut s,"COMMIT");drop(s);let mut s=Session::open(&d.0).unwrap();assert_eq!(rows(&mut s,"SELECT id FROM t"),vec![vec![Cell::Int(1)]]);
}
#[test]
fn normal_shutdown_discards_uncommitted_changes(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY)");ok(&mut s,"INSERT INTO t VALUES(1)");ok(&mut s,"BEGIN");ok(&mut s,"DELETE FROM t");ok(&mut s,"INSERT INTO t VALUES(2)");s.shutdown().unwrap();drop(s);
 let mut s=Session::open(&d.0).unwrap();assert_eq!(rows(&mut s,"SELECT id FROM t"),vec![vec![Cell::Int(1)]]);
}
#[test]
fn large_records_cross_pages_and_spill_without_losing_visibility(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY, text VARCHAR(200000), v VECTOR(2))");
 let text="测".repeat(100000);ok(&mut s,&format!("INSERT INTO t VALUES(1,'{text}','[3,4]')"));
 assert_eq!(rows(&mut s,"SELECT text FROM t WHERE id=1"),vec![vec![Cell::Text(text.clone())]]);
 ok(&mut s,"BEGIN");ok(&mut s,"UPDATE t SET text='short',v='[0,0]' WHERE id=1");ok(&mut s,"ROLLBACK");
 assert_eq!(rows(&mut s,"SELECT L2_DISTANCE(v,'[0,0]') AS d FROM t ORDER BY d LIMIT 1"),vec![vec![Cell::Real(5.0)]]);
 s.shutdown().unwrap();drop(s);let mut s=Session::open(&d.0).unwrap();assert_eq!(rows(&mut s,"SELECT text FROM t WHERE id=1"),vec![vec![Cell::Text(text)]]);
}
#[test]
fn dense_delete_insert_retains_rids_and_rolls_back(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,s VARCHAR(1000))");
 let payload="a".repeat(900);
 for batch in 0..3 {let values=(batch*100..(batch+1)*100).map(|i|format!("({i},'{payload}')")).collect::<Vec<_>>().join(",");ok(&mut s,&format!("INSERT INTO t VALUES {values}"));}
 ok(&mut s,"BEGIN");ok(&mut s,"DELETE FROM t WHERE id<150");let values=(300..450).map(|i|format!("({i},'{payload}')")).collect::<Vec<_>>().join(",");ok(&mut s,&format!("INSERT INTO t VALUES {values}"));ok(&mut s,"ROLLBACK");
 for i in 0..300 {assert_eq!(rows(&mut s,&format!("SELECT id FROM t WHERE id={i}")),vec![vec![Cell::Int(i)]]);}
}
#[test]
fn binding_fails_even_on_empty_table_or_limit_zero(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,v VECTOR(2))");
 for sql in ["SELECT id FROM t WHERE nope=1","SELECT id FROM t WHERE nope=1 LIMIT 0","UPDATE t SET id=2 WHERE id=9","DELETE FROM t WHERE nope=1"] {assert!(matches!(s.execute(sql),Response::Err(_)),"{sql}");}
}
#[test]
fn cached_topk_obeys_write_delete_rollback_and_reopen(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,v VECTOR(2))");ok(&mut s,"INSERT INTO t VALUES(1,'[1,0]'),(2,'[2,0]')");
 let q="SELECT id FROM t ORDER BY L2_DISTANCE(v,'[0,0]') LIMIT 1";
 assert_eq!(rows(&mut s,q),vec![vec![Cell::Int(1)]]);ok(&mut s,"BEGIN");ok(&mut s,"DELETE FROM t WHERE id=1");assert_eq!(rows(&mut s,q),vec![vec![Cell::Int(2)]]);ok(&mut s,"ROLLBACK");assert_eq!(rows(&mut s,q),vec![vec![Cell::Int(1)]]);
 ok(&mut s,"UPDATE t SET v='[0,0]' WHERE id=2");assert_eq!(rows(&mut s,q),vec![vec![Cell::Int(2)]]);s.shutdown().unwrap();drop(s);let mut s=Session::open(&d.0).unwrap();assert_eq!(rows(&mut s,q),vec![vec![Cell::Int(2)]]);
}
#[test]
fn unicode_codepoints_bigints_and_invalid_vectors(){
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,s VARCHAR(2),v VECTOR(2))");
 ok(&mut s,"INSERT INTO t VALUES(9223372036854775807,'😀a','[1,0]'),(-9223372036854775808,'中','[0,1]')");
 assert_eq!(rows(&mut s,"SELECT id FROM t ORDER BY s"),vec![vec![Cell::Int(i64::MIN)],vec![Cell::Int(i64::MAX)]]);
 for v in ["[NaN,0]","[Infinity,0]","[1e100,0]","[1]","[1,2,3]"] {assert!(matches!(s.execute(&format!("INSERT INTO t VALUES(2,'x','{v}')")),Response::Err(_)),"{v}");}
 assert_eq!(rows(&mut s,"SELECT id FROM t WHERE id=9223372036854775807"),vec![vec![Cell::Int(i64::MAX)]]);
}
