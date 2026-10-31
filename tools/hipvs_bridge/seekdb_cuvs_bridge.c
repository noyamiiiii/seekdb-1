/* seekdb <-> hipVS C ABI. FP32 input and L2 output remain unchanged. */
#include "seekdb_cuvs_bridge.h"
#include <cuvs/core/c_api.h>
#include <cuvs/neighbors/cagra.h>
#if SK_CAGRA_BUILD_ALGO == 3
#include <cuvs/neighbors/brute_force.h>
#endif
#include <dlpack/dlpack.h>
#ifdef __HIP_PLATFORM_AMD__
#include <cuvs/cuda_runtime.h>
#else
#include <cuda_runtime.h>
#endif
#include <pthread.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* Construction knobs. The defaults are the tuned production values; the sweep
   harness overrides them at compile time, so a build without -D keeps the
   configuration that the official evaluation measured. */
#ifndef SK_CAGRA_GRAPH_DEGREE
#define SK_CAGRA_GRAPH_DEGREE 160
#endif
#ifndef SK_CAGRA_INTERMEDIATE_DEGREE
#define SK_CAGRA_INTERMEDIATE_DEGREE 192
#endif
#ifndef SK_CAGRA_NITER
#define SK_CAGRA_NITER 40
#endif
/* 0 auto, 1 IVF-PQ, 2 NN-descent, 3 exact brute-force graph,
   4 CAGRA iterative search.
   NN-descent is the tuned production path; 3 is handled outside CAGRA build. */
#ifndef SK_CAGRA_BUILD_ALGO
#define SK_CAGRA_BUILD_ALGO 2
#endif
/* IVF-PQ knobs, read only when SK_CAGRA_BUILD_ALGO is 1. Zero keeps the
   library heuristic for every value except the PQ bit width. */
#ifndef SK_CAGRA_IVFPQ_LISTS
#define SK_CAGRA_IVFPQ_LISTS 0
#endif
#ifndef SK_CAGRA_IVFPQ_PROBES
#define SK_CAGRA_IVFPQ_PROBES 0
#endif
#ifndef SK_CAGRA_IVFPQ_PQ_BITS
#define SK_CAGRA_IVFPQ_PQ_BITS 8
#endif
#ifndef SK_CAGRA_IVFPQ_PQ_DIM
#define SK_CAGRA_IVFPQ_PQ_DIM 0
#endif
#ifndef SK_CAGRA_IVFPQ_KMEANS_ITERS
#define SK_CAGRA_IVFPQ_KMEANS_ITERS 0
#endif
#ifndef SK_CAGRA_IVFPQ_TRAINSET
#define SK_CAGRA_IVFPQ_TRAINSET 0.0
#endif
/* >1 recomputes exact distances for the top (rate * k) graph candidates,
   trading build time for recall. 1 keeps the library default (off). */
#ifndef SK_CAGRA_IVFPQ_REFINE
#define SK_CAGRA_IVFPQ_REFINE 1.0
#endif

typedef struct {
  cuvsResources_t res;
  cuvsCagraIndexParams_t ip;
  cuvsCagraIndex_t idx;
  cuvsCagraSearchParams_t sp;
#if SK_CAGRA_BUILD_ALGO == 1
  cuvsIvfPqIndexParams_t pq_build;
  cuvsIvfPqSearchParams_t pq_search;
#endif
  size_t default_itopk;
  long n, dim;
  float *queries, *distances;
  unsigned int *neighbors;
  size_t query_bytes, neighbor_bytes, distance_bytes;
  pthread_mutex_t mutex;
} sk_cuvs_index;

static int checked_bytes(long rows, long columns, size_t *bytes)
{
  if (rows <= 0 || columns <= 0 || (size_t)rows > SIZE_MAX / sizeof(float) / (size_t)columns) return 1;
  *bytes = (size_t)rows * (size_t)columns * sizeof(float);
  return 0;
}

static DLManagedTensor tensor(void *data, long *shape, DLDeviceType device, uint8_t code)
{
  DLManagedTensor value = {0};
  value.dl_tensor.data = data;
  value.dl_tensor.device.device_type = device;
  value.dl_tensor.device.device_id = 0;
  value.dl_tensor.ndim = 2;
  value.dl_tensor.dtype.code = code;
  value.dl_tensor.dtype.bits = 32;
  value.dl_tensor.dtype.lanes = 1;
  value.dl_tensor.shape = shape;
  return value;
}

