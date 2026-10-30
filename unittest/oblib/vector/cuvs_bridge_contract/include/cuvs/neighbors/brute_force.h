#pragma once
#include <cuvs/core/c_api.h>
#include <dlpack/dlpack.h>
#ifdef __cplusplus
extern "C" {
#endif
/* Local double for the exact-kNN build path; the real header lives in hipVS. */
typedef struct {uintptr_t addr;int bits;} cuvsBruteForceIndex;
typedef cuvsBruteForceIndex* cuvsBruteForceIndex_t;
cuvsError_t cuvsBruteForceIndexCreate(cuvsBruteForceIndex_t*);
cuvsError_t cuvsBruteForceIndexDestroy(cuvsBruteForceIndex_t);
cuvsError_t cuvsBruteForceBuild(cuvsResources_t,DLManagedTensor*,cuvsDistanceType,float,cuvsBruteForceIndex_t);
cuvsError_t cuvsBruteForceSearch(cuvsResources_t,cuvsBruteForceIndex_t,DLManagedTensor*,DLManagedTensor*,DLManagedTensor*,cuvsFilter);
#ifdef __cplusplus
}
#endif
