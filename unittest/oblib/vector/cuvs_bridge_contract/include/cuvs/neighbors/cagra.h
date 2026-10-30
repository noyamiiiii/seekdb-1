#pragma once
#include <cuvs/core/c_api.h>
#include <dlpack/dlpack.h>
#ifdef __cplusplus
extern "C" {
#endif
enum cuvsCagraGraphBuildAlgo {AUTO_SELECT=0,IVF_PQ=1,NN_DESCENT=2,ITERATIVE_CAGRA_SEARCH=3};
typedef struct {uint32_t n_lists,kmeans_n_iters,pq_bits,pq_dim;double kmeans_trainset_fraction;} *cuvsIvfPqIndexParams_t;
typedef struct {uint32_t n_probes;} *cuvsIvfPqSearchParams_t;
typedef struct {cuvsIvfPqIndexParams_t ivf_pq_build_params;cuvsIvfPqSearchParams_t ivf_pq_search_params;float refinement_rate;} *cuvsIvfPqParams_t;
typedef struct { int build_algo;size_t graph_degree,intermediate_graph_degree,nn_descent_niter;cuvsIvfPqParams_t graph_build_params; } *cuvsCagraIndexParams_t;
typedef struct FakeCagraIndex *cuvsCagraIndex_t;
typedef struct {size_t itopk_size,search_width;} *cuvsCagraSearchParams_t;
enum {NO_FILTER=0};typedef struct {int type;uintptr_t addr;} cuvsFilter;
cuvsError_t cuvsCagraIndexParamsCreate(cuvsCagraIndexParams_t*);
cuvsError_t cuvsCagraIndexParamsDestroy(cuvsCagraIndexParams_t);
cuvsError_t cuvsIvfPqIndexParamsCreate(cuvsIvfPqIndexParams_t*);
cuvsError_t cuvsIvfPqIndexParamsDestroy(cuvsIvfPqIndexParams_t);
cuvsError_t cuvsIvfPqSearchParamsCreate(cuvsIvfPqSearchParams_t*);
cuvsError_t cuvsIvfPqSearchParamsDestroy(cuvsIvfPqSearchParams_t);
cuvsError_t cuvsCagraIndexCreate(cuvsCagraIndex_t*);
cuvsError_t cuvsCagraIndexDestroy(cuvsCagraIndex_t);
cuvsError_t cuvsCagraSearchParamsCreate(cuvsCagraSearchParams_t*);
cuvsError_t cuvsCagraSearchParamsDestroy(cuvsCagraSearchParams_t);
cuvsError_t cuvsCagraBuild(cuvsResources_t,cuvsCagraIndexParams_t,DLManagedTensor*,cuvsCagraIndex_t);
cuvsError_t cuvsCagraSearch(cuvsResources_t,cuvsCagraSearchParams_t,cuvsCagraIndex_t,DLManagedTensor*,DLManagedTensor*,DLManagedTensor*,cuvsFilter);
cuvsError_t cuvsCagraSerialize(cuvsResources_t,const char*,cuvsCagraIndex_t,bool);
cuvsError_t cuvsCagraDeserialize(cuvsResources_t,const char*,cuvsCagraIndex_t);
cuvsError_t cuvsCagraIndexGetSize(cuvsCagraIndex_t,int64_t*);
cuvsError_t cuvsCagraIndexGetDims(cuvsCagraIndex_t,int64_t*);
#ifdef __cplusplus
}
#endif