void seekdb_cuvs_free(void *handle)
{
  sk_cuvs_index *h = (sk_cuvs_index *)handle;
  if (!h) return;
  if (h->res) cuvsStreamSync(h->res);
  if (h->queries) cuvsRMMFree(h->res, h->queries, h->query_bytes);
  if (h->neighbors) cuvsRMMFree(h->res, h->neighbors, h->neighbor_bytes);
  if (h->distances) cuvsRMMFree(h->res, h->distances, h->distance_bytes);
  if (h->sp) cuvsCagraSearchParamsDestroy(h->sp);
  if (h->idx) cuvsCagraIndexDestroy(h->idx);
#if SK_CAGRA_BUILD_ALGO == 1
  if (h->pq_build) cuvsIvfPqIndexParamsDestroy(h->pq_build);
  if (h->pq_search) cuvsIvfPqSearchParamsDestroy(h->pq_search);
#endif
  if (h->ip) cuvsCagraIndexParamsDestroy(h->ip);
  if (h->res) cuvsResourcesDestroy(h->res);
  pthread_mutex_destroy(&h->mutex);
  free(h);
}

#if SK_CAGRA_BUILD_ALGO == 3
/* Exact all-neighbours graph via brute force, then adopt it as the CAGRA graph.
   Every row is sorted by distance, so the index can search it directly; the
   graph degree equals SK_CAGRA_GRAPH_DEGREE and no pruning pass runs. */
static int sk_cuvs_build_from_knn(sk_cuvs_index *h, const float *base, long n, long dim)
{
  const size_t k = SK_CAGRA_GRAPH_DEGREE;
  size_t data_bytes = 0, graph_bytes = 0;
  void *d_base = NULL, *d_graph = NULL, *d_dist = NULL;
  cuvsBruteForceIndex_t bf = NULL;
  int rc = 1;
  if (checked_bytes(n, dim, &data_bytes) ||
      (size_t)n > SIZE_MAX / k / sizeof(unsigned)) return 1;
  graph_bytes = (size_t)n * k * sizeof(unsigned);
  if (cuvsRMMAlloc(h->res, &d_base, data_bytes) != CUVS_SUCCESS) goto done;
  if (cudaMemcpy(d_base, base, data_bytes, cudaMemcpyDefault) != cudaSuccess) goto done;
  long dataset_shape[2] = {n, dim}, graph_shape[2] = {n, (long)k};
  DLManagedTensor dataset = tensor(d_base, dataset_shape, kDLCUDA, kDLFloat);
  if (cuvsBruteForceIndexCreate(&bf) != CUVS_SUCCESS) goto done;
  if (cuvsBruteForceBuild(h->res, &dataset, L2Expanded, 0.f, bf) != CUVS_SUCCESS) goto done;
  if (cuvsRMMAlloc(h->res, &d_graph, graph_bytes) != CUVS_SUCCESS) goto done;
  if (cuvsRMMAlloc(h->res, &d_dist, graph_bytes) != CUVS_SUCCESS) goto done;
  DLManagedTensor graph = tensor(d_graph, graph_shape, kDLCUDA, kDLUInt);
  DLManagedTensor dist = tensor(d_dist, graph_shape, kDLCUDA, kDLFloat);
  cuvsFilter filter;
  filter.type = NO_FILTER;
  filter.addr = (uintptr_t)0;
  if (cuvsBruteForceSearch(h->res, bf, &dataset, &graph, &dist, filter) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS ||
      cuvsCagraIndexFromArgs(h->res, L2Expanded, &graph, &dataset, h->idx) != CUVS_SUCCESS) {
    goto done;
  }
  rc = 0;
done:
  if (bf) cuvsBruteForceIndexDestroy(bf);
  if (d_base) cuvsRMMFree(h->res, d_base, data_bytes);
  if (d_graph) cuvsRMMFree(h->res, d_graph, graph_bytes);
  if (d_dist) cuvsRMMFree(h->res, d_dist, graph_bytes);
  return rc;
}
#endif

