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

#define USING_LOG_PREFIX STORAGE

#include "ob_text_daat_iter.h"

namespace oceanbase
{
namespace storage
{
int ObTextDaaTIter::init(const ObTextDaaTParam &param)
{
  int ret = OB_SUCCESS;
  if (OB_ISNULL(param.base_param_) || OB_ISNULL(param.dim_iters_) || OB_ISNULL(param.allocator_)
      || OB_ISNULL(param.relevance_collector_)) {
    ret = OB_INVALID_ARGUMENT;
  } else if (OB_FAIL(ObSRDaaTIterImpl::init(*param.base_param_, *param.dim_iters_,
                                            *param.allocator_, *param.relevance_collector_))) {
  } else if (nullptr != param.base_param_->relevance_expr_ &&
             OB_FAIL(bm25_param_estimator_.init(param.bm25_param_est_ctx_))) {
  } else {
    mode_flag_ = param.mode_flag_;
    function_lookup_mode_ = param.function_lookup_mode_;
    const ObDatumMeta &id_meta = iter_param_->id_proj_expr_->datum_meta_;
    use_score_free_union_ = sql::ObMatchAgainstMode::NATURAL_LANGUAGE_MODE == mode_flag_
        && !function_lookup_mode_ && nullptr == iter_param_->relevance_expr_
        && !iter_param_->need_filter() && !iter_param_->need_pushdown_topk()
        && !iter_param_->limit_param_->is_valid()
        && dim_iters_->count() > 0 && dim_iters_->count() <= SMALL_UNION_SIZE
        && common::ObVarcharType == id_meta.type_ && common::CS_TYPE_BINARY == id_meta.cs_type_;
  }
  return ret;
}

void ObTextDaaTIter::reset()
{
  use_score_free_union_ = false;
  union_started_ = false;
  pending_union_advances_ = 0;
  MEMSET(union_heads_, 0, sizeof(union_heads_));
  MEMSET(union_blocks_, 0, sizeof(union_blocks_));
  MEMSET(union_block_counts_, 0, sizeof(union_block_counts_));
  bm25_param_estimator_.reset();
  // this class can know the type of dim_iters_, but ObSRDaaTIterImpl maybe not
  if (OB_NOT_NULL(dim_iters_)) {
    for (int64_t i = 0; i < dim_iters_->count(); ++i) {
      static_cast<ObTextRetrievalDaaTTokenIter *>(dim_iters_->at(i))->reset();
    }
  }
  ObSRDaaTIterImpl::reset();
}

void ObTextDaaTIter::reuse(const bool switch_tablet)
{
  union_started_ = false;
  pending_union_advances_ = 0;
  MEMSET(union_heads_, 0, sizeof(union_heads_));
  MEMSET(union_blocks_, 0, sizeof(union_blocks_));
  MEMSET(union_block_counts_, 0, sizeof(union_block_counts_));
  if ((OB_NOT_NULL(dim_iters_) && 0 == dim_iters_->count())) {
    // do nothing
  } else if (nullptr != iter_param_ && nullptr != iter_param_->relevance_expr_) {
    bm25_param_estimator_.reuse(switch_tablet);
  }
  if (OB_NOT_NULL(dim_iters_)) {
    for (int64_t i = 0; i < dim_iters_->count(); ++i) {
      static_cast<ObTextRetrievalDaaTTokenIter *>(dim_iters_->at(i))->reuse();
    }
  }
  ObSRDaaTIterImpl::reuse(switch_tablet);
}

int ObTextDaaTIter::get_next_rows(const int64_t capacity, int64_t &count)
{
  return use_score_free_union_ ? get_next_union_rows(capacity, count)
                              : ObSRDaaTIterImpl::get_next_rows(capacity, count);
}

OB_INLINE int ObTextDaaTIter::advance_union_head(const int64_t index)
{
  int ret = OB_SUCCESS;
  union_heads_[index] = nullptr;
  if (union_block_counts_[index] == 0) {
    auto *iter = static_cast<ObTextRetrievalDaaTTokenIter *>(dim_iters_->at(index));
    if (OB_ISNULL(iter)) {
      ret = OB_ERR_UNEXPECTED;
    } else if (OB_FAIL(iter->get_next_docid_block(union_blocks_[index], union_block_counts_[index]))) {
      if (OB_ITER_END == ret) {
        ret = OB_SUCCESS;
      }
    }
  }
  if (OB_SUCC(ret) && union_block_counts_[index] > 0) {
    union_heads_[index] = &union_blocks_[index]->get_datum();
    ++union_blocks_[index];
    --union_block_counts_[index];
    if (union_heads_[index]->is_null()) {
      ret = OB_ERR_UNEXPECTED;
    } else if (union_heads_[index]->len_ == 16) {
      // memcpy permits unaligned IDs without aliasing or alignment UB.
      // Convert each complete word instead of constructing it byte by byte.
      MEMCPY(union_keys_[index], union_heads_[index]->ptr_, 16);
#if __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
      union_keys_[index][0] = __builtin_bswap64(union_keys_[index][0]);
      union_keys_[index][1] = __builtin_bswap64(union_keys_[index][1]);
#endif
    }
  }
  return ret;
}

template <int64_t dimension_count>
int ObTextDaaTIter::get_next_union_rows_impl(const int64_t capacity, int64_t &count)
{
  int ret = OB_SUCCESS;
  count = 0;
  if (IS_NOT_INIT) {
    ret = OB_NOT_INIT;
  } else if (capacity <= 0) {
    ret = capacity == 0 ? OB_SUCCESS : OB_INVALID_ARGUMENT;
  } else {
    constexpr int64_t dim_count = dimension_count;
    if (!union_started_) {
      for (int64_t i = 0; OB_SUCC(ret) && i < dim_count; ++i) {
        ret = advance_union_head(i);
      }
      union_started_ = OB_SUCC(ret);
    }
    const uint64_t pending = pending_union_advances_;
    pending_union_advances_ = 0;
    for (int64_t i = 0; OB_SUCC(ret) && i < dim_count; ++i) {
      if (pending & (1ULL << i)) ret = advance_union_head(i);
    }
    const int64_t real_capacity = MIN(capacity, iter_param_->max_batch_size_);
    ObEvalCtx &ctx = *iter_param_->eval_ctx_;
    ObExpr *id_expr = iter_param_->id_proj_expr_;
    ObExpr *score_expr = iter_param_->relevance_proj_expr_;
    const bool direct_projection = ctx.is_vectorized() && id_expr->is_batch_result()
        && (nullptr == score_expr || score_expr->is_batch_result());
    ObDatum *projected_ids = direct_projection ? id_expr->locate_batch_datums(ctx) : nullptr;
    ObDocIdExt *buffered_ids = &buffered_domain_ids_.at(0);
    double *buffered_scores = &buffered_relevances_.at(0);
    while (OB_SUCC(ret) && count < real_capacity) {
      int64_t minimum = -1;
      uint64_t matching = 0;
      for (int64_t i = 0; i < dim_count; ++i) {
        if (nullptr == union_heads_[i]) {
        } else if (minimum < 0) {
          minimum = i;
          matching = 1ULL << i;
        } else {
          const ObString left = union_heads_[i]->get_string();
          const ObString right = union_heads_[minimum]->get_string();
          int cmp = 0;
          if (left.length() == 16 && right.length() == 16) {
            const uint64_t *lkey = union_keys_[i];
            const uint64_t *rkey = union_keys_[minimum];
            if (lkey[0] != rkey[0]) cmp = lkey[0] < rkey[0] ? -1 : 1;
            else if (lkey[1] != rkey[1]) cmp = lkey[1] < rkey[1] ? -1 : 1;
          } else {
            cmp = MEMCMP(left.ptr(), right.ptr(), MIN(left.length(), right.length()));
            if (0 == cmp) cmp = left.length() - right.length();
          }
          if (cmp < 0) {
            minimum = i;
            matching = 1ULL << i;
          } else if (0 == cmp) {
            matching |= 1ULL << i;
          }
        }
      }
      if (minimum < 0) {
        ret = OB_ITER_END;
      } else {
        const ObDatum &id = *union_heads_[minimum];
        if (direct_projection) {
          projected_ids[count].set_datum(id);
        } else if (id.len_ == 16) {
          buffered_ids[count].assign_binary16(id);
        } else if (OB_FAIL(buffered_ids[count].from_datum(id))) {
          break;
        }
        if (!direct_projection) buffered_scores[count] = 1.0;
        ++count;
        ++input_row_cnt_;
        ++output_row_cnt_;
        for (int64_t i = 0; OB_SUCC(ret) && i < dim_count; ++i) {
          if (matching & (1ULL << i)) {
            if (direct_projection && union_block_counts_[i] == 0) {
              pending_union_advances_ |= 1ULL << i;
            } else {
              ret = advance_union_head(i);
            }
          }
        }
        if (pending_union_advances_ != 0) break;
      }
    }
    if ((OB_SUCC(ret) || OB_ITER_END == ret) && count > 0) {
      // As in the generic path, return a nonempty final batch successfully;
      // the following call reports OB_ITER_END with zero rows.
      if (direct_projection) {
        id_expr->get_evaluated_flags(ctx).set_all(count);
        id_expr->set_evaluated_projected(ctx);
        if (nullptr != score_expr) {
          ObDatum *scores = score_expr->locate_datums_for_update(ctx, count);
          for (int64_t i = 0; i < count; ++i) scores[i].set_double(1.0);
          score_expr->get_evaluated_flags(ctx).set_all(count);
          score_expr->set_evaluated_projected(ctx);
        }
        ret = OB_SUCCESS;
      } else {
        ret = project_union_results(count);
      }
    }
  }
  return ret;
}

int ObTextDaaTIter::get_next_union_rows(const int64_t capacity, int64_t &count)
{
  count = 0;
  if (IS_NOT_INIT) {
    return OB_NOT_INIT;
  }
  switch (dim_iters_->count()) {
    case 1: return get_next_union_rows_impl<1>(capacity, count);
    case 2: return get_next_union_rows_impl<2>(capacity, count);
    case 3: return get_next_union_rows_impl<3>(capacity, count);
    default: return OB_ERR_UNEXPECTED;
  }
}

int ObTextDaaTIter::project_union_results(const int64_t count)
{
  ObEvalCtx &ctx = *iter_param_->eval_ctx_;
  ObExpr *id_expr = iter_param_->id_proj_expr_;
  ObExpr *score_expr = iter_param_->relevance_proj_expr_;
  if (!ctx.is_vectorized() || !id_expr->is_batch_result() ||
      (nullptr != score_expr && !score_expr->is_batch_result())) {
    return project_results(count);
  }
  ObDatum *ids = id_expr->locate_batch_datums(ctx);
  const ObDocIdExt *buffered = &buffered_domain_ids_.at(0);
  for (int64_t i = 0; i < count; ++i) ids[i].set_datum(buffered[i].get_datum());
  id_expr->get_evaluated_flags(ctx).set_all(count);
  id_expr->set_evaluated_projected(ctx);
  if (nullptr != score_expr) {
    // Numeric setters write through datum.ptr_, which must be restored to
    // each expression's result buffer before writing a new batch.
    ObDatum *scores = score_expr->locate_datums_for_update(ctx, count);
    for (int64_t i = 0; i < count; ++i) scores[i].set_double(1.0);
    score_expr->get_evaluated_flags(ctx).set_all(count);
    score_expr->set_evaluated_projected(ctx);
  }
  return OB_SUCCESS;
}

int ObTextDaaTIter::pre_process()
{
  int ret = OB_SUCCESS;
  if (dim_iters_->count() == 0) {
    ret = OB_ITER_END;
  } else if (nullptr != iter_param_->relevance_expr_) {
    if (OB_FAIL(bm25_param_estimator_.do_estimation(*iter_param_->eval_ctx_))) {
    }
  }
  return ret;
}

ObTextBMWIter::ObTextBMWIter()
  : ObSRBMWIterImpl(),
    bm25_param_estimator_() {}

void ObTextBMWIter::reuse(const bool switch_tablet)
{
  if (nullptr != iter_param_ && nullptr != iter_param_->relevance_expr_) {
    bm25_param_estimator_.reuse(switch_tablet);
  }
  if (OB_NOT_NULL(dim_iters_)) {
    for (int64_t i = 0; i < dim_iters_->count(); ++i) {
      static_cast<ObTextRetrievalBlockMaxIter *>(dim_iters_->at(i))->reuse();
    }
  }
  ObSRBMWIterImpl::reuse(switch_tablet);
}

void ObTextBMWIter::reset()
{
  bm25_param_estimator_.reset();
  if (OB_NOT_NULL(dim_iters_)) {
    for (int64_t i = 0; i < dim_iters_->count(); ++i) {
      static_cast<ObTextRetrievalBlockMaxIter *>(dim_iters_->at(i))->reset();
    }
  }
  ObSRBMWIterImpl::reset();
}

int ObTextBMWIter::init(const ObTextDaaTParam &param)
{
  int ret = OB_SUCCESS;
  if (OB_ISNULL(param.base_param_) || OB_ISNULL(param.dim_iters_) || OB_ISNULL(param.allocator_)
      || OB_ISNULL(param.relevance_collector_)) {
    ret = OB_INVALID_ARGUMENT;
  } else if (OB_FAIL(ObSRBMWIterImpl::init(*param.base_param_, *param.dim_iters_,
                                           *param.allocator_, *param.relevance_collector_))) {
  } else if (nullptr != param.base_param_->relevance_expr_ &&
             OB_FAIL(bm25_param_estimator_.init(param.bm25_param_est_ctx_))) {
  }
  return ret;
}

int ObTextBMWIter::get_next_rows(const int64_t capacity, int64_t &count)
{
  int ret = OB_SUCCESS;
  if (IS_NOT_INIT) {
    ret = OB_NOT_INIT;
  } else if (dim_iters_->count() == 0) {
    ret = OB_ITER_END;
  } else if (OB_FAIL(bm25_param_estimator_.do_estimation(*iter_param_->eval_ctx_))) {
  }

  if (FAILEDx(ObSRBMWIterImpl::get_next_rows(capacity, count))) {
    if (OB_UNLIKELY(OB_ITER_END != ret)) {
    }
  }
  return ret;
}

int ObTextBMWIter::init_before_wand_process()
{
  int ret = OB_SUCCESS;
  if (OB_UNLIKELY(!bm25_param_estimator_.is_estimated())) {
    ret = OB_ERR_UNEXPECTED;
  }

  for (int64_t i = 0; OB_SUCC(ret) && i < dim_iters_->count(); ++i) {
    ObTextRetrievalBlockMaxIter *block_max_iter = static_cast<ObTextRetrievalBlockMaxIter *>(dim_iters_->at(i));
    if (OB_FAIL(block_max_iter->init_block_max_iter(
        bm25_param_estimator_.get_total_doc_cnt(), bm25_param_estimator_.get_avg_doc_token_cnt()))) {
    }
  }
  return ret;
}

} // namespace storage
} // namespace oceanbase
