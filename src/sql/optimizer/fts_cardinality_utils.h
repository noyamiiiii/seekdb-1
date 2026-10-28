/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once

#include "sql/resolver/dml/ob_select_stmt.h"

namespace oceanbase
{
namespace sql
{
// A direct COUNT(*) consumer observes the number of rows in this view, never
// which matching IDs a LIMIT selects. Keep the guard deliberately narrow:
// explicit ordering, score-valued expressions and additional filters retain
// the regular relevance/ranking path.
inline bool is_fts_cardinality_only_view(const ObDMLStmt *stmt, const ObDMLStmt *parent)
{
  bool only_count = false;
  if (nullptr != stmt && nullptr != parent && stmt != parent &&
      stmt->is_select_stmt() && parent->is_select_stmt()) {
    const ObSelectStmt *view = static_cast<const ObSelectStmt *>(stmt);
    const ObSelectStmt *consumer = static_cast<const ObSelectStmt *>(parent);
    if (consumer->is_scala_group_by() && consumer->get_aggr_item_size() == 1 &&
        !consumer->has_window_function() && !consumer->has_order_by() &&
        !consumer->has_for_update() && consumer->get_condition_size() == 0 &&
        consumer->get_having_expr_size() == 0 && consumer->get_table_items().count() == 1 &&
        nullptr != consumer->get_table_item(0) && consumer->get_table_item(0)->is_generated_table() &&
        consumer->get_table_item(0)->ref_query_ == view &&
        nullptr != consumer->get_aggr_item(0) &&
        consumer->get_aggr_item(0)->get_expr_type() == T_FUN_COUNT &&
        consumer->get_aggr_item(0)->get_real_param_count() == 0 &&
        !view->is_set_stmt() && !view->is_fetch_with_ties() &&
        !view->has_for_update() && !view->is_calc_found_rows() &&
        !view->has_order_by() && !view->has_group_by() && view->get_having_expr_size() == 0 &&
        !view->has_distinct() && !view->has_window_function() &&
        view->get_table_items().count() == 1 && nullptr != view->get_table_item(0) &&
        view->get_table_item(0)->is_basic_table() && view->get_match_exprs().count() == 1 &&
        view->get_condition_size() == 1 && view->get_select_item_size() > 0) {
      const ObMatchFunRawExpr *match = view->get_match_exprs().at(0);
      const ObRawExpr *condition = view->get_condition_exprs().at(0);
      if (nullptr != condition && condition->get_expr_type() == T_OP_BOOL &&
          condition->get_param_count() == 1) {
        condition = condition->get_param_expr(0);
      }
      only_count = nullptr != match && !match->is_es_match() && condition == match &&
          match->get_mode_flag() == ObMatchAgainstMode::NATURAL_LANGUAGE_MODE &&
          match->get_columns_boosts().empty();
      for (int64_t i = 0; only_count && i < consumer->get_select_item_size(); ++i) {
        const ObRawExpr *output = consumer->get_select_item(i).expr_;
        only_count = nullptr != output &&
            (output == consumer->get_aggr_item(0) || output->is_const_raw_expr());
      }
      for (int64_t i = 0; only_count && i < view->get_select_item_size(); ++i) {
        const ObRawExpr *output = view->get_select_item(i).expr_;
        only_count = nullptr != output &&
            (output->is_column_ref_expr() || output->is_const_raw_expr());
      }
    }
  }
  return only_count;
}
} // namespace sql
} // namespace oceanbase
