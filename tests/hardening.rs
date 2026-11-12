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

#[test]
fn grouped_filters_and_projected_aliases_preserve_order_and_mutations() {
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();
 ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY, label VARCHAR(4), v VECTOR(2))");
 ok(&mut s,"INSERT INTO t VALUES(1,'甲','[1,0]'),(2,'乙','[-1,0]'),(3,'丙','[3,0]')");
 let sql="SELECT id AS item_id, L2_DISTANCE(v,'[0,0]') distance FROM t WHERE ((id>=1 AND id<=3) AND (label='甲' AND id=1)) ORDER BY distance DESC LIMIT 2";
 let r=match s.execute(sql) {Response::Rows(r)=>r,x=>panic!("alias/grouped filter failed: {x:?}")};
 assert_eq!(r.columns,vec!["item_id","distance"]);assert_eq!(r.rows,vec![vec![Cell::Int(1),Cell::Real(1.0)]]);
 assert_eq!(rows(&mut s,"SELECT id item_id FROM t WHERE (id>=1 AND id<=2) ORDER BY item_id DESC"),vec![vec![Cell::Int(2)],vec![Cell::Int(1)]]);
 ok(&mut s,"BEGIN");ok(&mut s,"UPDATE t SET label='改' WHERE (id=1 AND (id>=1))");ok(&mut s,"DELETE FROM t WHERE (id=2)");ok(&mut s,"ROLLBACK");
 assert!(matches!(s.execute("SELECT id AS item_id FROM t WHERE (id=1 AND label='甲') OR id=2"),Response::Err(_)));
}

#[test]
fn direct_execution_understands_comments_without_joining_keywords() {
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();
 ok(&mut s,"CREATE/*gap*/TABLE t(id BIGINT PRIMARY KEY,s VARCHAR(20))");
 ok(&mut s,"INSERT/*gap*/INTO t VALUES(1,'/*文字*/;--')");
 assert_eq!(rows(&mut s,"SELECT id-- gap\nFROM t WHERE id/*gap*/=1"),vec![vec![Cell::Int(1)]]);
 assert!(matches!(s.execute("SELECT 1 /* never closed"),Response::Err(_)));
}

#[test]
fn repeated_filter_selections_keep_ties_and_variable_offsets_correct() {
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();
 ok(&mut s,"CREATE TABLE t(label VARCHAR(300),g BIGINT,v VECTOR(3),id BIGINT PRIMARY KEY)");
 let mut model=Vec::new();
 for batch in 0..5 {
  let mut inserts=Vec::new();
  for i in batch*50..(batch+1)*50 {
   let label="测".repeat(i as usize%71);let v=[(i%13-6) as f64,(i%7-3) as f64,(i%5-2) as f64];
   inserts.push(format!("('{label}',{},'[{}, {}, {}]',{i})",i%40,v[0],v[1],v[2]));model.push((i,i%40,label,v));
  }
  ok(&mut s,&format!("INSERT INTO t VALUES {}",inserts.join(",")));
 }
 for round in 0..3 {
  for group in 0..45 {
   let asc=round%2==0;
   let mut expected:Vec<_>=model.iter().filter(|(_,g,_,_)|*g==group).collect();
   expected.sort_by(|a,b|{let da=a.3.iter().map(|v|v*v).sum::<f64>();let db=b.3.iter().map(|v|v*v).sum::<f64>();(if asc {da.total_cmp(&db)}else{db.total_cmp(&da)}).then_with(||a.0.cmp(&b.0))});
   let want:Vec<_>=expected.into_iter().take(5).map(|(id,_,label,v)|vec![Cell::Int(*id),Cell::Text(label.clone()),Cell::Vector(v.iter().map(|x|*x as f32).collect())]).collect();
   let got=rows(&mut s,&format!("SELECT id,label,v FROM t WHERE g={group} ORDER BY L2_DISTANCE(v,'[0,0,0]') {} LIMIT 5",if asc{"ASC"}else{"DESC"}));
   assert_eq!(got,want,"round {round} group {group}");
  }
 }
}

