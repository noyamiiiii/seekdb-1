/*
 * Copyright (c) 2025 OceanBase.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#define USING_LOG_PREFIX SQL_RESV
#include "sql/resolver/ddl/ob_explain_resolver.h"
#include "sql/resolver/ob_resolver.h"
#include "sql/parser/ob_parser.h"
#include "sql/resolver/dml/ob_select_stmt.h"
#ifndef _WIN32
#include "sql/engine/basic/file_scan_payload.h"
#endif
#include <string>

using namespace oceanbase;
using namespace sql;
using namespace common;
const static int64_t EXPLAIN_FORMAT = 0;
const static int64_t EXPLAIN_DISPLAY_OPTION = 1;
const static int64_t EXPLAIN_INTO_TABLE = 2;
const static int64_t EXPLAIN_STMT_ID = 3;
const static int64_t EXPLAIN_CHILD_STMT = 4;
int ObExplainResolver::resolve(const ParseNode &parse_tree)
{
  int ret = OB_SUCCESS;
  if(parse_tree.type_==T_EXPLAIN&&parse_tree.num_child_==5&&parse_tree.children_[0]
      &&parse_tree.children_[0]->type_==T_SHOW_TABLES&&parse_tree.children_[0]->value_==73003){
    const auto *relation=parse_tree.children_[4];
    if(!relation||relation->num_child_!=2||!relation->children_[1])return OB_INVALID_ARGUMENT;
    const auto *name=relation->children_[1];ObString table(name->str_len_,name->str_value_);
    ObString database=session_info_->get_database_name();
    if(relation->children_[0]){name=relation->children_[0];database=ObString(name->str_len_,name->str_value_);}
    const share::schema::ObTableSchema *schema=nullptr;
    if(OB_FAIL(schema_checker_->get_table_schema(database,table,false,schema)))return ret;
    if(!schema)return OB_TABLE_NOT_EXIST;
    ObSqlString sql;sql.append_fmt("SELECT column_name,normalized_expr,max_retries,state_column_id,request_column_id,result_spec_column_id,definition_version FROM oceanbase.__all_ai_column WHERE table_id=%lu ORDER BY column_id",schema->get_table_id());
    ObParser parser(*params_.allocator_,session_info_->get_sql_mode(),session_info_->get_charsets4parser());ParseResult parsed;
    if(OB_FAIL(parser.parse(sql.string(),parsed)))return ret;
    ObResolver resolver(params_);ObStmt *metadata=nullptr;
    if(OB_FAIL(resolver.resolve(ObResolver::IS_NOT_PREPARED_STMT,*parsed.result_tree_->children_[0],metadata)))return ret;
    stmt_=metadata;return ret;
  }
  ObExplainStmt *explain_stmt = NULL;
  if (!(T_EXPLAIN == parse_tree.type_  && 5 == parse_tree.num_child_)) {
    ret = OB_ERR_UNEXPECTED;
    LOG_WARN("invalid EXPLAIN syntax",
             "type", parse_tree.type_,
             "# of child", parse_tree.num_child_);
  } else if (NULL == (explain_stmt = create_stmt<ObExplainStmt>())) {
    ret = OB_SQL_RESOLVER_NO_MEMORY;
    LOG_WARN("failed to create explain stmt");
  } else {
    ParseNode *child_node = parse_tree.children_[EXPLAIN_CHILD_STMT];
    ObStmt *child_stmt = NULL;
    ObExplainDisplayOpt opt;
    opt.with_tree_line_ = true;
    if (OB_ISNULL(child_node)) {
      ret = OB_ERR_UNEXPECTED;
      LOG_WARN("empty parse tree for stmt");
    } else if (!(T_SELECT == child_node->type_
                 || T_DELETE == child_node->type_
                 || T_INSERT == child_node->type_
                 || T_UPDATE == child_node->type_
                 || T_REPLACE == child_node->type_)) {
      ret = OB_NOT_SUPPORTED;
      LOG_WARN("invalid stmt type for EXPLAIN", "type", child_node->type_);
      LOG_USER_ERROR(OB_NOT_SUPPORTED, "stmt type for EXPLAIN");
    } else {
      ObResolver resolver(params_);
      if (OB_FAIL(resolver.resolve(ObResolver::IS_NOT_PREPARED_STMT,
                                   *child_node,
                                   child_stmt))) {
      } else if (OB_ISNULL(child_stmt)) {
        ret = OB_ERR_UNEXPECTED;
        LOG_WARN("failed to generate stmt for child stmt");
      } else {
#ifndef _WIN32
        if(child_stmt->is_select_stmt() && parse_tree.children_[EXPLAIN_FORMAT]
            && parse_tree.children_[EXPLAIN_FORMAT]->type_==T_SHOW_COLUMNS){
          // Resolve the source query normally, then expose its metadata through
          // an ordinary native SELECT of constants. No source rows are read.
          auto *select=static_cast<ObSelectStmt *>(child_stmt);
          std::string query;
          const auto literal=[](const std::string &s){static const char hex[]="0123456789abcdef";std::string out="CONVERT(X'";for(unsigned char c:s){out+=hex[c>>4];out+=hex[c&15];}return out+"' USING utf8mb4)";};
          for(int64_t i=0;i<select->get_select_item_size();++i){
            const auto &item=select->get_select_item(i);
            auto *expression=item.expr_;
            if(!expression)return OB_ERR_UNEXPECTED;
            std::string type=ob_obj_type_str(expression->get_data_type());
            std::transform(type.begin(),type.end(),type.begin(),[](unsigned char c){return std::toupper(c);});
            bool nullable=true;
            if(expression->is_column_ref_expr()){
              const auto *column=static_cast<const ObColumnRefRawExpr *>(expression);
              const auto *table=select->get_table_item_by_id(column->get_table_id());
              if(table&&table->function_table_expr_&&table->function_table_expr_->is_const_raw_expr()){
                const auto &value=static_cast<const ObConstRawExpr*>(table->function_table_expr_)->get_value();
                if(value.is_string_type()){
                  const auto payload=value.get_string();
                  if(file_sql::is_payload(payload.ptr(),payload.length())){
                    rapidjson::Document doc;doc.Parse(payload.ptr()+sizeof(file_sql::PREFIX)-1,payload.length()-sizeof(file_sql::PREFIX)+1);
                    const auto ordinal=column->get_column_id()-OB_APP_MIN_COLUMN_ID;
                    if(doc.HasParseError()||!doc.HasMember("columns")||ordinal>=doc["columns"].Size())return OB_INVALID_DATA;
                    const auto &field=doc["columns"][static_cast<rapidjson::SizeType>(ordinal)];
                    static const char *types[]={"NULL","BIGINT","DOUBLE","BOOLEAN","VARCHAR","DATE","DATETIME"};
                    const auto kind=field[1].GetInt();if(kind<0||kind>=7)return OB_INVALID_DATA;
                    type=types[kind];nullable=field.Size()<3||field[2].GetBool();
                  }
                }
              }
            }
            if(i)query+=" UNION ALL ";
            query+="SELECT "+literal(std::string(item.alias_name_.ptr(),item.alias_name_.length()))+" AS column_name,"+literal(type)+" AS column_type,"+literal(nullable?"YES":"NO")+" AS `null`,NULL AS `key`,NULL AS `default`,NULL AS extra";
          }
          ObParser parser(*params_.allocator_,params_.session_info_->get_sql_mode(),params_.session_info_->get_charsets4parser());
          ParseResult parsed;
          ObString source(static_cast<int32_t>(query.size()),query.data());
          if(OB_FAIL(parser.parse(source,parsed)))return ret;
          ObStmt *metadata=nullptr;ObResolver metadata_resolver(params_);
          if(OB_FAIL(metadata_resolver.resolve(ObResolver::IS_NOT_PREPARED_STMT,*parsed.result_tree_->children_[0],metadata)))return ret;
          stmt_=metadata;return OB_SUCCESS;
        }
#endif
        explain_stmt->set_explain_query_stmt(static_cast<ObDMLStmt *>(child_stmt));
        ParseNode *format = parse_tree.children_[EXPLAIN_FORMAT];
        if (NULL != format) {
          switch (format->type_) {
          case T_OUTLINE:
          {
            explain_stmt->set_explain_format(EXPLAIN_OUTLINE);
            opt.with_tree_line_ = true;
          } break;
          case T_EXTENDED:
          {
            explain_stmt->set_explain_format(EXPLAIN_EXTENDED);
            opt.with_tree_line_ = true;
          } break;
          case T_PARTITIONS:
          {
            explain_stmt->set_explain_format(EXPLAIN_PARTITIONS);
            opt.with_tree_line_ = true;
          } break;
          case T_TRADITIONAL:
          {
            explain_stmt->set_explain_format(EXPLAIN_TRADITIONAL);
            opt.with_tree_line_ = true;
          } break;
          case T_FORMAT_JSON:
          {
            explain_stmt->set_explain_format(EXPLAIN_FORMAT_JSON);
            opt.with_tree_line_ = false;
          } break;
          case T_BASIC: {
            explain_stmt->set_explain_format(EXPLAIN_BASIC);
            opt.with_tree_line_ = true;
            break;
          }
          case T_EXTENDED_NOADDR: {
            explain_stmt->set_explain_format(EXPLAIN_EXTENDED_NOADDR);
            opt.with_tree_line_ = true;
            break;
          }
          case T_PLANREGRESS: {
            explain_stmt->set_explain_format(EXPLAIN_PLANREGRESS);
            opt.with_tree_line_ = true;
            break;
          }
          default:
            ret = OB_ERROR;
            LOG_WARN("unknown explain format", "format", format->type_);
            break;
          }
        }
      }
    }
    ParseNode *opt_node = parse_tree.children_[EXPLAIN_DISPLAY_OPTION];
    if (OB_SUCC(ret) && NULL != opt_node) {
      switch (opt_node->type_) {
      case T_PRETTY_COLOR:
        opt.with_color_ = true;
        // pass
      case T_PRETTY:
        opt.with_tree_line_ = true;
        break;
      default:
        ret = OB_ERR_UNEXPECTED;
      }
    }
    if (OB_SUCC(ret)) {
      explain_stmt->set_display_opt(opt);
    }
    ParseNode *into_table_node = parse_tree.children_[EXPLAIN_INTO_TABLE];
    if (OB_SUCC(ret) && NULL != into_table_node) {
      ObString into_table(into_table_node->str_len_, into_table_node->str_value_);
      explain_stmt->set_into_table(into_table);
    }
    ParseNode *statement_id_node = parse_tree.children_[EXPLAIN_STMT_ID];
    if (OB_SUCC(ret) && NULL != statement_id_node) {
      ObString statement_id(statement_id_node->str_len_, statement_id_node->str_value_);
      explain_stmt->set_statement_id(statement_id);
    }
  }

  return ret;
}