void *seekdb_cuvs_build(const float *base, long n, long dim)
{
  size_t bytes = 0;
  if (!base || n > UINT_MAX || checked_bytes(n, dim, &bytes)) return NULL;
  sk_cuvs_index *h = (sk_cuvs_index *)calloc(1, sizeof(*h));
  if (!h) return NULL;
  if (pthread_mutex_init(&h->mutex, NULL)) { free(h); return NULL; }
#if SK_CAGRA_BUILD_ALGO != 3
  long shape[2] = {n, dim};
  DLManagedTensor dataset = tensor((void *)base, shape, kDLCPU, kDLFloat);
#endif
  if (cuvsResourcesCreate(&h->res) != CUVS_SUCCESS ||
      cuvsCagraIndexParamsCreate(&h->ip) != CUVS_SUCCESS ||
      cuvsCagraIndexCreate(&h->idx) != CUVS_SUCCESS ||
      cuvsCagraSearchParamsCreate(&h->sp) != CUVS_SUCCESS) goto failed;
  /* Retain more final neighbors while reducing construction iterations. */
#if SK_CAGRA_BUILD_ALGO == 4
  h->ip->build_algo = ITERATIVE_CAGRA_SEARCH;
#elif SK_CAGRA_BUILD_ALGO == 3
  h->ip->build_algo = NN_DESCENT; /* custom exact graph path below */
#else
  h->ip->build_algo = (enum cuvsCagraGraphBuildAlgo)SK_CAGRA_BUILD_ALGO;
#endif
  h->ip->graph_degree = SK_CAGRA_GRAPH_DEGREE;
  h->ip->intermediate_graph_degree = SK_CAGRA_INTERMEDIATE_DEGREE;
  h->ip->nn_descent_niter = SK_CAGRA_NITER;
#if SK_CAGRA_BUILD_ALGO == 1
  if (h->ip->graph_build_params == NULL) {
    fprintf(stderr, "[seekdb_cuvs] IVF-PQ build params missing\n");
    goto failed;
  }
  if (cuvsIvfPqIndexParamsCreate(&h->pq_build) != CUVS_SUCCESS ||
      cuvsIvfPqSearchParamsCreate(&h->pq_search) != CUVS_SUCCESS) goto failed;
  if (SK_CAGRA_IVFPQ_LISTS) h->pq_build->n_lists = SK_CAGRA_IVFPQ_LISTS;
  if (SK_CAGRA_IVFPQ_KMEANS_ITERS) h->pq_build->kmeans_n_iters = SK_CAGRA_IVFPQ_KMEANS_ITERS;
  if (SK_CAGRA_IVFPQ_TRAINSET > 0.0) {
    h->pq_build->kmeans_trainset_fraction = SK_CAGRA_IVFPQ_TRAINSET;
  }
  h->pq_build->pq_bits = SK_CAGRA_IVFPQ_PQ_BITS;
  if (SK_CAGRA_IVFPQ_PQ_DIM) h->pq_build->pq_dim = SK_CAGRA_IVFPQ_PQ_DIM;
  if (SK_CAGRA_IVFPQ_PROBES) h->pq_search->n_probes = SK_CAGRA_IVFPQ_PROBES;
  h->ip->graph_build_params->ivf_pq_build_params = h->pq_build;
  h->ip->graph_build_params->ivf_pq_search_params = h->pq_search;
  if (SK_CAGRA_IVFPQ_REFINE > 1.0) {
    h->ip->graph_build_params->refinement_rate = SK_CAGRA_IVFPQ_REFINE;
  }
#endif
#if SK_CAGRA_BUILD_ALGO == 3
  if (sk_cuvs_build_from_knn(h, base, n, dim) != 0) goto failed;
#else
  if (cuvsCagraBuild(h->res, h->ip, &dataset, h->idx) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS) goto failed;
#endif
  h->n = n;
  h->dim = dim;
  h->default_itopk = h->sp->itopk_size;
  return h;
failed:
  fprintf(stderr, "[seekdb_cuvs] build failed: %s\n", cuvsGetLastErrorText());
  seekdb_cuvs_free(h);
  return NULL;
}

/* Allocate before replacing an existing allocation so a failed larger query
   leaves the handle usable. Scratch belongs to this index and never escapes. */
static int reserve_buffer(sk_cuvs_index *h, void **ptr, size_t *capacity, size_t bytes)
{
  if (*capacity >= bytes) return 0;
  void *replacement = NULL;
  if (cuvsRMMAlloc(h->res, &replacement, bytes) != CUVS_SUCCESS) return 1;
  if (*ptr && cuvsRMMFree(h->res, *ptr, *capacity) != CUVS_SUCCESS) {
    cuvsRMMFree(h->res, replacement, bytes);
    return 1;
  }
  *ptr = replacement;
  *capacity = bytes;
  return 0;
}

