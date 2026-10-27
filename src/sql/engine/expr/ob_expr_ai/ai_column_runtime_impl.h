/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once
#include "sql/engine/expr/ob_expr_ai/ai_column_runtime.h"
#include "sql/engine/dml/ob_dml_service.h"
#include "sql/engine/dml/ob_table_modify_op.h"
#include "query/session/ob_session_inner_sql.h"
#include "common/mysqlclient/ob_mysql_transaction.h"
#include <atomic>
#include <thread>
#include <chrono>
#include <map>
#include <set>
#include <string>
#include <vector>
#include <rapidjson/document.h>
#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>

namespace oceanbase { namespace sql { namespace ai_column {
using namespace common;
using namespace share::schema;
namespace {
constexpr char marker[] = "seekdb:ai-column:v1:";
std::atomic<bool> stopping{true};
std::thread worker;
thread_local bool worker_publish = false;
thread_local bool in_model_request = false;
thread_local bool in_nested_statement = false;
thread_local bool reading_internal = false;
thread_local bool resolving_retry = false;
struct FlagGuard { bool &flag;bool previous;explicit FlagGuard(bool &value):flag(value),previous(value){flag=true;}~FlagGuard(){flag=previous;} };
thread_local std::string requested_system_prompt;
std::atomic<uint64_t> request_sequence{0};
std::string string(const ObString &s) { return std::string(s.ptr() ? s.ptr() : "", s.length()); }
std::string identifier(const std::string &s) {
  std::string out="`";for(char c:s){out+=c;if(c=='`')out+='`';}return out+'`';
}
std::string literal(const std::string &s) {
  static const char hex[]="0123456789abcdef";
  std::string out="CONVERT(X'";out.reserve(s.size()*2+32);
  for(unsigned char c:s){out+=hex[c>>4];out+=hex[c&15];}return out+"' USING utf8mb4)";
}
struct Definition { std::string expression; int64_t retries=3; std::vector<uint64_t> dependencies; };
bool definition(const ObColumnSchemaV2 &column,Definition &out) {
  const auto comment=string(column.get_comment_str());
  if(comment.compare(0,sizeof(marker)-1,marker)!=0)return false;
  rapidjson::Document doc;doc.Parse(comment.data()+sizeof(marker)-1,comment.size()-sizeof(marker)+1);
  if(doc.HasParseError()||!doc.IsObject()||!doc.HasMember("expression")||!doc["expression"].IsString())return false;
  out.expression=doc["expression"].GetString();out.retries=doc["retries"].GetInt64();return true;
}
std::string hidden(const char *kind,uint64_t id){return std::string("__ai_")+kind+"_"+std::to_string(id);}

// Lex SQL identifiers without interpreting quoted strings as column references.
template<class Callback> std::string rewrite(const std::string &sql,Callback callback) {
  std::string out;
  for(size_t i=0;i<sql.size();) {
    const size_t begin=i;
    if(sql[i]=='\''||sql[i]=='"') {
      const char q=sql[i++];bool closed=false;
      while(i<sql.size()) {
        if(sql[i]=='\\'){i+=2;continue;}
        if(sql[i++]==q){if(i<sql.size()&&sql[i]==q){++i;continue;}closed=true;break;}
      }
      if(!closed)throw std::runtime_error("AI column: unterminated string");
      out+=sql.substr(begin,i-begin);
    } else if(sql[i]=='`') {
      ++i;std::string name;bool closed=false;
      while(i<sql.size()){char c=sql[i++];if(c=='`'){if(i<sql.size()&&sql[i]=='`'){++i;name+='`';}else{closed=true;break;}}else name+=c;}
      if(!closed)throw std::runtime_error("AI column: unterminated identifier");
      out+=callback(name,sql.substr(begin,i-begin),false);
    } else if(std::isalpha(static_cast<unsigned char>(sql[i]))||sql[i]=='_') {
      while(i<sql.size()&&(std::isalnum(static_cast<unsigned char>(sql[i]))||sql[i]=='_'||sql[i]=='$'))++i;
      size_t next=i;while(next<sql.size()&&std::isspace(static_cast<unsigned char>(sql[next])))++next;
      out+=callback(sql.substr(begin,i-begin),sql.substr(begin,i-begin),next<sql.size()&&sql[next]=='(');
    } else {
      if(sql[i]==';'||(sql[i]=='-'&&i+1<sql.size()&&sql[i+1]=='-')
          ||(sql[i]=='/'&&i+1<sql.size()&&sql[i+1]=='*'))throw std::runtime_error("AI column: multiple statements and comments are forbidden");
      out+=sql[i++];
    }
  }
  return out;
}
const ObColumnSchemaV2 *find_column(const ObTableSchema &table,const std::string &name) {
  ObString view(static_cast<int32_t>(name.size()),name.data());return table.get_column_schema(view);
}
int describe(const ObTableSchema &table,const ObColumnSchemaV2 &column,Definition &out) {
  if(!definition(column,out))return OB_INVALID_DATA;
  try {
    std::set<uint64_t> ids;
    rewrite(out.expression,[&](const std::string &name,const std::string &original,bool function) {
      std::string keyword=name;
      std::transform(keyword.begin(),keyword.end(),keyword.begin(),[](unsigned char c){return std::toupper(c);});
      static const std::set<std::string> functions={"AI_COMPLETE","AI_EMBED","AI_PROMPT","CONCAT","CONCAT_WS",
          "CAST","COALESCE","IFNULL","JSON_OBJECT","JSON_ARRAY","LOWER","UPPER","SUBSTR","SUBSTRING","TRIM","REPLACE"};
      if((function&&functions.count(keyword)==0)||keyword=="SELECT"||keyword=="FROM"||keyword=="UNION")
        throw std::runtime_error("AI columns require a scalar expression over this row");
      if(!function){auto c=find_column(table,name);if(c){
        if(is_ai(*c)||is_internal(*c))throw std::runtime_error("AI columns cannot depend on AI columns");
        ids.insert(c->get_column_id());
      }}return original;
    });
    out.dependencies.assign(ids.begin(),ids.end());
  }catch(...){return OB_NOT_SUPPORTED;}
  return OB_SUCCESS;
}
int execute(ObTableModifyOp &op,const std::string &sql) {
  int ret=op.open_inner_conn();bool nested=false;
  if(OB_SUCC(ret)){ret=op.begin_nested_session(false);nested=OB_SUCC(ret);}
  const bool prior=in_nested_statement;
  if(OB_SUCC(ret)){in_nested_statement=true;ret=op.execute_write(sql.c_str());}
  if(nested){int end=op.end_nested_session();if(OB_SUCC(ret))ret=end;}
  in_nested_statement=prior;
  return ret;
}
int row_value(const ObDMLBaseCtDef &def,ObTableModifyOp &op,uint64_t id,bool old,std::string &sql,ObDatum *&datum) {
  int ret=OB_ENTRY_NOT_EXIST;const auto &row=old?def.old_row_:def.new_row_;
  for(int64_t i=0;i<def.column_ids_.count();++i)if(def.column_ids_.at(i)==id&&i<row.count()){
    ObExpr *expr=row.at(i);ret=expr->eval(op.get_eval_ctx(),datum);
    if(OB_SUCC(ret)){
      if(datum->is_null())sql="NULL";
      else if(ob_is_string_or_lob_type(expr->datum_meta_.type_)) {
        ObString text;
        ret=ObTextStringHelper::read_real_string_data(op.get_exec_ctx(),op.get_exec_ctx().get_allocator(),*datum,
              expr->datum_meta_,expr->obj_meta_.has_lob_header(),text);
        if(OB_SUCC(ret))sql=literal(string(text));
      }else{
        ObObj object;ret=datum->to_obj(object,expr->obj_meta_,expr->obj_datum_map_);
        char buffer[8192];int64_t pos=0;
        if(OB_SUCC(ret))ret=object.print_sql_literal(buffer,sizeof(buffer),pos);
        if(OB_SUCC(ret))sql.assign(buffer,pos);
      }
    }break;
  }return ret;
}
int set_int(const ObDMLBaseCtDef &def,ObTableModifyOp &op,uint64_t id,int64_t value,bool null=false) {
  std::string ignored;ObDatum *datum=nullptr;int ret=row_value(def,op,id,false,ignored,datum);
  if(OB_SUCC(ret)){
    if(null)datum->set_null();
    else{
      // Wrapper expressions may borrow the old row's value buffer even when
      // their datum slots differ. Give the new row its own writable bytes.
      char *buffer=static_cast<char*>(op.get_exec_ctx().get_allocator().alloc(sizeof(int64_t)));
      if(!buffer)return OB_ALLOCATE_MEMORY_FAILED;
      datum->set_string(buffer,sizeof(int64_t));datum->set_int(value);
    }
  }return ret;
}
int read_string(sqlclient::ObMySQLResult &result,const char *name,std::string &out) {
  ObString value;int ret=result.get_varchar(name,value);if(OB_SUCC(ret))out=string(value);return ret;
}
}
bool publishing(){return worker_publish;}
bool model_request(){return in_model_request;}
bool nested_statement(){return in_nested_statement;}
bool internal_read(){return reading_internal;}
bool strict_mode(const ObSQLSessionInfo &session){ObObj value;return session.get_sys_variable(share::SYS_VAR_AI_COLUMN_STRICT_READ,value)==OB_SUCCESS&&value.get_int()!=0;}
bool retry_resolution(){return resolving_retry;}
bool set_retry_resolution(bool value){const bool prior=resolving_retry;resolving_retry=value;return prior;}
int retry_sql(const ObTableSchema &table,const ObString &database,const ObString &column,
              const ObString &predicate,ObSqlString &query){
  const auto *target=find_column(table,string(column));
  if(!target||!is_ai(*target)){LOG_USER_ERROR(OB_NOT_SUPPORTED,"retrying a column that is not an AI column");return OB_NOT_SUPPORTED;}
  const auto state=identifier(hidden("state",target->get_column_id()));
  std::string filter=string(predicate);
  const auto start=filter.find_first_not_of(" \t\r\n");
  if(start!=std::string::npos&&filter.size()>=start+5){std::string word=filter.substr(start,5);for(char &c:word)c=std::toupper(static_cast<unsigned char>(c));if(word=="WHERE")filter.erase(0,start+5);}
  try{filter=rewrite(filter,[&](const std::string &name,const std::string &original,bool function){
    std::string key=name;for(char &c:key)c=std::toupper(static_cast<unsigned char>(c));
    if(!function&&key=="STATUS"&&original[0]!='`')return std::string("(CASE ")+state+" WHEN 1 THEN 'READY' WHEN 2 THEN 'FAILED' ELSE 'PENDING' END)";
    return original;
  });}catch(...){return OB_INVALID_ARGUMENT;}
  std::string sql="UPDATE "+identifier(string(database))+"."+identifier(string(table.get_table_name_str()))+" SET "+state+"=-1-ABS("+state+")";
  if(filter.find_first_not_of(" \t\r\n")!=std::string::npos)sql+=" WHERE "+filter;
  return query.assign(sql.c_str());
}
ObString system_prompt(){return ObString(static_cast<int32_t>(requested_system_prompt.size()),requested_system_prompt.data());}

int define_column(ObColumnSchemaV2 &column,const ObString &expression,int64_t retries) {
  if(retries<0||retries>100)return OB_INVALID_ARGUMENT;
  std::string source=string(expression),upper=source;
  const auto first=upper.find_first_not_of(" \t\r\n");
  if(first==std::string::npos)return OB_INVALID_ARGUMENT;
  source.erase(0,first);upper=source;
  std::transform(upper.begin(),upper.end(),upper.begin(),[](unsigned char c){return std::toupper(c);});
  const auto paren=upper.find('(');
  if(paren==std::string::npos)return OB_NOT_SUPPORTED;
  auto function=upper.substr(0,paren);function.erase(function.find_last_not_of(" \t\r\n")+1);
  if(function!="AI_COMPLETE"&&function!="AI_EMBED")return OB_NOT_SUPPORTED;
  rapidjson::StringBuffer buffer;rapidjson::Writer<rapidjson::StringBuffer> writer(buffer);
  writer.StartObject();writer.Key("expression");writer.String(source.data(),source.size());
  writer.Key("retries");writer.Int64(retries);writer.EndObject();
  std::string comment=std::string(marker)+std::string(buffer.GetString(),buffer.GetSize());
  ObString view(static_cast<int32_t>(comment.size()),comment.data());
  column.add_column_flag(COLUMN_FLAG);column.set_nullable(true);return column.set_comment(view);
}

int prepare_table(ObTableSchema &table) {
  int ret=OB_SUCCESS;std::vector<uint64_t> columns;
  for(auto it=table.column_begin();it!=table.column_end();++it)if(is_ai(**it))columns.push_back((*it)->get_column_id());
  for(uint64_t id:columns){
    const auto *column=table.get_column_schema(id);Definition def;
    if(OB_FAIL(describe(table,*column,def)))return ret;
    for(const char *kind:{"state","request","result_spec"}){
      std::string name=hidden(kind,id);if(find_column(table,name))continue;
      ObColumnSchemaV2 metadata;metadata.set_table_id(table.get_table_id());
      metadata.set_column_id(table.get_max_used_column_id()+1);table.set_max_used_column_id(metadata.get_column_id());
      metadata.set_data_type(ObIntType);metadata.set_accuracy(ObAccuracy::DDL_DEFAULT_ACCURACY[ObIntType]);
      metadata.set_is_hidden(true);metadata.add_column_flag(INTERNAL_FLAG);metadata.set_nullable(true);
      ObObj zero;zero.set_int(0);metadata.set_cur_default_value(zero,false);metadata.set_orig_default_value(zero);
      ObString view(static_cast<int32_t>(name.size()),name.data());
      if(OB_FAIL(metadata.set_column_name(view))||OB_FAIL(table.add_column(metadata)))return ret;
    }
  }return ret;
}

int prepare_alter(const ObTableSchema &original,AlterTableSchema &changes) {
  int ret=OB_SUCCESS;bool has_ai=false;
  for(auto it=changes.column_begin();it!=changes.column_end();++it)if(is_ai(**it))has_ai=true;
  if(!has_ai)return ret;
  ObTableSchema merged;if(OB_FAIL(merged.assign(original)))return ret;
  uint64_t next=original.get_max_used_column_id();
  std::set<std::string> existing;
  for(auto it=original.column_begin();it!=original.column_end();++it)existing.insert(string((*it)->get_column_name_str()));
  for(auto it=changes.column_begin();it!=changes.column_end();++it){
    auto *column=static_cast<AlterColumnSchema *>(*it);
    if(column->alter_type_==OB_DDL_ADD_COLUMN){
      column->set_table_id(original.get_table_id());column->set_column_id(++next);
      if(OB_FAIL(merged.add_column(*column)))return ret;
    }else if(is_ai(*column))return OB_NOT_SUPPORTED;
  }
  merged.set_max_used_column_id(next);
  if(OB_FAIL(prepare_table(merged)))return ret;
  for(auto it=merged.column_begin();it!=merged.column_end();++it){
    if(!is_internal(**it)||existing.count(string((*it)->get_column_name_str())))continue;
    AlterColumnSchema column;if(OB_FAIL(column.ObColumnSchemaV2::assign(**it)))return ret;column.alter_type_=OB_DDL_ADD_COLUMN;
    if(OB_FAIL(changes.add_column(column)))return ret;
    // add_column copies the base schema through a virtual assign operation;
    // restore the ALTER action on its owned derived object after that copy.
    auto *added=static_cast<AlterColumnSchema *>(changes.get_column_schema(column.get_column_id()));
    if(!added)return OB_SCHEMA_ERROR;
    added->alter_type_=OB_DDL_ADD_COLUMN;
  }
  return ret;
}

int persist_metadata(const ObTableSchema &table,ObISQLClient &client) {
  int ret=OB_SUCCESS;int64_t affected=0;
  for(auto it=table.column_begin();OB_SUCC(ret)&&it!=table.column_end();++it){
    const auto &c=**it;if(!is_ai(c))continue;Definition def;
    if(OB_FAIL(describe(table,c,def)))break;
    const auto *state=find_column(table,hidden("state",c.get_column_id()));
    const auto *request=find_column(table,hidden("request",c.get_column_id()));
    const auto *spec=find_column(table,hidden("result_spec",c.get_column_id()));
    if(!state||!request||!spec)return OB_SCHEMA_ERROR;
    std::string normalized=def.expression;
    if(c.get_data_type()==ObJsonType)normalized+=" /* Return exactly one valid JSON value */";
    else if(ob_is_integer_type(c.get_data_type()))normalized+=" /* Return exactly one base-10 integer */";
    std::string sql="REPLACE INTO oceanbase.__all_ai_column(table_id,column_id,column_name,normalized_expr,max_retries,state_column_id,request_column_id,result_spec_column_id,definition_version) VALUES ("
      +std::to_string(table.get_table_id())+","+std::to_string(c.get_column_id())+","+literal(string(c.get_column_name_str()))+","+literal(normalized)+","+std::to_string(def.retries)+","+std::to_string(state->get_column_id())+","+std::to_string(request->get_column_id())+","+std::to_string(spec->get_column_id())+","+std::to_string(table.get_schema_version())+")";
    ret=client.write(sql.c_str(),affected);
  }return ret;
}

int on_row(const ObDMLBaseCtDef &def,ObTableModifyOp &op,bool deleting) {
  int ret=OB_SUCCESS;if(worker_publish||!def.is_primary_index_)return ret;
  auto *guard=op.get_exec_ctx().get_sql_ctx()->schema_guard_;
  const ObTableSchema *table=nullptr;
  if(!guard)return OB_ERR_UNEXPECTED;
  if(OB_FAIL(guard->get_table_schema(def.das_base_ctdef_.index_tid_,table))||!table)return ret;
  std::vector<const ObColumnSchemaV2*> columns;
  for(auto it=table->column_begin();it!=table->column_end();++it)if(is_ai(**it))columns.push_back(*it);
  if(columns.empty())return ret;
  const ObDatabaseSchema *database=nullptr;
  if(OB_FAIL(guard->get_database_schema(table->get_database_id(),database))||!database)return OB_SCHEMA_ERROR;
  std::string predicate;
  for(int64_t i=0;OB_SUCC(ret)&&i<table->get_rowkey_info().get_size();++i){
    uint64_t id=0;table->get_rowkey_info().get_column_id(i,id);std::string value;ObDatum *datum=nullptr;
    if(OB_FAIL(row_value(def,op,id,deleting,value,datum)))break;
    if(i)predicate+=" AND ";predicate+=identifier(string(table->get_column_schema(id)->get_column_name_str()))+" = "+value;
  }
  if(OB_FAIL(ret)||predicate.empty())return OB_NOT_SUPPORTED;
  const bool inserting=def.old_row_.empty();
  std::string old_predicate;
  if(!inserting&&!deleting){
    for(int64_t i=0;OB_SUCC(ret)&&i<table->get_rowkey_info().get_size();++i){
      uint64_t id=0;table->get_rowkey_info().get_column_id(i,id);std::string value;ObDatum *datum=nullptr;
      if(OB_FAIL(row_value(def,op,id,true,value,datum)))break;
      if(i)old_predicate+=" AND ";old_predicate+=identifier(string(table->get_column_schema(id)->get_column_name_str()))+" = "+value;
    }
    if(OB_FAIL(ret))return ret;
  }
  const uint64_t request=static_cast<uint64_t>(ObTimeUtility::current_time())*1024+(request_sequence.fetch_add(1)%1024);
  const std::string qualified=identifier(string(database->get_database_name_str()))+"."+identifier(string(table->get_table_name_str()));
  for(const auto *column:columns){
    Definition config;if(OB_FAIL(describe(*table,*column,config)))break;
    bool changed=inserting||deleting;
    if(!changed){std::string value;ObDatum *state=nullptr;const auto *c=find_column(*table,hidden("state",column->get_column_id()));
      if(!c)return OB_SCHEMA_ERROR;
      if(OB_FAIL(row_value(def,op,c->get_column_id(),false,value,state)))return ret;
      changed=!state->is_null()&&state->get_int()<0;
    }
    std::vector<uint64_t> dependencies=config.dependencies;
    for(int64_t i=0;i<table->get_rowkey_info().get_size();++i){uint64_t id=0;table->get_rowkey_info().get_column_id(i,id);dependencies.push_back(id);}
    for(uint64_t id:dependencies)if(!changed){
      std::string old_value,new_value;ObDatum *a=nullptr,*b=nullptr;
      if(OB_FAIL(row_value(def,op,id,true,old_value,a))||OB_FAIL(row_value(def,op,id,false,new_value,b)))break;
      changed=old_value!=new_value;
    }
    if(OB_FAIL(ret)||!changed)continue;
    const auto id=column->get_column_id();
    std::string remove="DELETE FROM oceanbase.__all_ai_column_task WHERE table_id="+std::to_string(table->get_table_id())+" AND column_id="+std::to_string(id)+" AND rowkey_predicate="+literal(predicate);
    if(!old_predicate.empty()&&old_predicate!=predicate)
      remove="DELETE FROM oceanbase.__all_ai_column_task WHERE table_id="+std::to_string(table->get_table_id())+" AND column_id="+std::to_string(id)+" AND rowkey_predicate IN("+literal(predicate)+","+literal(old_predicate)+")";
    if(OB_FAIL(execute(op,remove)))break;
    if(deleting)continue;
    try {
      const std::string expression=rewrite(config.expression,[&](const std::string &name,const std::string &original,bool function){
        const auto *c=function?nullptr:find_column(*table,name);if(!c)return original;
        std::string value;ObDatum *datum=nullptr;int error=row_value(def,op,c->get_column_id(),false,value,datum);
        if(error!=OB_SUCCESS)throw std::runtime_error("AI column dependency value is unavailable");return value;
      });
      const auto *state=find_column(*table,hidden("state",id));const auto *req=find_column(*table,hidden("request",id));const auto *spec=find_column(*table,hidden("result_spec",id));
      if(!state||!req||!spec){ret=OB_SCHEMA_ERROR;break;}
      if(OB_FAIL(set_int(def,op,id,0,true))||OB_FAIL(set_int(def,op,state->get_column_id(),PENDING))
         ||OB_FAIL(set_int(def,op,req->get_column_id(),request))||OB_FAIL(set_int(def,op,spec->get_column_id(),table->get_schema_version())))break;
      std::string sql="INSERT INTO oceanbase.__all_ai_column_task(table_id,column_id,rowkey_predicate,request_id,definition_version,qualified_table,column_name,expression,state,retry_count,max_retries,eligible_ts,result_type,result_value) VALUES ("
        +std::to_string(table->get_table_id())+","+std::to_string(id)+","+literal(predicate)+","+std::to_string(request)+","+std::to_string(table->get_schema_version())+","+literal(qualified)+","+literal(string(column->get_column_name_str()))+","+literal(expression)+",0,0,"+std::to_string(config.retries)+",now(6),"+std::to_string(column->get_data_type())+",'')";
      ret=execute(op,sql);
    }catch(...){ret=OB_INVALID_ARGUMENT;}
    if(OB_FAIL(ret))break;
  }return ret;
}

int strict_read(ObExecContext &ctx,const ObTableSchema &table,const ObColumnSchemaV2 &column) {
  if(worker_publish||reading_internal||!is_ai(column))return OB_SUCCESS;
  ObObj setting;int ret=ctx.get_my_session()->get_sys_variable(share::SYS_VAR_AI_COLUMN_STRICT_READ,setting);
  if(OB_FAIL(ret)||setting.get_int()==0)return ret;
  const ObDatabaseSchema *database=nullptr;
  if(OB_FAIL(ctx.get_sql_ctx()->schema_guard_->get_database_schema(table.get_database_id(),database))||!database)return OB_SCHEMA_ERROR;
  std::string sql="SELECT COUNT(*) AS incomplete FROM "+identifier(string(database->get_database_name_str()))+"."+identifier(string(table.get_table_name_str()))+" WHERE "+identifier(hidden("state",column.get_column_id()))+" <> 1";
  FlagGuard internal(reading_internal);
  query::ObSessionInnerSql connection(ctx.get_my_session());ObISQLClient::ReadResult result;
  ObSqlString query;query.assign(sql.c_str());
  if(OB_FAIL(connection.execute_read(query,result)))return ret;
  sqlclient::ObMySQLResult *rows=result.get_result();int64_t count=0;
  if(!rows)return OB_ERR_UNEXPECTED;
  if(OB_FAIL(rows->next())||OB_FAIL(rows->get_int("incomplete",count)))return ret;
  if(count>0){
    LOG_USER_ERROR(OB_AI_COLUMN_NOT_READY,table.get_table_id(),column.get_column_id());
    return OB_AI_COLUMN_NOT_READY;
  }
  return OB_SUCCESS;
}

// Each task is durable before it becomes visible to this worker. Model execution
// occurs outside the publication transaction, and a request guard prevents stale writes.
namespace {
struct Task { uint64_t table,column,request,version;int64_t retries,max_retries,state,result_type;std::string predicate,qualified,name,expression,value; };
int fetch_tasks(ObMySQLProxy &proxy,std::vector<Task> &tasks) {
  ObISQLClient::ReadResult result;int ret=proxy.read(result,"SELECT * FROM oceanbase.__all_ai_column_task WHERE (table_id,request_id) IN (SELECT table_id,request_id FROM (SELECT table_id,request_id FROM oceanbase.__all_ai_column_task GROUP BY table_id,request_id HAVING SUM(state IN (0,3,4) AND eligible_ts<=now(6))>0 OR SUM(state IN (0,3,4))=0 ORDER BY MIN(eligible_ts) LIMIT 1) seed) ORDER BY column_id");
  if(OB_FAIL(ret))return ret;auto *rows=result.get_result();if(!rows)return OB_ERR_UNEXPECTED;
  while(OB_SUCC(ret=rows->next())){
    Task task{};int64_t n=0;
    for(auto pair:{std::make_pair("table_id",&task.table),{"column_id",&task.column},{"request_id",&task.request},{"definition_version",&task.version}}){if(OB_FAIL(rows->get_int(pair.first,n)))break;*pair.second=n;}
    if(OB_FAIL(ret))break;
    if(OB_FAIL(rows->get_int("retry_count",task.retries))||OB_FAIL(rows->get_int("max_retries",task.max_retries))||OB_FAIL(rows->get_int("state",task.state))||OB_FAIL(rows->get_int("result_type",task.result_type)))break;
    for(auto pair:{std::make_pair("rowkey_predicate",&task.predicate),{"qualified_table",&task.qualified},{"column_name",&task.name},{"expression",&task.expression}}){if(OB_FAIL(read_string(*rows,pair.first,*pair.second)))break;}
    if(OB_FAIL(ret))break;
    if(task.state==READY||task.state==FAILED)ret=read_string(*rows,"result_value",task.value);
    if(OB_FAIL(ret))break;tasks.push_back(std::move(task));
  }return ret==OB_ITER_END?OB_SUCCESS:ret;
}
std::string key(const Task &t){return "table_id="+std::to_string(t.table)+" AND column_id="+std::to_string(t.column)+" AND rowkey_predicate="+literal(t.predicate)+" AND request_id="+std::to_string(t.request);}
void process_group(ObMySQLProxy &proxy,std::vector<Task> &tasks,size_t begin,size_t end) {
  // DDL may remove the owner while a task is waiting or executing. Discard the
  // orphan rather than repeatedly selecting it ahead of live tables.
  for(size_t i=begin;i<end;++i){
    ObISQLClient::ReadResult owner;
    const auto &t=tasks[i];
    int ret=proxy.read(owner,("SELECT 1 AS present FROM oceanbase.__all_table t JOIN oceanbase.__all_column c ON c.table_id=t.table_id WHERE t.table_id="+std::to_string(t.table)+" AND c.column_id="+std::to_string(t.column)).c_str());
    if(OB_FAIL(ret)||!owner.get_result())return;
    ret=owner.get_result()->next();
    if(ret==OB_ITER_END){int64_t affected=0;proxy.write(("DELETE FROM oceanbase.__all_ai_column_task WHERE "+key(t)).c_str(),affected);return;}
    if(OB_FAIL(ret))return;
  }
  bool terminal=true;
  for(size_t i=begin;i<end;++i){auto &t=tasks[i];
    if(t.state==READY||t.state==FAILED)continue;
    int64_t affected=0;
    int ret=proxy.write(("UPDATE oceanbase.__all_ai_column_task SET state=4,eligible_ts=date_add(now(6),interval 120 second) WHERE "+key(t)+" AND state IN(0,3,4) AND eligible_ts<=now(6)").c_str(),affected);
    if(OB_FAIL(ret)||affected!=1){terminal=false;continue;}
    requested_system_prompt=t.result_type==ObJsonType?"Return exactly one valid JSON value, with no markdown or explanation."
        :ob_is_integer_type(static_cast<ObObjType>(t.result_type))?"Return exactly one base-10 integer, with no markdown or explanation.":"";
    in_model_request=true;
    ObISQLClient::ReadResult result;ret=proxy.read(result,("SELECT CAST("+t.expression+" AS CHAR) AS ai_result").c_str());
    in_model_request=false;requested_system_prompt.clear();
    if(OB_SUCC(ret)){
      auto *row=result.get_result();ObObj value;
      if(!row)ret=OB_ERR_UNEXPECTED;
      else if(OB_FAIL(row->next())){}
      else if(OB_FAIL(row->get_obj("ai_result",value))){}
      else if(value.is_null()){t.value="NULL";}
      else {ObString data;ret=row->get_varchar("ai_result",data);if(OB_SUCC(ret)){
        const auto text=string(data);
        if(t.result_type==ObJsonType){rapidjson::Document json;json.Parse(text.data(),text.size());if(json.HasParseError())ret=OB_ERR_INVALID_JSON_TEXT;}
        else if(ob_is_integer_type(static_cast<ObObjType>(t.result_type))){
          char *end=nullptr;errno=0;std::strtoll(text.c_str(),&end,10);
          if(text.empty()||end!=text.c_str()+text.size()||errno==ERANGE)ret=OB_INVALID_DATA;
        }
        if(OB_SUCC(ret))t.value=literal(text);
      }}
    }
    if(OB_SUCC(ret))t.state=READY;
    else if(t.retries<t.max_retries){
      terminal=false;proxy.write(("UPDATE oceanbase.__all_ai_column_task SET state=3,retry_count=retry_count+1,eligible_ts=date_add(now(6),interval 1 second) WHERE "+key(t)).c_str(),affected);
    }else {t.state=FAILED;t.value="NULL";}
    if(t.state==READY||t.state==FAILED){
      ret=proxy.write(("UPDATE oceanbase.__all_ai_column_task SET state="+std::to_string(t.state)+",result_value="+literal(t.value)+" WHERE "+key(t)).c_str(),affected);
      if(OB_FAIL(ret))terminal=false;
    }
  }
  if(!terminal)return;
  ObMySQLTransaction transaction;int ret=transaction.start(&proxy);if(OB_FAIL(ret))return;
  worker_publish=true;
  for(size_t i=begin;OB_SUCC(ret)&&i<end;++i){auto &t=tasks[i];int64_t affected=0;
    const std::string update="UPDATE "+t.qualified+" SET "+identifier(t.name)+"="+t.value+","+identifier(hidden("state",t.column))+"="+std::to_string(t.state)
      +" WHERE "+t.predicate+" AND "+identifier(hidden("request",t.column))+"="+std::to_string(t.request)
      +" AND "+identifier(hidden("result_spec",t.column))+"="+std::to_string(t.version);
    ret=transaction.write(update.c_str(),affected);
    if(OB_FAIL(ret)&&REACH_TIME_INTERVAL(5000000)){LOG_ERROR("cannot publish AI column result",K(ret),"table_id",t.table,"column_id",t.column);}
    if(OB_SUCC(ret))ret=transaction.write(("DELETE FROM oceanbase.__all_ai_column_task WHERE "+key(t)).c_str(),affected);
  }
  const int end_ret=transaction.end(OB_SUCC(ret));
  if(OB_SUCCESS!=end_ret&&REACH_TIME_INTERVAL(5000000)){LOG_ERROR("cannot commit AI column result group",K(end_ret));}
  worker_publish=false;
}
}
int start_worker(ObMySQLProxy &proxy) {
  if(!stopping.exchange(false))return OB_SUCCESS;
  try { worker=std::thread([&proxy]{
    while(!stopping.load()){
      try {std::vector<Task> tasks;
        if(fetch_tasks(proxy,tasks)==OB_SUCCESS){
          for(size_t begin=0;begin<tasks.size()&&!stopping.load();){size_t end=begin+1;
            while(end<tasks.size()&&tasks[end].table==tasks[begin].table&&tasks[end].request==tasks[begin].request&&tasks[end].predicate==tasks[begin].predicate)++end;
            process_group(proxy,tasks,begin,end);begin=end;}
        }
      }catch(...){worker_publish=false;in_model_request=false;requested_system_prompt.clear();}
      std::this_thread::sleep_for(std::chrono::milliseconds(100));
    }
  });}catch(...){stopping=true;return OB_ALLOCATE_MEMORY_FAILED;}
  return OB_SUCCESS;
}
void stop_worker(){stopping=true;if(worker.joinable())worker.join();}
}}} // namespace oceanbase::sql::ai_column
