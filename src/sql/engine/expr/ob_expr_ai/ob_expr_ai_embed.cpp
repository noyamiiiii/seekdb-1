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

#define USING_LOG_PREFIX SQL_ENG
#include "ob_expr_ai_embed.h"
#include "query/ai/ob_ai_endpoint_resolver.h"
#include "share/rc/ob_server_runtime.h"

using namespace oceanbase::common;
using namespace oceanbase::sql;

namespace oceanbase 
{
namespace sql 
{
ObExprAIEmbed::ObExprAIEmbed(common::ObIAllocator &alloc)
    : ObFuncExprOperator(alloc, 
                        T_FUN_SYS_AI_EMBED, 
                        N_AI_EMBED, 
                        MORE_THAN_ZERO,
                        NOT_VALID_FOR_GENERATED_COL, 
                        NOT_ROW_DIMENSION) 
{
}

ObExprAIEmbed::~ObExprAIEmbed() 
{
}

int ObExprAIEmbed::calc_result_typeN(ObExprResType &type,
                                     ObExprResType *types_stack,
                                     int64_t param_num,
                                     common::ObExprTypeCtx &type_ctx) const 
{
  UNUSED(type_ctx);
  UNUSED(types_stack);
  int ret = OB_SUCCESS;
  if (OB_UNLIKELY(param_num > 3 || param_num < 2)) {
    ObString func_name_(get_name());
    ret = OB_ERR_PARAM_SIZE;
    LOG_USER_ERROR(OB_ERR_PARAM_SIZE, func_name_.length(), func_name_.ptr());
  } else {
    types_stack[MODEL_IDX].set_calc_type(ObVarcharType);
    types_stack[MODEL_IDX].set_calc_collation_type(CS_TYPE_UTF8MB4_BIN);
    if (!ob_is_string_or_lob_type(types_stack[CONTENT_IDX].get_type())
        && types_stack[CONTENT_IDX].get_type() != ObNullType) {
      types_stack[CONTENT_IDX].set_calc_type(ObVarcharType);
    }
    types_stack[CONTENT_IDX].set_calc_collation_type(CS_TYPE_UTF8MB4_BIN);
    if (param_num == 3) {
      if (ob_is_integer_type(types_stack[DIM_IDX].get_type())) {
        types_stack[DIM_IDX].set_calc_type(ObIntType);
        types_stack[DIM_IDX].set_precision(10);
        types_stack[DIM_IDX].set_scale(0);
      } else {
        ret = OB_INVALID_ARGUMENT;
        LOG_USER_ERROR(OB_INVALID_ARGUMENT, "ai_embed, dimension parameter must be an integer, not a decimal or float");
      }
    }
    type.set_json();
    type.set_length((ObAccuracy::DDL_DEFAULT_ACCURACY[ObJsonType]).get_length());
    type.set_collation_type(CS_TYPE_UTF8MB4_BIN);
    type.set_collation_level(CS_LEVEL_COERCIBLE);
  }
  return ret;
}

int ObExprAIEmbed::eval_ai_embed(const ObExpr &expr, ObEvalCtx &ctx, ObDatum &res)
{
  int ret = OB_SUCCESS;
  ObDatum *model_arg = nullptr;
  ObDatum *content_arg = nullptr;
  ObDatum *dimension_arg = nullptr;
  if (expr.arg_cnt_ == 3
      ? OB_FAIL(expr.eval_param_value(ctx, model_arg, content_arg, dimension_arg))
      : OB_FAIL(expr.eval_param_value(ctx, model_arg, content_arg))) {
  } else if (model_arg->is_null() || content_arg->is_null()
             || (nullptr != dimension_arg && dimension_arg->is_null())) {
    res.set_null();
  } else {
    ObEvalCtx::TempAllocGuard guard(ctx);
    MultimodeAlloctor allocator(guard.get_allocator());
    ObString model_id = model_arg->get_string();
    ObString content;
    ObJsonObject *config = nullptr;
    ObAIFuncExprInfo *info = nullptr;
    share::ObAiModelEndpointInfo endpoint;
    query::ObIAiEndpointResolver *resolver =
        share::server_service<query::ObIAiEndpointResolver>();
    if (OB_FAIL(ObTextStringHelper::read_real_string_data(ctx.exec_ctx_, allocator, *content_arg,
            expr.args_[CONTENT_IDX]->datum_meta_, expr.args_[CONTENT_IDX]->obj_meta_.has_lob_header(), content))) {
    } else if (nullptr != dimension_arg) {
      ObJsonInt *dimension = nullptr;
      if (dimension_arg->get_int() <= 0) { ret = OB_INVALID_ARGUMENT;
      } else if (OB_FAIL(ObAIFuncJsonUtils::get_json_object(allocator, config))) {
      } else if (OB_FAIL(ObAIFuncJsonUtils::get_json_int(allocator, dimension_arg->get_int(), dimension))) {
      } else if (OB_FAIL(config->add("dimensions", dimension))) {}
    }
    ObString output;
    if (OB_FAIL(ret)) {
    } else if (model_id.empty() || content.empty()) { ret = OB_INVALID_ARGUMENT;
    } else if (OB_FAIL(ObAIFuncUtils::get_ai_func_info(allocator, model_id, info))) {
    } else if (OB_ISNULL(resolver)) { ret = OB_ERR_UNEXPECTED;
    } else if (OB_FAIL(resolver->resolve_by_model_name(model_id, allocator, endpoint))) {
    } else {
      ObAIFuncModel model(allocator, *info, endpoint);
      if (OB_FAIL(model.call_dense_embedding(content, config, output))) {}
    }
    ObIJsonBase *json = nullptr;
    ObString binary;
    if (OB_FAIL(ret)) {
    } else if (OB_FAIL(ObJsonBaseFactory::get_json_base(&allocator, output,
            ObJsonInType::JSON_TREE, ObJsonInType::JSON_TREE, json))) {
    } else if (OB_FAIL(json->get_raw_binary(binary, &allocator))) {
    } else if (OB_FAIL(ObJsonExprHelper::pack_json_str_res(expr, ctx, res, binary))) {}
  }
  return ret;
}

int ObExprAIEmbed::eval_ai_embed_batch(const ObExpr &expr, ObEvalCtx &ctx,
    const ObBitVector &skip, const int64_t size)
{
  int ret = OB_SUCCESS;
  ObEvalCtx::BatchInfoScopeGuard batch_guard(ctx);
  ObEvalCtx::TempAllocGuard temp_guard(ctx);
  MultimodeAlloctor allocator(temp_guard.get_allocator());
  ObBitVector &evaluated = expr.get_evaluated_flags(ctx);
  ObDatumVector output = expr.locate_expr_datumvector(ctx);
  struct Row {
    int64_t slot;
    int64_t dimension;
    ObString model;
    ObString content;
    bool processed;
  };
  ObArray<Row> rows;
  for (int64_t arg = 0; OB_SUCC(ret) && arg < expr.arg_cnt_; ++arg) {
    if (OB_FAIL(expr.args_[arg]->eval_batch(ctx, skip, size))) {}
  }
  for (int64_t i = 0; OB_SUCC(ret) && i < size; ++i) {
    if (skip.at(i) || evaluated.at(i)) { continue; }
    batch_guard.set_batch_idx(i);
    ObDatum *model = expr.args_[MODEL_IDX]->locate_expr_datumvector(ctx).at(i);
    ObDatum *content = expr.args_[CONTENT_IDX]->locate_expr_datumvector(ctx).at(i);
    ObDatum *dimension = expr.arg_cnt_ == 3 ? expr.args_[DIM_IDX]->locate_expr_datumvector(ctx).at(i) : nullptr;
    if (model->is_null() || content->is_null() || (nullptr != dimension && dimension->is_null())) {
      output.at(i)->set_null();
      evaluated.set(i);
    } else {
      Row row{i, nullptr == dimension ? 0 : dimension->get_int(), model->get_string(), ObString(), false};
      if (nullptr != dimension && row.dimension <= 0) { ret = OB_INVALID_ARGUMENT;
      } else if (OB_FAIL(ObTextStringHelper::read_real_string_data(ctx.exec_ctx_, allocator, *content,
              expr.args_[CONTENT_IDX]->datum_meta_, expr.args_[CONTENT_IDX]->obj_meta_.has_lob_header(), row.content))) {
      } else if (row.model.empty() || row.content.empty()) { ret = OB_INVALID_ARGUMENT;
      } else if (OB_FAIL(rows.push_back(row))) {}
    }
  }
  for (int64_t first = 0; OB_SUCC(ret) && first < rows.count(); ++first) {
    if (rows.at(first).processed) { continue; }
    ObArray<ObString> contents;
    ObArray<int64_t> slots;
    for (int64_t i = first; OB_SUCC(ret) && i < rows.count(); ++i) {
      Row &row = rows.at(i);
      if (!row.processed && row.model == rows.at(first).model && row.dimension == rows.at(first).dimension) {
        if (OB_FAIL(contents.push_back(row.content))) {
        } else if (OB_FAIL(slots.push_back(row.slot))) {
        } else { row.processed = true; }
      }
    }
    ObJsonObject *config = nullptr;
    if (OB_SUCC(ret) && rows.at(first).dimension > 0) {
      ObJsonInt *dimension = nullptr;
      if (OB_FAIL(ObAIFuncJsonUtils::get_json_object(allocator, config))) {
      } else if (OB_FAIL(ObAIFuncJsonUtils::get_json_int(allocator, rows.at(first).dimension, dimension))) {
      } else if (OB_FAIL(config->add("dimensions", dimension))) {}
    }
    ObAIFuncExprInfo *info = nullptr;
    share::ObAiModelEndpointInfo endpoint;
    query::ObIAiEndpointResolver *resolver = share::server_service<query::ObIAiEndpointResolver>();
    ObArray<ObString> results;
    if (OB_FAIL(ret)) {
    } else if (OB_ISNULL(resolver)) { ret = OB_ERR_UNEXPECTED;
    } else if (OB_FAIL(ObAIFuncUtils::get_ai_func_info(allocator, rows.at(first).model, info))) {
    } else if (OB_FAIL(resolver->resolve_by_model_name(rows.at(first).model, allocator, endpoint))) {
    } else {
      ObAIFuncModel model(allocator, *info, endpoint);
      if (OB_FAIL(model.call_dense_embedding_vector_v2(contents, config, results))) {}
    }
    if (OB_SUCC(ret) && results.count() != slots.count()) { ret = OB_INVALID_DATA; }
    for (int64_t i = 0; OB_SUCC(ret) && i < results.count(); ++i) {
      batch_guard.set_batch_idx(slots.at(i));
      ObIJsonBase *json = nullptr;
      ObString binary;
      if (OB_FAIL(ObJsonBaseFactory::get_json_base(&allocator, results.at(i),
              ObJsonInType::JSON_TREE, ObJsonInType::JSON_TREE, json))) {
      } else if (OB_FAIL(json->get_raw_binary(binary, &allocator))) {
      } else if (OB_FAIL(ObJsonExprHelper::pack_json_str_res(expr, ctx, *output.at(slots.at(i)), binary))) {
      } else { evaluated.set(slots.at(i)); }
    }
  }
  return ret;
}

int ObExprAIEmbed::cg_expr(ObExprCGCtx &expr_cg_ctx, 
                           const ObRawExpr &raw_expr,
                           ObExpr &rt_expr) const 
{
  int ret = OB_SUCCESS;
  // TODO: support schema version match in plan cache for ai func
  // const ObRawExpr *model_key = raw_expr.get_param_expr(0);
  // if (OB_NOT_NULL(model_key)
  //     && (model_key->is_static_scalar_const_expr() || model_key->is_const_expr())
  //     && model_key->get_expr_type() != T_OP_GET_USER_VAR &&
  //     OB_NOT_NULL(expr_cg_ctx.schema_guard_)) {
  //   ObIAllocator *allocator = expr_cg_ctx.allocator_;
  //   ObExecContext *exec_ctx = expr_cg_ctx.session_->get_cur_exec_ctx();
  //   ObObj const_data;
  //   bool got_data = false;
  //   ObAIFuncExprInfo *info = nullptr;
  //   if (OB_ISNULL(allocator)) {
  //     ret = OB_ERR_UNEXPECTED;
  //     LOG_WARN("allocator is null", K(ret));
  //   } else if (OB_FAIL(ObSQLUtils::calc_const_or_calculable_expr(exec_ctx,
  //                                                         model_key,
  //                                                         const_data,
  //                                                         got_data,
  //                                                         *allocator))) {
  //     LOG_WARN("failed to calc offset expr", K(ret));
  //   } else if (!got_data || const_data.is_null()) {
  //   } else if (OB_FAIL(ObAIFuncUtils::get_ai_func_info(*allocator, const_data.get_string(), *expr_cg_ctx.schema_guard_, info))) {
  //     LOG_WARN("failed to get ai func info", K(ret), K(const_data.get_string()));
  //   } else {
  //     rt_expr.extra_info_ = info;
  //   }
  // }

  if (OB_SUCC(ret)) {
    rt_expr.eval_func_ = ObExprAIEmbed::eval_ai_embed;
    rt_expr.eval_batch_func_ = ObExprAIEmbed::eval_ai_embed_batch;
  }
  return ret;
}

} // namespace sql
} // namespace oceanbase