int seekdb_cuvs_search(void *handle, const float *query, long nq, long topk,
                     unsigned int *out_ids, float *out_dist)
{
  sk_cuvs_index *h = (sk_cuvs_index *)handle;
  size_t query_bytes = 0, result_bytes = 0;
  if (!h || !query || !out_ids || topk > h->n ||
      checked_bytes(nq, h->dim, &query_bytes) || checked_bytes(nq, topk, &result_bytes)) return 1;
  if (pthread_mutex_lock(&h->mutex)) return 2;
  int rc = 0;
  long query_shape[2] = {nq, h->dim}, result_shape[2] = {nq, topk};
  DLManagedTensor queries, neighbors, distances;
  cuvsFilter filter;
  if (reserve_buffer(h, (void **)&h->queries, &h->query_bytes, query_bytes) ||
      reserve_buffer(h, (void **)&h->neighbors, &h->neighbor_bytes, result_bytes) ||
      reserve_buffer(h, (void **)&h->distances, &h->distance_bytes, result_bytes)) { rc = 10; goto done; }
  if (cudaMemcpy(h->queries, query, query_bytes, cudaMemcpyDefault) != cudaSuccess) { rc = 13; goto done; }
  queries = tensor(h->queries, query_shape, kDLCUDA, kDLFloat);
  neighbors = tensor(h->neighbors, result_shape, kDLCUDA, kDLUInt);
  distances = tensor(h->distances, result_shape, kDLCUDA, kDLFloat);
  filter.type = NO_FILTER;
  filter.addr = (uintptr_t)0;
  /* Match V5 for every request, including a small query after a large one. */
  h->sp->itopk_size = (size_t)topk * 4;
  if (h->sp->itopk_size < 512) h->sp->itopk_size = 512;
  if (h->sp->itopk_size < h->default_itopk) h->sp->itopk_size = h->default_itopk;
  h->sp->search_width = 4;
  if (cuvsCagraSearch(h->res, h->sp, h->idx, &queries, &neighbors, &distances, filter) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS) { rc = 15; goto done; }
  if (cudaMemcpy(out_ids, h->neighbors, result_bytes, cudaMemcpyDefault) != cudaSuccess) { rc = 16; goto done; }
  if (out_dist && cudaMemcpy(out_dist, h->distances, result_bytes, cudaMemcpyDefault) != cudaSuccess) { rc = 17; goto done; }
done:
  if (rc) { cuvsStreamSync(h->res); fprintf(stderr, "[seekdb_cuvs] search rc=%d: %s\n", rc, cuvsGetLastErrorText()); }
  pthread_mutex_unlock(&h->mutex);
  return rc;
}

/* Persistence is part of the V5 ABI: reload must serve the saved GPU graph,
   not rebuild it or lose the original graph/search-quality configuration. */
int seekdb_cuvs_serialize(void *handle, const char *filename)
{
  sk_cuvs_index *h = (sk_cuvs_index *)handle;
  if (!h || !filename || !filename[0]) return 1;
  if (pthread_mutex_lock(&h->mutex)) return 2;
  int rc = 0;
  if (cuvsStreamSync(h->res) != CUVS_SUCCESS ||
      cuvsCagraSerialize(h->res, filename, h->idx, true) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS) {
    fprintf(stderr, "[seekdb_cuvs] serialize failed: %s\n", cuvsGetLastErrorText());
    rc = 1;
  }
  pthread_mutex_unlock(&h->mutex);
  return rc;
}

void *seekdb_cuvs_deserialize(const char *filename, long n, long dim)
{
  size_t bytes = 0;
  if (!filename || !filename[0] || n > UINT_MAX || checked_bytes(n, dim, &bytes)) return NULL;
  sk_cuvs_index *h = (sk_cuvs_index *)calloc(1, sizeof(*h));
  if (!h) return NULL;
  if (pthread_mutex_init(&h->mutex, NULL)) { free(h); return NULL; }
  int64_t actual_n = 0, actual_dim = 0;
  if (cuvsResourcesCreate(&h->res) != CUVS_SUCCESS ||
      cuvsCagraIndexParamsCreate(&h->ip) != CUVS_SUCCESS ||
      cuvsCagraIndexCreate(&h->idx) != CUVS_SUCCESS ||
      cuvsCagraSearchParamsCreate(&h->sp) != CUVS_SUCCESS ||
      cuvsCagraDeserialize(h->res, filename, h->idx) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS ||
      cuvsCagraIndexGetSize(h->idx, &actual_n) != CUVS_SUCCESS ||
      cuvsCagraIndexGetDims(h->idx, &actual_dim) != CUVS_SUCCESS) goto failed;
  if (actual_n != n || actual_dim != dim) {
    fprintf(stderr, "[seekdb_cuvs] snapshot shape mismatch: %ld x %ld, expected %ld x %ld\n",
            (long)actual_n, (long)actual_dim, n, dim);
    goto failed;
  }
  h->n = n;
  h->dim = dim;
  h->default_itopk = h->sp->itopk_size;
  return h;
failed:
  fprintf(stderr, "[seekdb_cuvs] deserialize failed: %s\n", cuvsGetLastErrorText());
  seekdb_cuvs_free(h);
  return NULL;
}

int seekdb_cuvs_cagra_knn(const float *base, long n, long dim,
                        const float *query, long nq, long topk, unsigned int *out_ids)
{
  void *h = seekdb_cuvs_build(base, n, dim);
  if (!h) return 10;
  int rc = seekdb_cuvs_search(h, query, nq, topk, out_ids, NULL);
  seekdb_cuvs_free(h);
  return rc;
}
