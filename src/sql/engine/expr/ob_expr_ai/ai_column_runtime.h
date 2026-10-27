/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once
#include "share/schema/ob_table_schema.h"
#include "share/schema/ob_column_schema.h"
#include "common/mysqlclient/ob_mysql_proxy.h"

namespace oceanbase {
namespace sql {
class ObExecContext;
class ObSQLSessionInfo;
class ObTableModifyOp;
struct ObDMLBaseCtDef;
namespace ai_column {
constexpr int64_t COLUMN_FLAG = INT64_C(1) << 4;
constexpr int64_t INTERNAL_FLAG = INT64_C(1) << 5;
constexpr int AI_COLUMN_NOT_READY = -11120;
enum State { PENDING = 0, READY = 1, FAILED = 2 };
inline bool is_ai(const share::schema::ObColumnSchemaV2 &column)
{ return (column.get_column_flags() & COLUMN_FLAG) != 0; }
inline bool is_internal(const share::schema::ObColumnSchemaV2 &column)
{ return (column.get_column_flags() & INTERNAL_FLAG) != 0; }
int define_column(share::schema::ObColumnSchemaV2 &column,
                  const common::ObString &expression, int64_t retries);
int prepare_table(share::schema::ObTableSchema &table);
int prepare_alter(const share::schema::ObTableSchema &original,share::schema::AlterTableSchema &changes);
int persist_metadata(const share::schema::ObTableSchema &table, common::ObISQLClient &client);
int on_row(const ObDMLBaseCtDef &definition, ObTableModifyOp &op, bool deleting);
int strict_read(ObExecContext &ctx, const share::schema::ObTableSchema &table,
                const share::schema::ObColumnSchemaV2 &column);
int start_worker(common::ObMySQLProxy &proxy);
void stop_worker();
bool publishing();
bool model_request();
bool nested_statement();
bool internal_read();
bool strict_mode(const ObSQLSessionInfo &session);
bool retry_resolution();
bool set_retry_resolution(bool value);
int retry_sql(const share::schema::ObTableSchema &table,const common::ObString &database,
              const common::ObString &column,const common::ObString &predicate,common::ObSqlString &query);
common::ObString system_prompt();
}
}}
