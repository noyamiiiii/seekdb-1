/* seekdb <-> hipVS C ABI. FP32 input and L2 output remain unchanged. */
#include "seekdb_cuvs_bridge.h"
#include <cuvs/core/c_api.h>
#include <cuvs/neighbors/cagra.h>
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

#ifndef SEEKDB_CUVS_BUILD_ALGO
#define SEEKDB_CUVS_BUILD_ALGO NN_DESCENT
#endif

typedef struct {
  cuvsResources_t res;
  cuvsCagraIndexParams_t ip;
  cuvsCagraIndex_t idx;
  cuvsCagraSearchParams_t sp;
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
  if (h->ip) cuvsCagraIndexParamsDestroy(h->ip);
  if (h->res) cuvsResourcesDestroy(h->res);
  pthread_mutex_destroy(&h->mutex);
  free(h);
}

void *seekdb_cuvs_build(const float *base, long n, long dim)
{
  size_t bytes = 0;
  if (!base || n > UINT_MAX || checked_bytes(n, dim, &bytes)) return NULL;
  sk_cuvs_index *h = (sk_cuvs_index *)calloc(1, sizeof(*h));
  if (!h) return NULL;
  if (pthread_mutex_init(&h->mutex, NULL)) { free(h); return NULL; }
  long shape[2] = {n, dim};
  DLManagedTensor dataset = tensor((void *)base, shape, kDLCPU, kDLFloat);
  if (cuvsResourcesCreate(&h->res) != CUVS_SUCCESS ||
      cuvsCagraIndexParamsCreate(&h->ip) != CUVS_SUCCESS ||
      cuvsCagraIndexCreate(&h->idx) != CUVS_SUCCESS ||
      cuvsCagraSearchParamsCreate(&h->sp) != CUVS_SUCCESS) goto failed;
  /* High-dimensional NN-descent avoids the IVF-PQ training pass. Preserve
     the library's default for low-dimensional workloads, where the SQL
     batch path regressed in end-to-end measurements. Graph degrees,
     iteration count, FP32 input and search quality parameters stay intact. */
  if (dim >= 128) h->ip->build_algo = SEEKDB_CUVS_BUILD_ALGO;
  if (cuvsCagraBuild(h->res, h->ip, &dataset, h->idx) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS) goto failed;
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
  /* CAGRA requires its internal candidate list to accommodate requested k.
     Keep the default for small k and grow to its supported warp multiple. */
  h->sp->itopk_size = h->default_itopk;
  if (h->sp->itopk_size < (size_t)topk) h->sp->itopk_size = ((size_t)topk + 31) / 32 * 32;
  if (cuvsCagraSearch(h->res, h->sp, h->idx, &queries, &neighbors, &distances, filter) != CUVS_SUCCESS ||
      cuvsStreamSync(h->res) != CUVS_SUCCESS) { rc = 15; goto done; }
  if (cudaMemcpy(out_ids, h->neighbors, result_bytes, cudaMemcpyDefault) != cudaSuccess) { rc = 16; goto done; }
  if (out_dist && cudaMemcpy(out_dist, h->distances, result_bytes, cudaMemcpyDefault) != cudaSuccess) { rc = 17; goto done; }
done:
  if (rc) { cuvsStreamSync(h->res); fprintf(stderr, "[seekdb_cuvs] search rc=%d: %s\n", rc, cuvsGetLastErrorText()); }
  pthread_mutex_unlock(&h->mutex);
  return rc;
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