#[test]
fn vector_text_fast_path_matches_numeric_grammar_and_rejects_partial_insert() {
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,v VECTOR(3))");
 ok(&mut s,"INSERT INTO t VALUES(1,'[1e-3, -2E+1, 3.141592653589793]')");
 assert_eq!(rows(&mut s,"SELECT v FROM t"),vec![vec![Cell::Vector(vec![0.001,-20.,std::f32::consts::PI])]]);
 for bad in ["[]","[1,,2]","[1,2,]","[1,2,NaN]","[1,2,1e50]","[1,2,1.]","[1,2,1e]","[1,2,1 2]","[1,2,1.e2]","[1,2,3]extra"] {
  assert!(matches!(s.execute(&format!("INSERT INTO t VALUES(2,'[0,0,0]'),(3,'{bad}')")),Response::Err(_)),"{bad}");
  assert_eq!(rows(&mut s,"SELECT id FROM t ORDER BY id"),vec![vec![Cell::Int(1)]]);
 }
 // Comments still use the full lexer path rather than being mistaken for numbers.
 ok(&mut s,"INSERT INTO t VALUES(2,'[1,/* gap */2,3]')");
 assert_eq!(rows(&mut s,"SELECT v FROM t WHERE id=2"),vec![vec![Cell::Vector(vec![1.,2.,3.])]]);
}

#[test]
fn large_cached_prefilter_matches_exact_filtered_topk_and_ties() {
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();
 ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,g BIGINT,v VECTOR(16))");
 let mut model=Vec::new();
 for batch in 0..30 {
  let mut inserts=Vec::new();
  for i in batch*100..(batch+1)*100 {
   let v=(0..16).map(|d|((i*17+d*13)%23) as f32-11.0).collect::<Vec<_>>();
   inserts.push(format!("({i},{},'[{}]')",i%10,v.iter().map(|x|x.to_string()).collect::<Vec<_>>().join(",")));
   model.push((i as i64,(i%10) as i64,v));
  }
  ok(&mut s,&format!("INSERT INTO t VALUES {}",inserts.join(",")));
 }
 for group in 0..10 {
  let mut expected=model.iter().filter(|(_,g,_)|*g==group).collect::<Vec<_>>();
  expected.sort_by(|a,b|{
   let da=a.2.iter().map(|v|{let v=f64::from(*v);v*v}).sum::<f64>();
   let db=b.2.iter().map(|v|{let v=f64::from(*v);v*v}).sum::<f64>();
   da.total_cmp(&db).then_with(||a.0.cmp(&b.0))
  });
  let want=expected.into_iter().take(10).map(|(id,_,_)|vec![Cell::Int(*id)]).collect::<Vec<_>>();
  let got=rows(&mut s,&format!("SELECT id FROM t WHERE g={group} ORDER BY L2_DISTANCE(v,'[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]') LIMIT 10"));
  assert_eq!(got,want,"group {group}");
 }
}

#[test]
fn wide_filtered_subset_keeps_exact_topk_order() {
 let d=Db::new();let mut s=Session::open(&d.0).unwrap();
 ok(&mut s,"CREATE TABLE t(id BIGINT PRIMARY KEY,g BIGINT,v VECTOR(8))");
 let mut model=Vec::new();
 for batch in 0..60 {
  let mut inserts=Vec::new();
  for i in batch*100..(batch+1)*100 {
   // Only 24 distinct vectors, so the top 50 is full of distance ties that
   // must come back in primary-key order.
   let v=(0..8).map(|d|((i*7+d*5)%24) as f32-11.5).collect::<Vec<_>>();
   inserts.push(format!("({i},{},'[{}]')",i%4,v.iter().map(|x|x.to_string()).collect::<Vec<_>>().join(",")));
   model.push((i as i64,(i%4) as i64,v));
  }
  ok(&mut s,&format!("INSERT INTO t VALUES {}",inserts.join(",")));
 }
 for (probe,group) in [(0usize,1i64),(1,3),(2,0)] {
  let mut expected=model.iter().filter(|(_,g,_)|*g==group).collect::<Vec<_>>();
  let query=(0..8).map(|d|((probe*13+d*3)%17) as f32*0.75).collect::<Vec<_>>();
  let q=query.iter().map(|x|x.to_string()).collect::<Vec<_>>().join(",");
  expected.sort_by(|a,b|{
   let da=a.2.iter().zip(&query).map(|(v,q)|{let d=f64::from(*v)-f64::from(*q);d*d}).sum::<f64>();
   let db=b.2.iter().zip(&query).map(|(v,q)|{let d=f64::from(*v)-f64::from(*q);d*d}).sum::<f64>();
   da.total_cmp(&db).then_with(||a.0.cmp(&b.0))
  });
  let want=expected.into_iter().take(50).map(|(id,_,_)|vec![Cell::Int(*id)]).collect::<Vec<_>>();
  let got=rows(&mut s,&format!("SELECT id FROM t WHERE g={group} ORDER BY L2_DISTANCE(v,'[{q}]') LIMIT 50"));
  assert_eq!(got,want,"probe {probe} group {group}");
 }
}
